//! The user-facing sync history: a transparency log of what the automatic engine did
//! (design v2 §B.2).

use crate::error::DbResult;
use serde::{Deserialize, Serialize};
use serde_json::Value as Json;
use sqlx::{FromRow, PgExecutor};
use tankovault_domain::{SeriesId, UserId};
use time::OffsetDateTime;
use uuid::Uuid;

/// Append one row to the user-facing sync history (design v2 §B.2).
///
/// # Errors
/// [`crate::DbError::Sqlx`] only; an unknown `user_id`/`series_id` is a foreign-key violation (500).
pub async fn append_history<'e, E: PgExecutor<'e>>(
    exec: E,
    user_id: UserId,
    series_id: SeriesId,
    provider: &str,
    action: &str,
    detail: &Json,
) -> DbResult<()> {
    sqlx::query!(
        "INSERT INTO sync_history (user_id, series_id, provider, action, detail) \
         VALUES ($1,$2,$3,$4,$5)",
        user_id.as_uuid(),
        series_id.as_uuid(),
        provider,
        action,
        detail,
    )
    .execute(exec)
    .await?;
    Ok(())
}

/// One row of the user-facing sync history (design v2 §B.6).
///
/// `Deserialize` + schema'd: `services/api` republishes this in the `OpenAPI` document.
#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
pub struct HistoryRow {
    /// The history row.
    pub id: Uuid,
    /// The local series the action touched.
    pub series_id: Uuid,
    /// Its canonical title, joined in so the list renders from one fetch.
    pub series_title: String,
    /// Which external tracker, as a slug.
    pub provider: String,
    /// What the engine did, e.g. `pull`, `push` or `resolve`.
    pub action: String,
    /// Free-form, action-specific detail (the changed field and its before/after values).
    pub detail: Json,
    /// When the action was taken, which is what the list is ordered by.
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
}

/// A page of a user's sync history, newest first, optionally filtered by series and/or provider.
///
/// # Errors
/// [`crate::DbError::Sqlx`] only; no match is an empty `Vec`, never [`crate::DbError::NotFound`].
pub async fn list_history<'e, E: PgExecutor<'e>>(
    exec: E,
    user_id: UserId,
    series_id: Option<SeriesId>,
    provider: Option<&str>,
    limit: i64,
    offset: i64,
) -> DbResult<Vec<HistoryRow>> {
    let rows = sqlx::query_as!(
        HistoryRow,
        "SELECT h.id, h.series_id, s.canonical_title AS series_title, h.provider, h.action, \
                h.detail AS \"detail: Json\", h.created_at \
         FROM sync_history h JOIN series s ON s.id = h.series_id \
         WHERE h.user_id = $1 \
           AND ($2::uuid IS NULL OR h.series_id = $2) \
           AND ($3::text IS NULL OR h.provider = $3) \
         ORDER BY h.created_at DESC \
         LIMIT $4 OFFSET $5",
        user_id.as_uuid(),
        series_id.map(SeriesId::as_uuid),
        provider,
        limit,
        offset,
    )
    .fetch_all(exec)
    .await?;
    Ok(rows)
}
