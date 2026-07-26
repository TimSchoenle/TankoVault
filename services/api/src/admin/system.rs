//! Read-only operator dashboard surfaces: the system overview and the audit trail.
//!
//! User administration used to live here alongside them. It now has its own module
//! ([`crate::admin::users`]) because it grew from one read into a mutating surface with its own
//! safety rules, and mixing "count the series" with "erase an account" in one file made the
//! second harder to review than it deserves.

use crate::error::ApiResult;
use crate::openapi::ADMIN_OVERVIEW_TAG;
use crate::state::{AppState, AuthUser};
use axum::Json;
use axum::extract::State;
use tankovault_domain::Permission;

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
        (status = 403, description = "caller does not hold the required permission", body = crate::error::ProblemDetails),
        (status = 404, description = "the system statistics feature is disabled", body = crate::error::ProblemDetails),
    )
)]
pub async fn system_stats(
    State(state): State<AppState>,
    user: AuthUser,
) -> ApiResult<Json<tankovault_db::repo::stats::SystemStats>> {
    user.require(Permission::SystemStats).await?;
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
        (status = 403, description = "caller does not hold the required permission", body = crate::error::ProblemDetails),
        (status = 404, description = "the audit trail feature is disabled", body = crate::error::ProblemDetails),
    )
)]
pub async fn audit_log(
    State(state): State<AppState>,
    user: AuthUser,
) -> ApiResult<Json<Vec<tankovault_db::repo::audit::AuditView>>> {
    user.require(Permission::AuditRead).await?;
    Ok(Json(
        tankovault_db::repo::audit::list_recent(&state.pool, 40).await?,
    ))
}
