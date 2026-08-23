//! Email confirmation: the link a new account must click, and resending it.

use axum::Json;
use axum::extract::State;
use axum::http::StatusCode;
use axum_extra::extract::cookie::CookieJar;
use secrecy::{ExposeSecret as _, SecretString};
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
    // A bearer credential, wrapped like every other one.
    #[schema(value_type = String)]
    pub token: SecretString,
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

    // Single-use guard: atomic `used_at` flip closes the race; the loser sees 0 rows.
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
    mailer::send_in_background(
        &state,
        mailer::welcome(state.branding.name(), &user.email, &user.username),
    );
    issue_session(&state, jar, &user, Uuid::now_v7()).await
}

/// Resend the email-confirmation link
///
/// Always responds `202 Accepted`, whether or not the address is registered or already
/// confirmed, so the endpoint can't be used to probe which emails have accounts. A fresh
/// link is only sent when the address exists, is still unconfirmed, and email is configured.
///
/// Spawned in full, for the reason `auth::password::forgot_password` explains at length
/// (SEC-10). This endpoint's channel was in fact the wider of the two: the known-and-unconfirmed
/// branch performed the token `INSERT`, while both "no such address" *and* "already confirmed"
/// returned straight after the lookup — so the timing separated three states rather than two,
/// and "this address exists and has not been confirmed" is the more useful answer of the pair.
/// The audit named `forgot_password`; this is the same defect in the sibling handler.
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
) -> StatusCode {
    let email = req.email.trim().to_owned();
    // Detached, but not out of the trace; see `tankovault_service::in_current_trace`.
    tokio::spawn(tankovault_service::in_current_trace(async move {
        deliver_verification_resend(state, email).await;
    }));
    StatusCode::ACCEPTED
}

/// Send a fresh confirmation link for `email`, if it belongs to an unconfirmed account.
///
/// Detached, so nothing here reaches the caller; a failure is logged and dropped.
async fn deliver_verification_resend(state: AppState, email: String) {
    if !state.mailer.is_enabled() {
        return;
    }
    let found =
        tankovault_db::repo::users::find_by_email_with_verification(&state.pool, &email).await;
    let user = match found {
        Ok(Some((user, false))) => user,
        // No such address, or already confirmed — indistinguishable from here.
        Ok(_) => return,
        Err(e) => {
            tracing::warn!(error = %e, "confirmation-resend lookup failed");
            return;
        }
    };

    let link = match issue_verification_link(&state, &user).await {
        Ok(link) => link,
        Err(e) => {
            tracing::warn!(error = %e, "failed to store a confirmation token");
            return;
        }
    };
    if let Err(e) = state
        .mailer
        .send(mailer::verification(
            state.branding.name(),
            &user.email,
            &user.username,
            &link,
        ))
        .await
    {
        tracing::warn!(error = %e, "failed to resend the confirmation email");
    }
}

/// Issue and email a fresh single-use confirmation link for `user`, off the request path.
///
/// For callers where a slow relay must not show up in the response: [`super::register`] and
/// the address change in `me::account`.
pub(crate) async fn send_verification_email(state: &AppState, user: &User) -> ApiResult<()> {
    let link = issue_verification_link(state, user).await?;
    mailer::send_in_background(
        state,
        mailer::verification(state.branding.name(), &user.email, &user.username, &link),
    );
    Ok(())
}

/// Mint a confirmation token for `user`, store its hash, and return the link to send.
///
/// Split from [`send_verification_email`] so the delivery decision belongs to the caller —
/// the resend path is already detached and awaits the send itself.
///
/// Reuses the high-entropy opaque-token generator; only the SHA-256 hash is stored.
async fn issue_verification_link(state: &AppState, user: &User) -> ApiResult<String> {
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
    // Deliberate unwrapping: the token has to be in the emailed link for the link to work.
    // The link itself is never logged.
    Ok(format!(
        "{}/verify-email?token={}",
        state.email_base_url.trim_end_matches('/'),
        raw.expose_secret(),
    ))
}
