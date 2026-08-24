//! Recommender tuning overrides — the persistence behind [`tankovault_domain::Tunable`]. Holds
//! only deviations from the compiled registry, so an empty table is a fully tuned deployment.
//!
//! Resolution against the compiled default, and clamping to the registry's range, happen in
//! `tankovault_service::tunables`, not here.

use crate::error::DbResult;
use sqlx::{FromRow, PgExecutor};
use tankovault_domain::UserId;
use time::OffsetDateTime;

/// A stored override, with the provenance the console displays.
#[derive(Debug, Clone, serde::Serialize)]
pub struct TunableOverrideRow {
    /// The tunable key, as a string not the enum, so an override from a retired build stays
    /// visible instead of vanishing from the only page that can delete it.
    pub key: String,
    /// The value as it was stored. Reads clamp it to the registry's range, so a bound
    /// narrowed by a later build does not have to rewrite the rows written under the old one.
    pub value: f64,
    /// Why it was changed, if the operator said.
    pub note: Option<String>,
    /// When it last changed. Creating the override and editing it both move this.
    #[serde(with = "time::serde::rfc3339")]
    pub updated_at: OffsetDateTime,
    /// Username of the operator who last changed it; `None` once that account is erased.
    pub updated_by: Option<String>,
}

/// Every stored override, keyed by tunable.
///
/// Raw rows, not a resolved map — pairing with the compiled registry is the caller's job.
///
/// # Errors
/// `Sqlx` only; an empty table is `Ok(vec![])`.
pub async fn list_overrides<'e, E: PgExecutor<'e>>(exec: E) -> DbResult<Vec<TunableOverrideRow>> {
    #[derive(FromRow)]
    struct Row {
        key: String,
        value: f64,
        note: Option<String>,
        updated_at: OffsetDateTime,
        updated_by: Option<String>,
    }
    let rows = sqlx::query_as!(
        Row,
        "SELECT t.key, t.value, t.note, t.updated_at, \
                u.username AS \"updated_by?: String\" \
         FROM tunable_overrides t \
         LEFT JOIN users u ON u.id = t.updated_by \
         ORDER BY t.key",
    )
    .fetch_all(exec)
    .await?;

    Ok(rows
        .into_iter()
        .map(|r| TunableOverrideRow {
            key: r.key,
            value: r.value,
            note: r.note,
            updated_at: r.updated_at,
            updated_by: r.updated_by,
        })
        .collect())
}

/// The minimal `(key, value)` pairs the runtime snapshot needs.
///
/// Narrower than [`list_overrides`]: no provenance or `users` join, since this is refreshed on
/// a timer in every service.
///
/// # Errors
/// `Sqlx` only. Callers must not collapse `Err` into `Ok(vec![])` — that reads as "no
/// overrides" and would silently discard everything an operator has tuned; treat failure as
/// keep-previous-snapshot.
pub async fn effective_overrides<'e, E: PgExecutor<'e>>(exec: E) -> DbResult<Vec<(String, f64)>> {
    #[derive(FromRow)]
    struct Row {
        key: String,
        value: f64,
    }
    let rows = sqlx::query_as!(Row, "SELECT key, value FROM tunable_overrides")
        .fetch_all(exec)
        .await?;
    Ok(rows.into_iter().map(|r| (r.key, r.value)).collect())
}

/// Record an explicit operator decision for `key`.
///
/// Always writes, even at the value it already has, so `updated_at`/`updated_by` show who last
/// confirmed it. Range validation belongs to the caller: this layer stores what it is given, and
/// every reader clamps regardless.
///
/// # Errors
/// `Sqlx` only; a repeat write is not [`crate::DbError::Conflict`], an erased `updated_by` is a
/// foreign-key violation (500).
pub async fn set_override<'e, E: PgExecutor<'e>>(
    exec: E,
    key: &str,
    value: f64,
    note: Option<&str>,
    updated_by: UserId,
) -> DbResult<()> {
    sqlx::query!(
        "INSERT INTO tunable_overrides (key, value, note, updated_at, updated_by) \
         VALUES ($1,$2,$3,now(),$4) \
         ON CONFLICT (key) DO UPDATE SET \
             value = excluded.value, \
             note = excluded.note, \
             updated_at = now(), \
             updated_by = excluded.updated_by",
        key,
        value,
        note,
        updated_by.as_uuid(),
    )
    .execute(exec)
    .await?;
    Ok(())
}

/// Drop the override for `key`, returning the tunable to its compiled default.
///
/// Returns `false` when there was nothing to clear, so the caller can answer "reset" honestly
/// rather than reporting a change that did not happen.
///
/// # Errors
/// `Sqlx` only; "nothing to clear" is `Ok(false)`, deliberately indistinguishable from an
/// unknown `key`.
pub async fn clear_override<'e, E: PgExecutor<'e>>(exec: E, key: &str) -> DbResult<bool> {
    let result = sqlx::query!("DELETE FROM tunable_overrides WHERE key = $1", key)
        .execute(exec)
        .await?;
    Ok(result.rows_affected() > 0)
}
