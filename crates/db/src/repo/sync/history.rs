//! The user-facing sync history: a transparency log of what the automatic engine did
//! (design v2 §B.2).

use crate::error::DbResult;
use serde::{Deserialize, Serialize};
use serde_json::Value as Json;
use sqlx::{FromRow, PgExecutor};
use tankovault_domain::{SeriesId, UserId};
use time::OffsetDateTime;
use uuid::Uuid;

/// Append one row to the user-facing sync history (design v2 §B.2): a transparency log of what
/// the automatic engine actually did.
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

/// One row of the user-facing sync history (design v2 §B.6 `GET /v1/me/sync/history`).
///
/// Schema'd and `Deserialize` for the same reason as [`ConflictRow`]: `services/api`
/// re-publishes it, so the generated client needs it in the `OpenAPI` document.
#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
pub struct HistoryRow {
    pub id: Uuid,
    pub series_id: Uuid,
    pub series_title: String,
    pub provider: String,
    /// What the engine did, e.g. `pull`, `push` or `resolve`.
    pub action: String,
    /// Free-form, action-specific detail (the changed field and its before/after values).
    pub detail: Json,
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
}

/// A page of a user's sync history, newest first, optionally filtered by series and/or provider.
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
