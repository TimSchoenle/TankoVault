//! Email confirmation: the link a new account must click, and resending it.

use axum::Json;
use axum::extract::State;
use axum::http::StatusCode;
use axum_extra::extract::cookie::CookieJar;
use serde::Deserialize;
use tankovault_auth::{generate_refresh_token, hash_refresh_token};
use tankovault_domain::User;
use time::{Duration, OffsetDateTime};
use utoipa::ToSchema;
use uuid::Uuid;

use super::login::TokenResponse;
use super::session::issue_session;
use crate::error::{ApiError, ApiResult};
use crate::mailer;
use crate::openapi::AUTH_TAG;
use crate::state::AppState;

/// How long an email-confirmation link stays valid. Longer than a reset link since a new
/// user may not check their inbox immediately, but still bounded so stale links expire.
const VERIFY_TOKEN_TTL: Duration = Duration::hours(24);

#[derive(Debug, Deserialize, ToSchema)]
pub struct VerifyEmailRequest {
    /// The opaque token from the emailed confirmation link.
    pub token: String,
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct ResendVerificationRequest {
    /// The account's email address. Unknown or already-confirmed addresses are accepted
    /// silently so the endpoint can't be used to probe which emails have accounts.
    pub email: String,
}

/// Confirm an email address with a token
///
/// Consumes a valid, unexpired, unused confirmation token, marks the address verified, and
/// signs the user in — issuing an access token and a rotating refresh cookie exactly like
/// [`login`] so clicking the link lands the user in the app.
#[utoipa::path(
    post,
    path = "/v1/auth/verify-email",
    tag = AUTH_TAG,
    request_body = VerifyEmailRequest,
    responses(
        (status = 200, description = "Email confirmed; session issued", body = TokenResponse),
        (status = 400, description = "Invalid or expired confirmation token", body = crate::error::ProblemDetails),
    )
)]
pub async fn verify_email(
    State(state): State<AppState>,
    jar: CookieJar,
    Json(req): Json<VerifyEmailRequest>,
) -> ApiResult<(CookieJar, Json<TokenResponse>)> {
    let token_hash = hash_refresh_token(&req.token);
    let record = tankovault_db::repo::users::find_email_verification(&state.pool, &token_hash)
        .await?
        .ok_or_else(|| ApiError::BadRequest("invalid or expired confirmation token".into()))?;
    if record.used_at.is_some() || record.expires_at <= OffsetDateTime::now_utc() {
        return Err(ApiError::BadRequest(
            "invalid or expired confirmation token".into(),
        ));
    }

    // Single-use guard: the atomic `used_at` flip closes the race between two concurrent
    // confirmations presenting the same token — the loser sees `0` rows and fails.
    let consumed =
        tankovault_db::repo::users::consume_email_verification(&state.pool, record.id).await?;
    if consumed == 0 {
        return Err(ApiError::BadRequest(
            "invalid or expired confirmation token".into(),
        ));
    }

    tankovault_db::repo::users::mark_email_verified(&state.pool, record.user_id).await?;
    let user = tankovault_db::repo::users::get(&state.pool, record.user_id).await?;
    // Now that the address is confirmed, send the welcome email that registration deferred.
    mailer::send_in_background(&state, mailer::welcome(&user.email, &user.username));
    issue_session(&state, jar, &user, Uuid::now_v7()).await
}

/// Resend the email-confirmation link
///
/// Always responds `202 Accepted`, whether or not the address is registered or already
/// confirmed, so the endpoint can't be used to probe which emails have accounts. A fresh
/// link is only sent when the address exists, is still unconfirmed, and email is configured.
#[utoipa::path(
    post,
    path = "/v1/auth/verify-email/resend",
    tag = AUTH_TAG,
    request_body = ResendVerificationRequest,
    responses(
        (status = 202, description = "If the address is registered and unconfirmed, a confirmation email has been sent"),
    )
)]
pub async fn resend_verification(
    State(state): State<AppState>,
    Json(req): Json<ResendVerificationRequest>,
) -> ApiResult<StatusCode> {
    let email = req.email.trim();
    if let Some((user, verified)) =
        tankovault_db::repo::users::find_by_email_with_verification(&state.pool, email).await?
    {
        if !verified && state.mailer.is_enabled() {
            send_verification_email(&state, &user).await?;
        }
    }
    // Uniform response regardless of whether the account exists or is already confirmed.
    Ok(StatusCode::ACCEPTED)
}

/// Issue and email a fresh single-use confirmation link for `user`. Reuses the high-entropy
/// opaque-token generator; only the SHA-256 hash is stored, and delivery is fire-and-forget.
pub(crate) async fn send_verification_email(state: &AppState, user: &User) -> ApiResult<()> {
    let raw = generate_refresh_token();
    let token_hash = hash_refresh_token(&raw);
    let expires_at = OffsetDateTime::now_utc() + VERIFY_TOKEN_TTL;
    tankovault_db::repo::users::insert_email_verification(
        &state.pool,
        user.id,
        &token_hash,
        expires_at,
    )
    .await?;
    let link = format!(
        "{}/verify-email?token={raw}",
        state.email_base_url.trim_end_matches('/'),
    );
    mailer::send_in_background(
        state,
        mailer::verification(&user.email, &user.username, &link),
    );
    Ok(())
}
