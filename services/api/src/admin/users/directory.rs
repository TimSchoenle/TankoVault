//! The operator user directory: the one read that answers "who is in this deployment".

use crate::error::ApiResult;
use crate::openapi::ADMIN_USERS_TAG;
use crate::state::{AppState, AuthUser};
use crate::views::IntoView;
use axum::Json;
use axum::extract::{Query, State};
use serde::Deserialize;
use tankovault_contracts::admin::UserDirectoryPage;
use tankovault_domain::Permission;
use utoipa::IntoParams;

/// Directory paging. Deliberately capped server-side — the console offers 25/50/100 and a
/// caller asking for everything would page the whole user table into one response.
const MAX_PAGE: i64 = 200;

#[derive(Debug, Deserialize, IntoParams)]
pub struct DirectoryQuery {
    /// Case-insensitive substring match on username or email. Empty lists everyone.
    #[serde(default)]
    pub search: Option<String>,
    #[serde(default)]
    pub limit: Option<i64>,
    #[serde(default)]
    pub offset: Option<i64>,
}

/// List users
///
/// The operator user directory: identity, account state, permission and tracking counts, and
/// last sign-in, with search and paging.
#[utoipa::path(
    get,
    path = "/v1/admin/users",
    tag = ADMIN_USERS_TAG,
    params(DirectoryQuery),
    security(("bearer_auth" = [])),
    responses(
        (status = 200, description = "A page of the user directory", body = UserDirectoryPage),
        (status = 401, description = "authentication required", body = crate::error::ProblemDetails),
        (status = 403, description = "caller does not hold the required permission", body = crate::error::ProblemDetails),
    )
)]
pub async fn list_users(
    State(state): State<AppState>,
    user: AuthUser,
    Query(q): Query<DirectoryQuery>,
) -> ApiResult<Json<UserDirectoryPage>> {
    user.require(Permission::UsersRead).await?;
    let search = q.search.unwrap_or_default();
    let limit = q.limit.unwrap_or(50).clamp(1, MAX_PAGE);
    let offset = q.offset.unwrap_or(0).max(0);
    Ok(Json(
        tankovault_db::repo::user_admin::directory(&state.pool, search.trim(), limit, offset)
            .await?
            .into_view(),
    ))
}
