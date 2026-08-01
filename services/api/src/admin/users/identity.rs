//! Who the account *is*: its username, its email address, and whether that address is
//! confirmed.

use crate::audit::audit;
use crate::error::{ApiError, ApiResult};
use crate::openapi::ADMIN_USERS_TAG;
use crate::state::{AppState, AuthUser};
use axum::Json;
use axum::extract::{Path, State};
use serde::Deserialize;
use tankovault_domain::{Permission, UserId};
use utoipa::ToSchema;
use uuid::Uuid;

use super::detail::{UserDetailResponse, user_detail_response};

#[derive(Debug, Deserialize, ToSchema)]
pub struct AdminProfileUpdate {
    #[serde(default)]
    pub username: Option<String>,
    #[serde(default)]
    pub email: Option<String>,
}

/// Edit a user's identity
///
/// Change another account's username and/or email. Omitted fields are left alone.
///
/// Changing an email does **not** re-open verification: an administrator setting an address is
/// asserting it is correct, and forcing a confirmation round trip would lock the account out
/// until the user acted on mail they did not expect.
#[utoipa::path(
    patch,
    path = "/v1/admin/users/{id}",
    tag = ADMIN_USERS_TAG,
    params(("id" = Uuid, Path, description = "User id")),
    request_body = AdminProfileUpdate,
    security(("bearer_auth" = [])),
    responses(
        (status = 200, description = "The updated account", body = UserDetailResponse),
        (status = 401, description = "authentication required", body = crate::error::ProblemDetails),
        (status = 403, description = "caller does not hold the required permission", body = crate::error::ProblemDetails),
        (status = 404, description = "no such user", body = crate::error::ProblemDetails),
        (status = 409, description = "email or username already taken", body = crate::error::ProblemDetails),
    )
)]
pub async fn update_user(
    State(state): State<AppState>,
    user: AuthUser,
    Path(id): Path<Uuid>,
    Json(body): Json<AdminProfileUpdate>,
) -> ApiResult<Json<UserDetailResponse>> {
    user.require(Permission::UsersWrite).await?;
    let target = UserId::from_uuid(id);

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
    if username.is_none() && email.is_none() {
        return Err(ApiError::BadRequest("nothing to update".to_owned()));
    }

    // Same validators as registration and `PATCH /v1/me/profile`: without them here, an operator
    // could set a username containing `@`, making login ambiguous between username and email.
    if let Some(username) = username {
        crate::auth::validate_username(username)?;
    }
    if let Some(email) = email {
        crate::auth::validate_email(email)?;
    }

    tankovault_db::repo::user_admin::update_identity(&state.pool, target, username, email).await?;
    audit(
        &state,
        &user,
        "user.update",
        &id.to_string(),
        &serde_json::json!({ "username": username, "email": email }),
    )
    .await;

    user_detail_response(&state, target).await
}

/// Confirm a user's email address
///
/// The escape hatch for an account that never received its confirmation link. Without it, a
/// deployment whose outbound mail broke leaves those accounts permanently unable to sign in
/// with no self-service route back.
#[utoipa::path(
    post,
    path = "/v1/admin/users/{id}/verify-email",
    tag = ADMIN_USERS_TAG,
    params(("id" = Uuid, Path, description = "User id")),
    security(("bearer_auth" = [])),
    responses(
        (status = 200, description = "The updated account", body = UserDetailResponse),
        (status = 401, description = "authentication required", body = crate::error::ProblemDetails),
        (status = 403, description = "caller does not hold the required permission", body = crate::error::ProblemDetails),
        (status = 404, description = "no such user", body = crate::error::ProblemDetails),
    )
)]
pub async fn verify_user_email(
    State(state): State<AppState>,
    user: AuthUser,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<UserDetailResponse>> {
    user.require(Permission::UsersWrite).await?;
    let target = UserId::from_uuid(id);
    tankovault_db::repo::user_admin::force_verify_email(&state.pool, target).await?;
    audit(
        &state,
        &user,
        "user.verify_email",
        &id.to_string(),
        &serde_json::json!({}),
    )
    .await;
    user_detail_response(&state, target).await
}
