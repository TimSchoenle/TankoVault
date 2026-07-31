//! The single-account read model, and the one handler that only reads it.
//!
//! [`UserDetailResponse`] is not merely `GET /v1/admin/users/{id}`'s answer: it is what *every*
//! mutating handler in this module returns, because each answers with the re-read state rather
//! than echoing its own input, so the client always renders what the database now holds. That
//! makes the response builder shared infrastructure rather than one handler's, which is why it
//! sits here instead of alongside the read handler that happens to be simplest.

use crate::error::ApiResult;
use crate::openapi::ADMIN_USERS_TAG;
use crate::state::{AppState, AuthUser};
use crate::views::IntoView;
use axum::Json;
use axum::extract::{Path, State};
use serde::Serialize;
use tankovault_contracts::admin::{GrantView, UserDetailView};
use tankovault_domain::{Permission, UserId};
use utoipa::ToSchema;
use uuid::Uuid;

/// One account's administrative detail, together with its grants.
#[derive(Debug, Serialize, ToSchema)]
pub struct UserDetailResponse {
    pub user: UserDetailView,
    /// The account's permission grants, with provenance.
    pub permissions: Vec<GrantView>,
}

/// Read one account's detail panel: the account row plus its grants.
///
/// Every mutating handler in this module answers with the *re-read* state rather than echoing
/// its own input, so the client always renders what the database now holds. That made the same
/// two queries appear five times; they live here instead.
pub(crate) async fn user_detail_response(
    state: &AppState,
    target: UserId,
) -> ApiResult<Json<UserDetailResponse>> {
    let user = tankovault_db::repo::user_admin::detail(&state.pool, target).await?;
    let permissions = tankovault_db::repo::permissions::list_for_user(&state.pool, target).await?;
    Ok(Json(UserDetailResponse {
        user: user.into_view(),
        permissions: permissions.into_view(),
    }))
}

/// Get a user
///
/// Everything the user-detail panel shows: identity, account state, activity counts, and the
/// permissions currently granted with who granted each and when.
#[utoipa::path(
    get,
    path = "/v1/admin/users/{id}",
    tag = ADMIN_USERS_TAG,
    params(("id" = Uuid, Path, description = "User id")),
    security(("bearer_auth" = [])),
    responses(
        (status = 200, description = "The account and its grants", body = UserDetailResponse),
        (status = 401, description = "authentication required", body = crate::error::ProblemDetails),
        (status = 403, description = "caller does not hold the required permission", body = crate::error::ProblemDetails),
        (status = 404, description = "no such user", body = crate::error::ProblemDetails),
    )
)]
pub async fn get_user(
    State(state): State<AppState>,
    user: AuthUser,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<UserDetailResponse>> {
    user.require(Permission::UsersRead).await?;
    let target = UserId::from_uuid(id);
    user_detail_response(&state, target).await
}
