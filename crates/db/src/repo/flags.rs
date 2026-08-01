//! Feature-flag overrides — the persistence behind [`tankovault_domain::Feature`]. Holds only
//! deviations from shipped defaults, so an empty table is a fully working deployment.
//!
//! Resolution against the compiled default happens in `tankovault_service::flags`, not here.

use crate::error::DbResult;
use sqlx::{FromRow, PgExecutor};
use tankovault_domain::UserId;
use time::OffsetDateTime;

/// A stored override, with the provenance the control plane displays.
#[derive(Debug, Clone, serde::Serialize)]
pub struct OverrideRow {
    /// The feature key, as a string not the enum, so an override from a retired build stays
    /// visible instead of vanishing from the only page that can delete it.
    pub feature_key: String,
    pub enabled: bool,
    /// Why the switch was flipped, if the operator said.
    pub note: Option<String>,
    #[serde(with = "time::serde::rfc3339")]
    pub updated_at: OffsetDateTime,
    /// Username of the operator who last changed it; `None` once that account is erased.
    pub updated_by: Option<String>,
}

/// Every stored override, keyed by feature.
///
/// Raw rows, not a resolved map — pairing with the compiled registry is the caller's job.
///
/// # Errors
/// `Sqlx` only; an empty table is `Ok(vec![])`.
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
/// Narrower than [`list_overrides`]: no provenance or `users` join, since this is refreshed on
/// a timer in every service.
///
/// # Errors
/// `Sqlx` only. Callers must not collapse `Err` into `Ok(vec![])` — that reads as "no
/// overrides" and would silently reset every flag to its compiled default; treat failure as
/// keep-previous-snapshot.
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
/// Always writes, even at the value it already has, so `updated_at`/`updated_by` show who
/// last confirmed it.
///
/// # Errors
/// `Sqlx` only; a repeat write is not [`crate::DbError::Conflict`], an erased `updated_by` is
/// a foreign-key violation (500).
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
///
/// # Errors
/// `Sqlx` only; "nothing to clear" is `Ok(false)`, deliberately indistinguishable from an
/// unknown `feature_key`.
pub async fn clear_override<'e, E: PgExecutor<'e>>(exec: E, feature_key: &str) -> DbResult<bool> {
    let result = sqlx::query!(
        "DELETE FROM feature_flag_overrides WHERE feature_key = $1",
        feature_key,
    )
    .execute(exec)
    .await?;
    Ok(result.rows_affected() > 0)
}
