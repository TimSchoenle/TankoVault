//! Account settings: profile, sessions, notification preferences.

use crate::error::{ApiError, ApiResult};
use crate::openapi::{ME_ACCOUNT_TAG, ME_NOTIFICATIONS_TAG};
use crate::state::{AppState, AuthUser};
use axum::Json;
use axum::extract::{Path, State};
use secrecy::SecretString;
use serde::{Deserialize, Serialize};
use tankovault_domain::NotificationPrefs;
use time::OffsetDateTime;
use utoipa::ToSchema;
use uuid::Uuid;

#[derive(Debug, Deserialize, ToSchema)]
pub struct ProfileUpdate {
    #[serde(default)]
    pub username: Option<String>,
    #[serde(default)]
    pub email: Option<String>,
    /// Required when `email` changes the address on the account. See [`patch_profile`].
    #[schema(value_type = Option<String>)]
    #[serde(default)]
    pub current_password: Option<SecretString>,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct ProfileDto {
    pub id: uuid::Uuid,
    pub email: String,
    pub username: String,
    /// Whether the account is active or suspended. A suspended caller cannot reach this
    /// endpoint at all, so in practice this is always `active` — it is here because the
    /// profile is the account's identity record and omitting its state would make the DTO an
    /// incomplete picture of it.
    pub status: tankovault_domain::AccountStatus,
}

/// Update the profile
///
/// Update the caller's username and/or email (frontend §9.4). A duplicate email/username
/// surfaces as `409 Conflict`.
///
/// Changing the **email address** additionally requires `current_password`, because the
/// address is the account's recovery channel. Without that check, anyone holding an access
/// token for 15 minutes — a shared browser, a proxy log, a leaked SSE URL — could point the
/// account at their own address, request a password reset to it, and take the account over;
/// `reset_password` would then revoke the real owner's sessions on the attacker's behalf.
///
/// On a successful change the new address starts **unverified**
/// (`repo::users::update_profile` clears `email_verified_at`), a confirmation link is sent to
/// it, a warning is sent to the *old* address, and every session is revoked — matching what
/// `reset_password` already does for the other credential.
#[utoipa::path(
    patch,
    path = "/v1/me/profile",
    tag = ME_ACCOUNT_TAG,
    request_body = ProfileUpdate,
    security(("bearer_auth" = [])),
    responses(
        (status = 200, description = "Updated profile", body = ProfileDto),
        (status = 400, description = "Invalid username or email", body = crate::error::ProblemDetails),
        (status = 401, description = "authentication required, or current_password is wrong", body = crate::error::ProblemDetails),
        (status = 409, description = "Email or username already taken", body = crate::error::ProblemDetails),
    )
)]
pub async fn patch_profile(
    State(state): State<AppState>,
    user: AuthUser,
    Json(body): Json<ProfileUpdate>,
) -> ApiResult<Json<ProfileDto>> {
    let username = body
        .username
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty());
    let email = body
        .email
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty());

    if let Some(username) = username {
        crate::auth::validate_username(username)?;
    }
    if let Some(email) = email {
        crate::auth::validate_email(email)?;
    }

    let before = tankovault_db::repo::users::get(&state.pool, user.user_id).await?;
    // `citext` is case-insensitive; match that so a case-only change isn't treated as an address change.
    let email_changing = email.is_some_and(|e| !e.eq_ignore_ascii_case(&before.email));

    if email_changing {
        let current = body.current_password.as_ref().ok_or_else(|| {
            ApiError::BadRequest("current_password is required to change the email address".into())
        })?;
        let credentials =
            tankovault_db::repo::users::find_credentials(&state.pool, &before.username)
                .await?
                .ok_or(ApiError::Unauthorized)?;
        let ok = tankovault_auth::verify_password(
            current,
            &credentials.password_hash,
            &state.password_pepper,
        )
        .map_err(|_| ApiError::Internal)?;
        if !ok {
            return Err(ApiError::Unauthorized);
        }
    }

    let updated =
        tankovault_db::repo::users::update_profile(&state.pool, user.user_id, username, email)
            .await?;

    if email_changing {
        // Only the old address can warn the legitimate owner this happened.
        crate::mailer::send_in_background(
            &state,
            crate::mailer::email_changed(&before.email, &updated.username, &updated.email),
        );
        crate::auth::send_verification_email(&state, &updated).await?;
        // The credential the address protects has changed hands: same rule as a password reset, every session dies.
        tankovault_db::repo::users::revoke_all_for_user(&state.pool, user.user_id).await?;
    }

    Ok(Json(ProfileDto {
        id: updated.id.as_uuid(),
        email: updated.email,
        username: updated.username,
        status: updated.status,
    }))
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct PasswordChange {
    #[schema(value_type = String)]
    pub current_password: SecretString,
    #[schema(value_type = String)]
    pub new_password: SecretString,
}

/// Change the password
///
/// Change the caller's password, proving knowledge of the current one.
///
/// There was previously **no authenticated path to a new password at all** — the only route
/// was the emailed reset link, so a signed-in user who simply wanted to rotate their password
/// had to go through an out-of-band channel, and a user whose email had been taken over could
/// not lock the attacker out.
///
/// Every session is revoked on success, including the caller's: a password change is exactly
/// when you want the other device signed out, and leaving the caller's own session alive would
/// mean special-casing the one session an attacker is most likely to be holding.
#[utoipa::path(
    post,
    path = "/v1/me/password",
    tag = ME_ACCOUNT_TAG,
    request_body = PasswordChange,
    security(("bearer_auth" = [])),
    responses(
        (status = 204, description = "Password changed; every session was revoked"),
        (status = 400, description = "New password fails the policy", body = crate::error::ProblemDetails),
        (status = 401, description = "authentication required, or current_password is wrong", body = crate::error::ProblemDetails),
    )
)]
pub async fn change_password(
    State(state): State<AppState>,
    user: AuthUser,
    Json(body): Json<PasswordChange>,
) -> ApiResult<axum::http::StatusCode> {
    crate::auth::validate_password(&body.new_password)?;

    let current = tankovault_db::repo::users::get(&state.pool, user.user_id).await?;
    let credentials = tankovault_db::repo::users::find_credentials(&state.pool, &current.username)
        .await?
        .ok_or(ApiError::Unauthorized)?;
    let ok = tankovault_auth::verify_password(
        &body.current_password,
        &credentials.password_hash,
        &state.password_pepper,
    )
    .map_err(|_| ApiError::Internal)?;
    if !ok {
        return Err(ApiError::Unauthorized);
    }

    let hash = tankovault_auth::hash_password(&body.new_password, &state.password_pepper)
        .map_err(|_| ApiError::Internal)?;
    tankovault_db::repo::users::update_password(&state.pool, user.user_id, &hash).await?;
    tankovault_db::repo::users::revoke_all_for_user(&state.pool, user.user_id).await?;

    Ok(axum::http::StatusCode::NO_CONTENT)
}

#[derive(Debug, Serialize, ToSchema)]
pub struct SessionDto {
    pub id: String,
    pub family_id: String,
    #[serde(with = "time::serde::rfc3339")]
    #[schema(value_type = String)]
    pub created_at: OffsetDateTime,
    #[serde(with = "time::serde::rfc3339")]
    #[schema(value_type = String)]
    pub expires_at: OffsetDateTime,
}

/// List active sessions
///
/// The caller's active login sessions (frontend §9.4).
#[utoipa::path(
    get,
    path = "/v1/me/sessions",
    tag = ME_ACCOUNT_TAG,
    security(("bearer_auth" = [])),
    responses(
        (status = 200, description = "Active sessions", body = Vec<SessionDto>),
        (status = 401, description = "authentication required", body = crate::error::ProblemDetails),
    )
)]
pub async fn sessions(
    State(state): State<AppState>,
    user: AuthUser,
) -> ApiResult<Json<Vec<SessionDto>>> {
    let list = tankovault_db::repo::users::list_sessions(&state.pool, user.user_id).await?;
    let out = list
        .into_iter()
        .map(|s| SessionDto {
            id: s.id.to_string(),
            family_id: s.family_id.to_string(),
            created_at: s.created_at,
            expires_at: s.expires_at,
        })
        .collect();
    Ok(Json(out))
}

/// Revoke a session
///
/// Revoke one of the caller's own sessions (frontend §9.4). Scoped to ownership; a
/// foreign/unknown id yields `404`.
#[utoipa::path(
    delete,
    path = "/v1/me/sessions/{id}",
    tag = ME_ACCOUNT_TAG,
    params(("id" = uuid::Uuid, Path, description = "Session id")),
    security(("bearer_auth" = [])),
    responses(
        (status = 200, description = "Revoked", body = serde_json::Value, example = json!({"revoked": 1})),
        (status = 401, description = "authentication required", body = crate::error::ProblemDetails),
        (status = 404, description = "No such session for this caller", body = crate::error::ProblemDetails),
    )
)]
pub async fn delete_session(
    State(state): State<AppState>,
    user: AuthUser,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<serde_json::Value>> {
    let revoked = tankovault_db::repo::users::revoke_session(&state.pool, user.user_id, id).await?;
    if revoked == 0 {
        return Err(ApiError::NotFound);
    }
    Ok(Json(serde_json::json!({ "revoked": revoked })))
}

/// Get notification preferences
///
/// The caller's effective notification preferences. A reader who has never saved any gets the
/// product defaults, fully populated, rather than an empty object the client would have to know
/// how to fill in.
#[utoipa::path(
    get,
    path = "/v1/me/notification-prefs",
    tag = ME_NOTIFICATIONS_TAG,
    security(("bearer_auth" = [])),
    responses(
        (status = 200, description = "The caller's effective preferences", body = NotificationPrefs),
        (status = 401, description = "authentication required", body = crate::error::ProblemDetails),
    )
)]
pub async fn notification_prefs(
    State(state): State<AppState>,
    user: AuthUser,
) -> ApiResult<Json<NotificationPrefs>> {
    Ok(Json(
        tankovault_db::repo::users::get_notification_prefs(&state.pool, user.user_id).await?,
    ))
}

/// Replace notification preferences
///
/// Replaces the caller's preferences wholesale. Every field defaults, so a partial body is a
/// valid document — the omitted fields come back to the product defaults, not to `false`.
///
/// The stored document used to be free-form and unvalidated, and nothing read it: the three
/// toggles it held had no effect on delivery at all. It is typed now precisely so that a
/// preference which is saved is a preference the notifier honours.
#[utoipa::path(
    put,
    path = "/v1/me/notification-prefs",
    tag = ME_NOTIFICATIONS_TAG,
    request_body = NotificationPrefs,
    security(("bearer_auth" = [])),
    responses(
        (status = 200, description = "The stored preferences, echoed back", body = NotificationPrefs),
        (status = 400, description = "the document is out of range or from a newer schema", body = crate::error::ProblemDetails),
        (status = 401, description = "authentication required", body = crate::error::ProblemDetails),
    )
)]
pub async fn put_notification_prefs(
    State(state): State<AppState>,
    user: AuthUser,
    Json(body): Json<NotificationPrefs>,
) -> ApiResult<Json<NotificationPrefs>> {
    body.validate()
        .map_err(|e| ApiError::BadRequest(e.to_string()))?;
    tankovault_db::repo::users::set_notification_prefs(&state.pool, user.user_id, &body).await?;
    Ok(Json(body))
}
