//! Read-only operator dashboard surfaces: the system overview and the audit trail.

use crate::error::ApiResult;
use crate::openapi::ADMIN_OVERVIEW_TAG;
use crate::state::{AppState, AuthUser};
use crate::views::IntoView;
use axum::Json;
use axum::extract::{Query, State};
use serde::Deserialize;
use tankovault_domain::{Permission, UserId};
use time::OffsetDateTime;
use utoipa::IntoParams;
use uuid::Uuid;

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
        (status = 403, description = "no second factor is enrolled, or the caller does not hold the required permission", body = crate::error::ProblemDetails),
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

/// Audit paging. Capped server-side: the trail is the deepest table on the admin surface, and
/// a caller asking for everything would page a year of operator actions into one response.
const MAX_AUDIT_PAGE: u32 = 200;
const DEFAULT_AUDIT_PAGE: u32 = 40;

#[derive(Debug, Deserialize, IntoParams)]
pub struct AuditQuery {
    /// Only actions attributed to this account. Absent matches every actor and the system.
    // A bare `Uuid`, not the `UserId` newtype: a newtype publishes as a `$ref`, and an
    // *optional* `$ref` parameter generates a `oneOf` wrapper enum in the API client that no
    // caller can build from an id it already holds.
    #[serde(default)]
    pub actor: Option<Uuid>,
    /// Exact action key, e.g. `admin.user.update`.
    #[serde(default)]
    pub action: Option<String>,
    /// Case-insensitive substring of the target.
    #[serde(default)]
    pub target: Option<String>,
    /// Inclusive lower bound on `created_at`, RFC 3339.
    // `value_type` because `utoipa` has no schema for `OffsetDateTime`; the serde attribute
    // above already pins the wire format this claims.
    #[serde(default, with = "time::serde::rfc3339::option")]
    #[param(value_type = Option<String>)]
    pub since: Option<OffsetDateTime>,
    /// Exclusive upper bound on `created_at`, RFC 3339.
    #[serde(default, with = "time::serde::rfc3339::option")]
    #[param(value_type = Option<String>)]
    pub until: Option<OffsetDateTime>,
    #[serde(default)]
    pub limit: Option<u32>,
    #[serde(default)]
    pub offset: Option<u32>,
}

/// Get the audit log
///
/// A filtered, paged window on the privileged-action trail (design §16), newest first.
///
/// Every filter is applied in SQL. The console cannot filter client-side here on purpose: it
/// holds one page, and a filter over one page silently answers a different question than the
/// one asked — the defect `users/activity.rs` shipped.
#[utoipa::path(
    get,
    path = "/v1/admin/audit",
    tag = ADMIN_OVERVIEW_TAG,
    params(AuditQuery),
    security(("bearer_auth" = [])),
    responses(
        (status = 200, description = "A page of the audit trail", body = tankovault_contracts::admin::AuditPageView),
        (status = 401, description = "authentication required", body = crate::error::ProblemDetails),
        (status = 403, description = "no second factor is enrolled, or the caller does not hold the required permission", body = crate::error::ProblemDetails),
        (status = 404, description = "the audit trail feature is disabled", body = crate::error::ProblemDetails),
    )
)]
pub async fn audit_log(
    State(state): State<AppState>,
    user: AuthUser,
    Query(q): Query<AuditQuery>,
) -> ApiResult<Json<tankovault_contracts::admin::AuditPageView>> {
    user.require(Permission::AuditRead).await?;
    let limit = q
        .limit
        .unwrap_or(DEFAULT_AUDIT_PAGE)
        .clamp(1, MAX_AUDIT_PAGE);
    let filter = tankovault_db::repo::audit::AuditFilter {
        actor_id: q.actor.map(UserId::from),
        // Blank is the absence of a filter, not a search for the empty string — a cleared
        // filter box sends `?action=`.
        action: q.action.as_deref().map(str::trim).filter(|s| !s.is_empty()),
        target: q.target.as_deref().map(str::trim).filter(|s| !s.is_empty()),
        since: q.since,
        until: q.until,
    };
    let page = tankovault_db::repo::audit::list_filtered(
        &state.pool,
        &filter,
        i64::from(limit),
        i64::from(q.offset.unwrap_or(0)),
    )
    .await?;
    Ok(Json(page.into_view()))
}

/// List audit action keys
///
/// Every distinct action present in the trail, for the console's filter picker. Read from the
/// data rather than from a hand-written list, so a newly recorded action is filterable the
/// first time it happens.
#[utoipa::path(
    get,
    path = "/v1/admin/audit/actions",
    tag = ADMIN_OVERVIEW_TAG,
    security(("bearer_auth" = [])),
    responses(
        (status = 200, description = "Distinct action keys, sorted", body = Vec<String>),
        (status = 401, description = "authentication required", body = crate::error::ProblemDetails),
        (status = 403, description = "no second factor is enrolled, or the caller does not hold the required permission", body = crate::error::ProblemDetails),
        (status = 404, description = "the audit trail feature is disabled", body = crate::error::ProblemDetails),
    )
)]
pub async fn audit_actions(
    State(state): State<AppState>,
    user: AuthUser,
) -> ApiResult<Json<Vec<String>>> {
    user.require(Permission::AuditRead).await?;
    Ok(Json(
        tankovault_db::repo::audit::distinct_actions(&state.pool).await?,
    ))
}
