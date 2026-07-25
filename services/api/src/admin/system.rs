//! Read-only operator dashboard surfaces: system overview, user list, audit trail.

use crate::error::ApiResult;
use crate::openapi::{ADMIN_OVERVIEW_TAG, ADMIN_USERS_TAG};
use crate::state::{AppState, AuthUser};
use axum::Json;
use axum::extract::State;
use tankovault_domain::UserRole;

/// List users
///
/// The operator Users directory: identity, role, and tracked-series count per user (frontend
/// §9.5 Users tab).
#[utoipa::path(
    get,
    path = "/v1/admin/users",
    tag = ADMIN_USERS_TAG,
    security(("bearer_auth" = [])),
    responses(
        (status = 200, description = "Up to 200 most relevant users", body = Vec<tankovault_db::repo::users::UserRow2>),
        (status = 401, description = "authentication required", body = crate::error::ProblemDetails),
        (status = 403, description = "caller must have at least the operator role", body = crate::error::ProblemDetails),
    )
)]
pub async fn list_users(
    State(state): State<AppState>,
    user: AuthUser,
) -> ApiResult<Json<Vec<tankovault_db::repo::users::UserRow2>>> {
    user.require(UserRole::Operator).await?;
    Ok(Json(
        tankovault_db::repo::users::list_users(&state.pool, 200).await?,
    ))
}

/// Get system stats
///
/// System-wide rollup for the console header.
#[utoipa::path(
    get,
    path = "/v1/admin/stats",
    tag = ADMIN_OVERVIEW_TAG,
    security(("bearer_auth" = [])),
    responses(
        (status = 200, description = "System-wide stats", body = tankovault_db::repo::stats::SystemStats),
        (status = 401, description = "authentication required", body = crate::error::ProblemDetails),
        (status = 403, description = "caller must have at least the operator role", body = crate::error::ProblemDetails),
    )
)]
pub async fn system_stats(
    State(state): State<AppState>,
    user: AuthUser,
) -> ApiResult<Json<tankovault_db::repo::stats::SystemStats>> {
    user.require(UserRole::Operator).await?;
    Ok(Json(
        tankovault_db::repo::stats::system_overview(&state.pool).await?,
    ))
}

/// Get the audit log
///
/// The most recent privileged actions (design §16 audit trail).
#[utoipa::path(
    get,
    path = "/v1/admin/audit",
    tag = ADMIN_OVERVIEW_TAG,
    security(("bearer_auth" = [])),
    responses(
        (status = 200, description = "Up to 40 most recent audit records", body = Vec<tankovault_db::repo::audit::AuditView>),
        (status = 401, description = "authentication required", body = crate::error::ProblemDetails),
        (status = 403, description = "caller must have at least the operator role", body = crate::error::ProblemDetails),
    )
)]
pub async fn audit_log(
    State(state): State<AppState>,
    user: AuthUser,
) -> ApiResult<Json<Vec<tankovault_db::repo::audit::AuditView>>> {
    user.require(UserRole::Operator).await?;
    Ok(Json(
        tankovault_db::repo::audit::list_recent(&state.pool, 40).await?,
    ))
}
