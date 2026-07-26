//! Feature-flag overrides — the persistence behind [`tankovault_domain::Feature`].
//!
//! This table holds *only* deviations from the shipped defaults, which is what makes the flag
//! system additive: a feature added in code appears in the control plane at its declared
//! default with no migration and no seed row, and an empty table is a fully working
//! deployment. See the [`tankovault_domain::features`] module docs.
//!
//! Resolution (override, else compiled default) is not done here. It belongs to the runtime
//! that caches the snapshot and is consulted per request — `tankovault_service::flags` — so
//! that there is one place where "is this feature on" is answered and it is not a database
//! round trip on the hot path.

use crate::error::DbResult;
use sqlx::{FromRow, PgExecutor};
use tankovault_domain::UserId;
use time::OffsetDateTime;

/// A stored override, with the provenance the control plane displays.
#[derive(Debug, Clone, serde::Serialize, utoipa::ToSchema)]
pub struct OverrideRow {
    /// The feature key. A string rather than the enum so an override left behind by another
    /// build stays *visible* to an operator instead of vanishing from the page that is the
    /// only place it can be deleted.
    pub feature_key: String,
    pub enabled: bool,
    /// Why the switch was flipped, if the operator said.
    pub note: Option<String>,
    #[serde(with = "time::serde::rfc3339")]
    #[schema(value_type = String)]
    pub updated_at: OffsetDateTime,
    /// Username of the operator who last changed it; `None` once that account is erased.
    pub updated_by: Option<String>,
}

/// Every stored override, keyed by feature.
///
/// Returns raw rows rather than a resolved map: the caller pairs them with the compiled
/// registry, and doing so here would mean this layer had to know which features exist.
pub async fn list_overrides<'e, E: PgExecutor<'e>>(exec: E) -> DbResult<Vec<OverrideRow>> {
    #[derive(FromRow)]
    struct Row {
        feature_key: String,
        enabled: bool,
        note: Option<String>,
        updated_at: OffsetDateTime,
        updated_by: Option<String>,
    }
    let rows = sqlx::query_as!(
        Row,
        "SELECT f.feature_key, f.enabled, f.note, f.updated_at, \
                u.username AS \"updated_by?: String\" \
         FROM feature_flag_overrides f \
         LEFT JOIN users u ON u.id = f.updated_by \
         ORDER BY f.feature_key",
    )
    .fetch_all(exec)
    .await?;

    Ok(rows
        .into_iter()
        .map(|r| OverrideRow {
            feature_key: r.feature_key,
            enabled: r.enabled,
            note: r.note,
            updated_at: r.updated_at,
            updated_by: r.updated_by,
        })
        .collect())
}

/// The minimal `(key, enabled)` pairs the runtime gate needs.
///
/// A separate, narrower query from [`list_overrides`] because the gate refreshes this on a
/// timer in every service and has no use for provenance or the `users` join.
pub async fn effective_overrides<'e, E: PgExecutor<'e>>(exec: E) -> DbResult<Vec<(String, bool)>> {
    #[derive(FromRow)]
    struct Row {
        feature_key: String,
        enabled: bool,
    }
    let rows = sqlx::query_as!(
        Row,
        "SELECT feature_key, enabled FROM feature_flag_overrides",
    )
    .fetch_all(exec)
    .await?;
    Ok(rows
        .into_iter()
        .map(|r| (r.feature_key, r.enabled))
        .collect())
}

/// Record an explicit operator decision for `feature_key`.
///
/// Always writes, even when the value already matches: setting a flag to the value it already
/// has is a deliberate act that pins it against a future change of the compiled default, and
/// it refreshes `updated_at`/`updated_by` so the page shows who last confirmed it.
pub async fn set_override<'e, E: PgExecutor<'e>>(
    exec: E,
    feature_key: &str,
    enabled: bool,
    note: Option<&str>,
    updated_by: UserId,
) -> DbResult<()> {
    sqlx::query!(
        "INSERT INTO feature_flag_overrides (feature_key, enabled, note, updated_at, updated_by) \
         VALUES ($1,$2,$3,now(),$4) \
         ON CONFLICT (feature_key) DO UPDATE SET \
             enabled = excluded.enabled, \
             note = excluded.note, \
             updated_at = now(), \
             updated_by = excluded.updated_by",
        feature_key,
        enabled,
        note,
        updated_by.as_uuid(),
    )
    .execute(exec)
    .await?;
    Ok(())
}

/// Drop the override for `feature_key`, returning the feature to its compiled default.
///
/// Returns `false` when there was nothing to clear, so the caller can answer "reset" honestly
/// rather than reporting a change that did not happen.
pub async fn clear_override<'e, E: PgExecutor<'e>>(exec: E, feature_key: &str) -> DbResult<bool> {
    let result = sqlx::query!(
        "DELETE FROM feature_flag_overrides WHERE feature_key = $1",
        feature_key,
    )
    .execute(exec)
    .await?;
    Ok(result.rows_affected() > 0)
}
