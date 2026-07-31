//! Conflicts left for the user to resolve under the `ask_me` policy (design v2 §B.3/§B.6).

use crate::error::DbResult;
use serde::{Deserialize, Serialize};
use sqlx::{FromRow, PgExecutor};
use tankovault_domain::{SeriesId, UserId};
use time::OffsetDateTime;
use uuid::Uuid;

/// A conflict to queue: which field disagreed, and what each side held.
///
/// A parameter struct rather than seven positional arguments, for the same reason as
/// [`super::snapshots::AgreedSnapshot`]: four consecutive `&str`s ending in
/// `local_value, remote_value` is transposable in silence, and getting *that* pair backwards
/// would show the user their own value as the remote one.
#[derive(Debug, Clone, Copy)]
pub struct NewConflict<'a> {
    pub user_id: UserId,
    pub series_id: SeriesId,
    pub provider: &'a str,
    /// The field in disagreement, `"progress"` or `"status"`.
    pub field: &'a str,
    pub local_value: &'a str,
    pub remote_value: &'a str,
}

/// Queue a genuine, unresolved conflict for the `ask_me` policy (design v2 §B.3). Idempotent:
/// the unique partial index guarantees at most one pending row per
/// `(user, series, provider, field)`, so re-detection never double-queues.
///
/// # Errors
/// [`crate::DbError::Sqlx`] only — no other variant is reachable. The `DO NOTHING` means a
/// conflict that is already queued is `Ok(())` rather than [`crate::DbError::Conflict`], which
/// is the intended idempotence; note what it also means, because the resolution path applies the
/// *stored* values verbatim: the row keeps the values of the **first** detection, so a remote
/// that moves on between detection and the user's answer is not reflected here.
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

/// One pending conflict awaiting the user's decision (design v2 §B.6 `GET /v1/me/sync/conflicts`).
///
/// Schema'd and `Deserialize` because `services/api` re-publishes this row under
/// `/v1/me/sync/conflicts`, so it has to appear in the `OpenAPI` document for the generated
/// client to expose the endpoint at all.
#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
pub struct ConflictRow {
    pub id: Uuid,
    pub series_id: Uuid,
    pub series_title: String,
    pub provider: String,
    /// Which tracked field disagrees, e.g. `progress` or `status`.
    pub field: String,
    pub local_value: String,
    pub remote_value: String,
    #[serde(with = "time::serde::rfc3339")]
    pub detected_at: OffsetDateTime,
}

/// Every pending (unresolved) conflict for a user, across all providers, newest first.
///
/// # Errors
/// [`crate::DbError::Sqlx`] only — no other variant is reachable. A user with nothing queued and
/// an unknown `user_id` are both an empty `Vec`, not [`crate::DbError::NotFound`] — "no conflicts"
/// is the state the account panel expects most of the time.
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
    pub series_id: Uuid,
    pub provider: String,
    pub field: String,
    pub local_value: String,
    pub remote_value: String,
}

/// Fetch a single pending conflict scoped to its owner, if it exists and is unresolved.
///
/// # Errors
/// [`crate::DbError::Sqlx`] only — no other variant is reachable. Three different situations are
/// deliberately one `Ok(None)`: no such conflict, a conflict belonging to somebody else, and one
/// this user has already resolved. Keep them indistinguishable — separating "not yours" from
/// "does not exist" would turn this into an oracle for other users' conflict ids.
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
/// [`crate::DbError::Sqlx`] only — no other variant is reachable. `Ok(false)` covers the same
/// three situations as [`get_pending_conflict`], and the caller must not default it to `true`:
/// this runs *after* the chosen side has been written to the local database and to the provider,
/// so a `false` here means the conflict will be offered again for a decision already applied.
/// `resolution` is stored verbatim — this layer does not check it is `local` or `remote`; the
/// engine does, before it acts on it.
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
/// [`crate::DbError::Sqlx`] only — no other variant is reachable. A user with none queued is
/// `Ok(0)` — `count(*)` is never `NULL`, which is what the `count!` override asserts. Do not
/// default a failure to zero: zero is the badge's "nothing needs your attention" state, so a
/// failed count would hide decisions the user has to make rather than showing an error.
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
