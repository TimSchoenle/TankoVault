//! Read-only operator dashboard surfaces: the system overview and the audit trail.

use crate::error::ApiResult;
use crate::openapi::ADMIN_OVERVIEW_TAG;
use crate::state::{AppState, AuthUser};
use crate::views::IntoView;
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
        (status = 200, description = "System-wide stats", body = tankovault_contracts::admin::SystemStatsView),
        (status = 401, description = "authentication required", body = crate::error::ProblemDetails),
        (status = 403, description = "caller does not hold the required permission", body = crate::error::ProblemDetails),
        (status = 404, description = "the system statistics feature is disabled", body = crate::error::ProblemDetails),
    )
)]
pub async fn system_stats(
    State(state): State<AppState>,
    user: AuthUser,
) -> ApiResult<Json<tankovault_contracts::admin::SystemStatsView>> {
    user.require(Permission::SystemStats).await?;
    // Served from a snapshot: every column is a `count(*)` over a table this aggregates whole.
    // See `crate::cache` for what the staleness buys.
    let pool = state.pool.clone();
    let overview = state
        .system_stats
        .get(move || {
            let pool = pool.clone();
            async move { tankovault_db::repo::stats::system_overview(&pool).await }
        })
        .await?;
    Ok(Json(overview.into_view()))
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
        (status = 200, description = "Up to 40 most recent audit records", body = Vec<tankovault_contracts::admin::AuditView>),
        (status = 401, description = "authentication required", body = crate::error::ProblemDetails),
        (status = 403, description = "caller does not hold the required permission", body = crate::error::ProblemDetails),
        (status = 404, description = "the audit trail feature is disabled", body = crate::error::ProblemDetails),
    )
)]
pub async fn audit_log(
    State(state): State<AppState>,
    user: AuthUser,
) -> ApiResult<Json<Vec<tankovault_contracts::admin::AuditView>>> {
    user.require(Permission::AuditRead).await?;
    let rows = tankovault_db::repo::audit::list_recent(&state.pool, 40).await?;
    Ok(Json(rows.into_view()))
}
