//! Read models for the admin Sync console.
//!
//! Consumed only by `services/api/src/admin/sync.rs`; everything else in this module tree is
//! read by `services/sync`.

use crate::error::DbResult;
use serde::Serialize;
use sqlx::{FromRow, PgExecutor};
use tankovault_domain::{SeriesId, UserId};
use time::OffsetDateTime;
use uuid::Uuid;

/// One row of the admin Sync console's "Linked accounts" table; policy columns are read-only
/// operator visibility (design v2 §B.7).
///
/// `pending_conflicts` is scoped to this `(user, provider)` row, not the user overall — see
/// [`count_pending_conflicts`](super::conflicts::count_pending_conflicts) for the user-wide count.
#[derive(Debug, Clone, Serialize, FromRow)]
pub struct AdminAccountRow {
    pub user_id: Uuid,
    pub username: String,
    pub provider: String,
    pub external_username: Option<String>,
    #[serde(with = "time::serde::rfc3339::option")]
    pub last_synced_at: Option<OffsetDateTime>,
    pub last_error: Option<String>,
    pub auto_sync_enabled: bool,
    pub conflict_policy: String,
    pub pending_conflicts: i64,
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
}

/// All linked external accounts across every user, newest-error-first then most-recently synced.
///
/// # Errors
/// [`crate::DbError::Sqlx`] only; nothing linked is an empty `Vec`. Erased users are inner-joined
/// out rather than shown blank.
pub async fn admin_list_accounts<'e, E: PgExecutor<'e>>(
    exec: E,
    limit: i64,
) -> DbResult<Vec<AdminAccountRow>> {
    let rows = sqlx::query_as!(
        AdminAccountRow,
        "SELECT ea.user_id, u.username, ea.provider, ea.external_username, \
                ea.last_synced_at, ea.last_error, ea.auto_sync_enabled, ea.conflict_policy, \
                (SELECT count(*) FROM sync_conflicts sc \
                   WHERE sc.user_id = ea.user_id AND sc.provider = ea.provider \
                     AND sc.resolved_at IS NULL) AS \"pending_conflicts!\", \
                ea.created_at \
         FROM external_accounts ea JOIN users u ON u.id = ea.user_id \
         ORDER BY (ea.last_error IS NOT NULL) DESC, ea.last_synced_at DESC NULLS LAST \
         LIMIT $1",
        limit,
    )
    .fetch_all(exec)
    .await?;
    Ok(rows)
}

/// One row of the admin Sync console's "Series mappings" table.
#[derive(Debug, Clone, Serialize, FromRow)]
pub struct AdminMappingRow {
    pub series_id: Uuid,
    pub series_title: String,
    pub provider: String,
    pub external_id: String,
    #[serde(with = "time::serde::rfc3339")]
    pub updated_at: OffsetDateTime,
}

/// All series↔external mappings across every provider, most recently updated first.
///
/// # Errors
/// [`crate::DbError::Sqlx`] only; nothing mapped is an empty `Vec`. `limit` is unvalidated — the
/// caller owns the bound.
pub async fn admin_list_mappings<'e, E: PgExecutor<'e>>(
    exec: E,
    limit: i64,
) -> DbResult<Vec<AdminMappingRow>> {
    let rows = sqlx::query_as!(
        AdminMappingRow,
        "SELECT sm.series_id, s.canonical_title AS series_title, sm.provider, \
                sm.external_id, sm.updated_at \
         FROM sync_mappings sm JOIN series s ON s.id = sm.series_id \
         ORDER BY sm.updated_at DESC \
         LIMIT $1",
        limit,
    )
    .fetch_all(exec)
    .await?;
    Ok(rows)
}

/// Every external mapping recorded for one canonical series (one row per provider).
///
/// # Errors
/// [`crate::DbError::Sqlx`] only; unknown or unmapped series are both an empty `Vec`.
pub async fn admin_list_mappings_for_series<'e, E: PgExecutor<'e>>(
    exec: E,
    series_id: SeriesId,
) -> DbResult<Vec<AdminMappingRow>> {
    let rows = sqlx::query_as!(
        AdminMappingRow,
        "SELECT sm.series_id, s.canonical_title AS series_title, sm.provider, \
                sm.external_id, sm.updated_at \
         FROM sync_mappings sm JOIN series s ON s.id = sm.series_id \
         WHERE sm.series_id = $1 \
         ORDER BY sm.provider",
        series_id.as_uuid(),
    )
    .fetch_all(exec)
    .await?;
    Ok(rows)
}

/// One row of the admin Sync console's "Assign queue" — a canonical series that has **no**
/// external mapping for the given provider yet, so an operator can review and assign one.
#[derive(Debug, Clone, Serialize, FromRow)]
pub struct UnmappedSeriesRow {
    pub series_id: Uuid,
    pub series_title: String,
    /// How many local sources back this series (a proxy for how confident a match is worth).
    pub source_count: i64,
}

/// Series lacking a mapping for `provider`, richest (most sources) first. An optional
/// case-insensitive title `query` narrows the list.
///
/// # Errors
/// [`crate::DbError::Sqlx`] only; empty is the assign queue's goal state, not a failure. The
/// `len() > 2` guard is on the wrapped `%…%` pattern, so it rejects only an empty query.
pub async fn admin_list_unmapped<'e, E: PgExecutor<'e>>(
    exec: E,
    provider: &str,
    query: Option<&str>,
    limit: i64,
) -> DbResult<Vec<UnmappedSeriesRow>> {
    let like = query
        .map(|q| format!("%{}%", q.trim()))
        .filter(|q| q.len() > 2);
    let rows = sqlx::query_as!(
        UnmappedSeriesRow,
        "SELECT s.id AS series_id, s.canonical_title AS series_title, \
                count(ss.id) AS \"source_count!\" \
         FROM series s \
         LEFT JOIN series_sources ss ON ss.series_id = s.id \
         WHERE NOT EXISTS ( \
                 SELECT 1 FROM sync_mappings sm \
                 WHERE sm.series_id = s.id AND sm.provider = $1) \
           AND ($2::text IS NULL OR s.canonical_title ILIKE $2) \
         GROUP BY s.id, s.canonical_title \
         ORDER BY count(ss.id) DESC, s.canonical_title \
         LIMIT $3",
        provider,
        like,
        limit,
    )
    .fetch_all(exec)
    .await?;
    Ok(rows)
}

/// One row of the admin console's "Unmatched remote entries" queue: a fetched provider entry
/// the auto-matcher could not confidently link to a local series.
#[derive(Debug, Clone, Serialize, FromRow)]
pub struct RemoteEntryRow {
    pub user_id: Uuid,
    pub username: String,
    pub provider: String,
    pub external_id: String,
    pub title: String,
    pub status: String,
    pub progress: f64,
    pub content_type: String,
    pub start_year: Option<i32>,
}

/// Unmatched remote entries for `provider`, alphabetically by title — the reverse view of
/// [`admin_list_unmapped`]. `query` behaves identically.
///
/// # Errors
/// [`crate::DbError::Sqlx`] only; nothing unmatched is an empty `Vec`.
pub async fn admin_list_unmatched_remote<'e, E: PgExecutor<'e>>(
    exec: E,
    provider: &str,
    query: Option<&str>,
    limit: i64,
) -> DbResult<Vec<RemoteEntryRow>> {
    let like = query
        .map(|q| format!("%{}%", q.trim()))
        .filter(|q| q.len() > 2);
    let rows = sqlx::query_as!(
        RemoteEntryRow,
        "SELECT re.user_id, u.username, re.provider, re.external_id, re.title, re.status, \
                re.progress, re.content_type, re.start_year \
         FROM sync_remote_entries re JOIN users u ON u.id = re.user_id \
         WHERE re.series_id IS NULL AND re.provider = $1 \
           AND ($2::text IS NULL OR re.title ILIKE $2) \
         ORDER BY re.title \
         LIMIT $3",
        provider,
        like,
        limit,
    )
    .fetch_all(exec)
    .await?;
    Ok(rows)
}

/// The stored snapshot for one remote entry, imported when an operator hand-assigns it to a
/// series without waiting for the next pull.
#[derive(Debug, Clone, FromRow)]
pub struct RemoteEntrySnapshot {
    pub title: String,
    pub status: String,
    pub progress: f64,
    pub updated_at: OffsetDateTime,
}

/// Fetch one stored remote-entry snapshot, if present.
///
/// # Errors
/// [`crate::DbError::Sqlx`] only; no snapshot yet is `Ok(None)` — the caller assigns the mapping
/// and lets the next pull fill in status/progress.
pub async fn get_remote_entry<'e, E: PgExecutor<'e>>(
    exec: E,
    user_id: UserId,
    provider: &str,
    external_id: &str,
) -> DbResult<Option<RemoteEntrySnapshot>> {
    let row = sqlx::query_as!(
        RemoteEntrySnapshot,
        "SELECT title, status, progress, updated_at FROM sync_remote_entries \
         WHERE user_id = $1 AND provider = $2 AND external_id = $3",
        user_id.as_uuid(),
        provider,
        external_id,
    )
    .fetch_optional(exec)
    .await?;
    Ok(row)
}

/// A local-series candidate for the admin "match every loaded entry" suggestions: trigram
/// similarity plus display fields for the console to rank and preview.
#[derive(Debug, Clone, FromRow)]
pub struct SeriesCandidateRow {
    pub series_id: Uuid,
    pub title: String,
    pub normalized_title: String,
    pub content_type: String,
    pub release_year: Option<i32>,
    pub source_count: i64,
    /// Best trigram similarity in `[0,1]` across the canonical + alternative titles.
    pub similarity: f32,
}

/// Trigram-similar local series for a remote entry's `normalized` title, richest first. Mirrors
/// [`crate::repo::matching::find_candidates`] but adds the display title and source count.
///
/// # Errors
/// [`crate::DbError::Sqlx`] only; no matches is an empty `Vec` — the console offers manual
/// assignment instead.
pub async fn suggest_series_candidates<'e, E: PgExecutor<'e>>(
    exec: E,
    normalized: &str,
    limit: i64,
) -> DbResult<Vec<SeriesCandidateRow>> {
    let rows = sqlx::query_as!(
        SeriesCandidateRow,
        "SELECT s.id AS series_id, s.canonical_title AS title, s.normalized_title, \
                s.content_type::text AS \"content_type!\", s.release_year, \
                (SELECT count(*) FROM series_sources ss WHERE ss.series_id = s.id) \
                    AS \"source_count!\", \
                GREATEST( \
                  similarity(s.normalized_title, $1), \
                  COALESCE((SELECT MAX(similarity(st.normalized, $1)) \
                            FROM series_titles st WHERE st.series_id = s.id), 0) \
                ) AS \"similarity!\" \
         FROM series s \
         WHERE s.normalized_title % $1 \
            OR EXISTS (SELECT 1 FROM series_titles st \
                       WHERE st.series_id = s.id AND st.normalized % $1) \
         ORDER BY 7 DESC \
         LIMIT $2",
        normalized,
        limit,
    )
    .fetch_all(exec)
    .await?;
    Ok(rows)
}

/// Record that a remote entry now resolves to `series_id`, removing it from the unmatched queue.
///
/// # Errors
/// [`crate::DbError::Sqlx`] only; an entry not loaded by any pull matches nothing and is
/// silently `Ok(())` — tolerable only because the operator reaches this through the queue the
/// entry itself populates.
pub async fn mark_remote_entry_matched<'e, E: PgExecutor<'e>>(
    exec: E,
    user_id: UserId,
    provider: &str,
    external_id: &str,
    series_id: SeriesId,
) -> DbResult<()> {
    sqlx::query!(
        "UPDATE sync_remote_entries SET series_id = $4 \
         WHERE user_id = $1 AND provider = $2 AND external_id = $3",
        user_id.as_uuid(),
        provider,
        external_id,
        series_id.as_uuid(),
    )
    .execute(exec)
    .await?;
    Ok(())
}
