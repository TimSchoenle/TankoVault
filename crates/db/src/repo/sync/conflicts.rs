//! Conflicts left for the user to resolve under the `ask_me` policy (design v2 §B.3/§B.6).

use crate::error::DbResult;
use serde::{Deserialize, Serialize};
use sqlx::{FromRow, PgExecutor};
use tankovault_domain::{SeriesId, UserId};
use time::OffsetDateTime;
use uuid::Uuid;

/// A conflict to queue: which field disagreed, and what each side held.
///
/// A struct, not positional args, as [`super::snapshots::AgreedSnapshot`]: `local_value` next
/// to `remote_value` transposes silently and would show a user their own value as the remote one.
#[derive(Debug, Clone, Copy)]
pub struct NewConflict<'a> {
    /// Whose account the conflict is on.
    pub user_id: UserId,
    /// The series both sides disagree about.
    pub series_id: SeriesId,
    /// Which external tracker, as a slug.
    pub provider: &'a str,
    /// The field in disagreement, `"progress"` or `"status"`.
    pub field: &'a str,
    /// What this deployment holds for `field`, rendered as text.
    pub local_value: &'a str,
    /// What the tracker holds for it, rendered the same way.
    pub remote_value: &'a str,
}

/// Queue a genuine, unresolved conflict for the `ask_me` policy (design v2 §B.3). Idempotent: a
/// unique partial index caps this at one pending row per `(user, series, provider, field)`.
///
/// # Errors
/// [`crate::DbError::Sqlx`] only; an already-queued conflict is `Ok(())`, not
/// [`crate::DbError::Conflict`] — the row keeps the first detection's values.
pub async fn insert_conflict<'e, E: PgExecutor<'e>>(
    exec: E,
    conflict: &NewConflict<'_>,
) -> DbResult<()> {
    let NewConflict {
        user_id,
        series_id,
        provider,
        field,
        local_value,
        remote_value,
    } = *conflict;
    sqlx::query!(
        "INSERT INTO sync_conflicts \
            (user_id, series_id, provider, field, local_value, remote_value) \
         VALUES ($1,$2,$3,$4,$5,$6) \
         ON CONFLICT (user_id, series_id, provider, field) WHERE resolved_at IS NULL \
         DO NOTHING",
        user_id.as_uuid(),
        series_id.as_uuid(),
        provider,
        field,
        local_value,
        remote_value,
    )
    .execute(exec)
    .await?;
    Ok(())
}

/// One pending conflict awaiting the user's decision (design v2 §B.6).
///
/// `Deserialize` + schema'd: `services/api` republishes this row, so it must appear in `OpenAPI`.
#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
pub struct ConflictRow {
    /// The conflict, which is what a resolve call names.
    pub id: Uuid,
    /// The series both sides disagree about.
    pub series_id: Uuid,
    /// Its canonical title, joined in so the list renders from one fetch.
    pub series_title: String,
    /// Which external tracker, as a slug.
    pub provider: String,
    /// Which tracked field disagrees, e.g. `progress` or `status`.
    pub field: String,
    /// What this deployment holds for `field`, rendered as text.
    pub local_value: String,
    /// What the tracker holds for it.
    pub remote_value: String,
    /// When a sync first found the two disagreeing.
    #[serde(with = "time::serde::rfc3339")]
    pub detected_at: OffsetDateTime,
}

/// Every pending (unresolved) conflict for a user, across all providers, newest first.
///
/// # Errors
/// [`crate::DbError::Sqlx`] only; nothing queued is an empty `Vec`, never
/// [`crate::DbError::NotFound`].
pub async fn list_pending_conflicts<'e, E: PgExecutor<'e>>(
    exec: E,
    user_id: UserId,
) -> DbResult<Vec<ConflictRow>> {
    let rows = sqlx::query_as!(
        ConflictRow,
        "SELECT c.id, c.series_id, s.canonical_title AS series_title, c.provider, c.field, \
                c.local_value, c.remote_value, c.detected_at \
         FROM sync_conflicts c JOIN series s ON s.id = c.series_id \
         WHERE c.user_id = $1 AND c.resolved_at IS NULL \
         ORDER BY c.detected_at DESC",
        user_id.as_uuid(),
    )
    .fetch_all(exec)
    .await?;
    Ok(rows)
}

/// A pending conflict's identity + values, used to apply a user's resolution.
#[derive(Debug, Clone, FromRow)]
pub struct ConflictDetail {
    /// The series the resolution writes to.
    pub series_id: Uuid,
    /// Which external tracker, as a slug.
    pub provider: String,
    /// Which tracked field disagrees.
    pub field: String,
    /// The value a `remote` resolution overwrites.
    pub local_value: String,
    /// The value a `remote` resolution writes.
    pub remote_value: String,
}

/// Fetch a single pending conflict scoped to its owner, if it exists and is unresolved.
///
/// # Errors
/// [`crate::DbError::Sqlx`] only; unknown, not-yours and already-resolved are all `Ok(None)` —
/// distinguishing them would let a client probe other users' conflict ids.
pub async fn get_pending_conflict<'e, E: PgExecutor<'e>>(
    exec: E,
    user_id: UserId,
    conflict_id: Uuid,
) -> DbResult<Option<ConflictDetail>> {
    let row = sqlx::query_as!(
        ConflictDetail,
        "SELECT series_id, provider, field, local_value, remote_value \
         FROM sync_conflicts \
         WHERE id = $1 AND user_id = $2 AND resolved_at IS NULL",
        conflict_id,
        user_id.as_uuid(),
    )
    .fetch_optional(exec)
    .await?;
    Ok(row)
}

/// Mark a conflict resolved with the chosen side (`local` | `remote`). Returns `true` if a
/// pending row was updated.
///
/// # Errors
/// [`crate::DbError::Sqlx`] only; `Ok(false)` covers the same cases as [`get_pending_conflict`] —
/// never default it to `true`, or an already-applied resolution is silently re-offered.
pub async fn resolve_conflict<'e, E: PgExecutor<'e>>(
    exec: E,
    user_id: UserId,
    conflict_id: Uuid,
    resolution: &str,
) -> DbResult<bool> {
    let result = sqlx::query!(
        "UPDATE sync_conflicts SET resolved_at = now(), resolution = $3 \
         WHERE id = $1 AND user_id = $2 AND resolved_at IS NULL",
        conflict_id,
        user_id.as_uuid(),
        resolution,
    )
    .execute(exec)
    .await?;
    Ok(result.rows_affected() > 0)
}

/// Count a user's pending conflicts, for the account panel badge and the admin console.
///
/// # Errors
/// [`crate::DbError::Sqlx`] only; never default a failure to `0` — the badge reads zero as
/// "nothing needs attention" and would hide the failure.
pub async fn count_pending_conflicts<'e, E: PgExecutor<'e>>(
    exec: E,
    user_id: UserId,
) -> DbResult<i64> {
    let n = sqlx::query_scalar!(
        "SELECT count(*) AS \"count!\" FROM sync_conflicts WHERE user_id = $1 AND resolved_at IS NULL",
        user_id.as_uuid(),
    )
    .fetch_one(exec)
    .await?;
    Ok(n)
}
