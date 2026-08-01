//! The password-reset flow: request a link, then consume it.
//!
//! Both endpoints answer identically whether or not the address is registered, so neither can
//! be used to probe which emails have accounts.

use axum::Json;
use axum::extract::State;
use axum::http::StatusCode;
use secrecy::{ExposeSecret as _, SecretString};
use serde::Deserialize;
use tankovault_auth::{generate_refresh_token, hash_password, hash_refresh_token};
use time::{Duration, OffsetDateTime};
use utoipa::ToSchema;

use super::validate::validate_password;
use crate::error::{ApiError, ApiResult};
use crate::mailer;
use crate::openapi::AUTH_TAG;
use crate::state::AppState;

/// How long a password-reset link stays valid. Short by design: long enough to arrive by
/// email and be clicked, short enough to limit the blast radius of a leaked inbox.
const RESET_TOKEN_TTL: Duration = Duration::hours(1);

#[derive(Debug, Deserialize, ToSchema)]
pub struct ForgotPasswordRequest {
    /// The account's email address. Unknown addresses are accepted silently.
    pub email: String,
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct ResetPasswordRequest {
    /// The opaque token from the emailed reset link.
    // A bearer credential, wrapped the same as the password beside it.
    #[schema(value_type = String)]
    pub token: SecretString,
    /// The new password (same policy as registration: at least 8 characters).
    #[schema(value_type = String)]
    pub new_password: SecretString,
}

/// Request a password-reset email
///
/// Always responds `202 Accepted`, whether or not the address is registered, so the
/// endpoint can't be used to probe which emails have accounts. When the address does exist
/// and email is configured, a single-use, time-limited reset link is sent.
///
/// **Every** account-dependent step happens on a detached task (SEC-10). The uniform `202`
/// was previously defeated by timing: the known-address branch generated a token, hashed it
/// and performed an `INSERT` — a write, so a WAL flush — before answering, while the unknown
/// branch returned straight after the lookup. That is a smaller gap than `login`'s two orders
/// of magnitude but it is the same oracle, and it needs no credentials at all to read. With
/// the whole body spawned, this handler's only statement is the spawn, so there is no branch
/// left for a response time to disclose.
///
/// The return type is deliberately `StatusCode` rather than `ApiResult<StatusCode>`: the
/// anti-enumeration property is "this endpoint has exactly one answer", and making that a
/// type rather than a convention means a later edit cannot reintroduce a second one.
#[utoipa::path(
    post,
    path = "/v1/auth/password/forgot",
    tag = AUTH_TAG,
    request_body = ForgotPasswordRequest,
    responses(
        (status = 202, description = "If the address is registered, a reset email has been sent"),
    )
)]
pub async fn forgot_password(
    State(state): State<AppState>,
    Json(req): Json<ForgotPasswordRequest>,
) -> StatusCode {
    let email = req.email.trim().to_owned();
    tokio::spawn(async move { deliver_reset_link(state, email).await });
    StatusCode::ACCEPTED
}

/// Mint, store and send a reset link for `email`, if it belongs to an account.
///
/// Runs detached: a failure here is logged and dropped, same as a success looks to the
/// caller. Awaited rather than backgrounded again — this task already is the background.
async fn deliver_reset_link(state: AppState, email: String) {
    let found = tankovault_db::repo::users::find_by_email(&state.pool, &email).await;
    let user = match found {
        Ok(Some(user)) => user,
        Ok(None) => return,
        Err(e) => {
            tracing::warn!(error = %e, "password-reset lookup failed");
            return;
        }
    };

    // Reuse the high-entropy opaque-token generator; only the SHA-256 hash is stored.
    let raw = generate_refresh_token();
    let token_hash = hash_refresh_token(&raw);
    let expires_at = OffsetDateTime::now_utc() + RESET_TOKEN_TTL;
    if let Err(e) = tankovault_db::repo::users::insert_password_reset(
        &state.pool,
        user.id,
        &token_hash,
        expires_at,
    )
    .await
    {
        tracing::warn!(error = %e, "failed to store a password-reset token");
        return;
    }

    if !state.mailer.is_enabled() {
        return;
    }
    // Deliberate unwrapping — the token must appear in the link for it to work. Never logged;
    // the failure branch below records only the error.
    let link = format!(
        "{}/reset-password?token={}",
        state.email_base_url.trim_end_matches('/'),
        raw.expose_secret(),
    );
    if let Err(e) = state
        .mailer
        .send(mailer::password_reset(&user.email, &link))
        .await
    {
        tracing::warn!(error = %e, "failed to send the password-reset email");
    }
}

/// Reset a password with a token
///
/// Consumes a valid, unexpired, unused reset token, sets the new password, and revokes all
/// of the user's active sessions so any stolen refresh token dies with the old credential.
#[utoipa::path(
    post,
    path = "/v1/auth/password/reset",
    tag = AUTH_TAG,
    request_body = ResetPasswordRequest,
    responses(
        (status = 200, description = "Password changed; existing sessions revoked"),
        (status = 400, description = "Invalid/expired token or weak password", body = crate::error::ProblemDetails),
    )
)]
pub async fn reset_password(
    State(state): State<AppState>,
    Json(req): Json<ResetPasswordRequest>,
) -> ApiResult<StatusCode> {
    validate_password(&req.new_password)?;

    let token_hash = hash_refresh_token(&req.token);
    let record = tankovault_db::repo::users::find_password_reset(&state.pool, &token_hash)
        .await?
        .ok_or_else(|| ApiError::BadRequest("invalid or expired reset token".into()))?;
    if record.used_at.is_some() || record.expires_at <= OffsetDateTime::now_utc() {
        return Err(ApiError::BadRequest(
            "invalid or expired reset token".into(),
        ));
    }

    // Atomic `used_at` flip closes the race between concurrent resets; the loser sees 0 rows.
    let consumed =
        tankovault_db::repo::users::consume_password_reset(&state.pool, record.id).await?;
    if consumed == 0 {
        return Err(ApiError::BadRequest(
            "invalid or expired reset token".into(),
        ));
    }

    let hash =
        hash_password(&req.new_password, &state.password_pepper).map_err(|_| ApiError::Internal)?;
    tankovault_db::repo::users::update_password(&state.pool, record.user_id, &hash).await?;
    tankovault_db::repo::users::revoke_all_for_user(&state.pool, record.user_id).await?;
    Ok(StatusCode::OK)
}
