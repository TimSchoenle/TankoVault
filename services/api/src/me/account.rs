//! Account settings: profile, sessions, notification preferences.

use crate::error::{ApiError, ApiResult};
use crate::openapi::{ME_ACCOUNT_TAG, ME_NOTIFICATIONS_TAG};
use crate::state::{AppState, AuthUser};
use axum::Json;
use axum::extract::{Path, State};
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;
use utoipa::ToSchema;
use uuid::Uuid;

// ---------------------------------------------------------------------------
// Account settings (frontend §9.4)
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize, ToSchema)]
pub struct ProfileUpdate {
    #[serde(default)]
    pub username: Option<String>,
    #[serde(default)]
    pub email: Option<String>,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct ProfileDto {
    pub id: uuid::Uuid,
    pub email: String,
    pub username: String,
    pub role: String,
}

/// Update the profile
///
/// Update the caller's username and/or email (frontend §9.4). A duplicate email/username
/// surfaces as `409 Conflict`.
#[utoipa::path(
    patch,
    path = "/v1/me/profile",
    tag = ME_ACCOUNT_TAG,
    request_body = ProfileUpdate,
    security(("bearer_auth" = [])),
    responses(
        (status = 200, description = "Updated profile", body = ProfileDto),
        (status = 401, description = "authentication required", body = crate::error::ProblemDetails),
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
    let updated =
        tankovault_db::repo::users::update_profile(&state.pool, user.user_id, username, email)
            .await?;
    Ok(Json(ProfileDto {
        id: updated.id.as_uuid(),
        email: updated.email,
        username: updated.username,
        role: updated.role.as_str().to_owned(),
    }))
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
/// The caller's notification preferences JSON (frontend §9.4). `{}` means "product defaults".
#[utoipa::path(
    get,
    path = "/v1/me/notification-prefs",
    tag = ME_NOTIFICATIONS_TAG,
    security(("bearer_auth" = [])),
    responses(
        (status = 200, description = "Product-defined free-form preferences JSON", body = serde_json::Value),
        (status = 401, description = "authentication required", body = crate::error::ProblemDetails),
    )
)]
pub async fn notification_prefs(
    State(state): State<AppState>,
    user: AuthUser,
) -> ApiResult<Json<serde_json::Value>> {
    Ok(Json(
        tankovault_db::repo::users::get_notification_prefs(&state.pool, user.user_id).await?,
    ))
}

/// Replace notification preferences
///
/// Replace the caller's notification preferences (frontend §9.4). The body is stored verbatim
/// as an open JSON document.
#[utoipa::path(
    put,
    path = "/v1/me/notification-prefs",
    tag = ME_NOTIFICATIONS_TAG,
    request_body = serde_json::Value,
    security(("bearer_auth" = [])),
    responses(
        (status = 200, description = "The stored preferences, echoed back", body = serde_json::Value),
        (status = 401, description = "authentication required", body = crate::error::ProblemDetails),
    )
)]
pub async fn put_notification_prefs(
    State(state): State<AppState>,
    user: AuthUser,
    Json(body): Json<serde_json::Value>,
) -> ApiResult<Json<serde_json::Value>> {
    tankovault_db::repo::users::set_notification_prefs(&state.pool, user.user_id, &body).await?;
    Ok(Json(body))
}
