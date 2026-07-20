//! Candidate lookup for series canonicalisation (design §10, step 2).
//!
//! This layer returns raw trigram candidates; the scoring/decision logic lives in the
//! `matcher` crate (pure) so it is unit-testable without a database.

use crate::error::DbResult;
use tankovault_domain::{ContentType, SeriesId};
use sqlx::{FromRow, PgExecutor};
use std::str::FromStr;
use uuid::Uuid;

/// A trigram candidate for matching a new source's title to an existing series.
pub struct MatchCandidate {
    pub series_id: SeriesId,
    pub normalized_title: String,
    /// Best trigram similarity in `[0,1]` across the canonical + alternative titles.
    pub similarity: f32,
    pub content_type: ContentType,
    pub release_year: Option<i32>,
}

/// Find existing series whose canonical or alternative normalized titles are
/// trigram-similar to `normalized`, ordered by best similarity.
pub async fn find_candidates<'e, E: PgExecutor<'e>>(
    exec: E,
    normalized: &str,
    limit: i64,
) -> DbResult<Vec<MatchCandidate>> {
    #[derive(FromRow)]
    struct Row {
        id: Uuid,
        normalized_title: String,
        content_type: String,
        release_year: Option<i32>,
        sim: f32,
    }
    let rows: Vec<Row> = sqlx::query_as(
        "SELECT s.id, s.normalized_title, s.content_type::text AS content_type, s.release_year, \
                GREATEST( \
                  similarity(s.normalized_title, $1), \
                  COALESCE((SELECT MAX(similarity(st.normalized, $1)) \
                            FROM series_titles st WHERE st.series_id = s.id), 0) \
                ) AS sim \
         FROM series s \
         WHERE s.normalized_title % $1 \
            OR EXISTS (SELECT 1 FROM series_titles st \
                       WHERE st.series_id = s.id AND st.normalized % $1) \
         ORDER BY sim DESC \
         LIMIT $2",
    )
    .bind(normalized)
    .bind(limit)
    .fetch_all(exec)
    .await?;

    rows.into_iter()
        .map(|r| {
            Ok(MatchCandidate {
                series_id: SeriesId::from_uuid(r.id),
                normalized_title: r.normalized_title,
                similarity: r.sim,
                content_type: ContentType::from_str(&r.content_type)?,
                release_year: r.release_year,
            })
        })
        .collect()
}

/// Record an operator-review merge candidate (ambiguous confidence band).
pub async fn record_merge_candidate<'e, E: PgExecutor<'e>>(
    exec: E,
    series_id: SeriesId,
    candidate_id: SeriesId,
    score: f32,
    reason: &str,
) -> DbResult<()> {
    sqlx::query(
        "INSERT INTO merge_candidates (id, series_id, candidate_id, score, reason) \
         VALUES ($1,$2,$3,$4,$5)",
    )
    .bind(Uuid::now_v7())
    .bind(series_id.as_uuid())
    .bind(candidate_id.as_uuid())
    .bind(score)
    .bind(reason)
    .execute(exec)
    .await?;
    Ok(())
}

/// A pending merge candidate enriched with both series' display titles, for the operator
/// review queue (design §11 `GET /v1/admin/merge-candidates`).
#[derive(Debug, Clone, serde::Serialize)]
pub struct MergeCandidateView {
    pub id: Uuid,
    pub series_id: SeriesId,
    pub series_title: String,
    pub candidate_id: SeriesId,
    pub candidate_title: String,
    pub score: f32,
    pub reason: Option<String>,
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: time::OffsetDateTime,
}

/// List the open (unresolved) merge candidates, newest first.
pub async fn list_open_merge_candidates<'e, E: PgExecutor<'e>>(
    exec: E,
    limit: i64,
) -> DbResult<Vec<MergeCandidateView>> {
    #[derive(sqlx::FromRow)]
    struct Row {
        id: Uuid,
        series_id: Uuid,
        series_title: String,
        candidate_id: Uuid,
        candidate_title: String,
        score: f32,
        reason: Option<String>,
        created_at: time::OffsetDateTime,
    }
    let rows: Vec<Row> = sqlx::query_as(
        "SELECT mc.id, mc.series_id, s1.canonical_title AS series_title, \
                mc.candidate_id, s2.canonical_title AS candidate_title, \
                mc.score, mc.reason, mc.created_at \
         FROM merge_candidates mc \
         JOIN series s1 ON s1.id = mc.series_id \
         JOIN series s2 ON s2.id = mc.candidate_id \
         WHERE NOT mc.resolved \
         ORDER BY mc.created_at DESC \
         LIMIT $1",
    )
    .bind(limit)
    .fetch_all(exec)
    .await?;
    Ok(rows
        .into_iter()
        .map(|r| MergeCandidateView {
            id: r.id,
            series_id: SeriesId::from_uuid(r.series_id),
            series_title: r.series_title,
            candidate_id: SeriesId::from_uuid(r.candidate_id),
            candidate_title: r.candidate_title,
            score: r.score,
            reason: r.reason,
            created_at: r.created_at,
        })
        .collect())
}

/// Dismiss a merge candidate (operator judged the two works distinct) without merging.
pub async fn dismiss_merge_candidate<'e, E: PgExecutor<'e>>(
    exec: E,
    id: Uuid,
    resolved_by: Option<tankovault_domain::UserId>,
) -> DbResult<bool> {
    let result = sqlx::query(
        "UPDATE merge_candidates \
         SET resolved = true, resolved_by = $2, resolved_at = now() \
         WHERE id = $1 AND NOT resolved",
    )
    .bind(id)
    .bind(resolved_by.map(tankovault_domain::UserId::as_uuid))
    .execute(exec)
    .await?;
    Ok(result.rows_affected() > 0)
}

/// Transactionally merge `drop_id` into `keep_id` (design §10 operator merge): re-parent
/// the merged series' sources, union its titles and tags, migrate user watchlist/progress
/// and external mappings, resolve any related merge candidates, then delete it. All
/// child-table moves are idempotent (`ON CONFLICT`), and read-progress keeps the furthest
/// point.
pub async fn merge_series(
    pool: &sqlx::PgPool,
    keep_id: SeriesId,
    drop_id: SeriesId,
    actor: Option<tankovault_domain::UserId>,
) -> DbResult<()> {
    if keep_id == drop_id {
        return Err(crate::error::DbError::Conflict(
            "cannot merge a series into itself".to_owned(),
        ));
    }
    let mut tx = pool.begin().await?;
    let keep = keep_id.as_uuid();
    let drop = drop_id.as_uuid();

    // Both series must exist.
    let exists: i64 = sqlx::query_scalar("SELECT count(*) FROM series WHERE id = $1 OR id = $2")
        .bind(keep)
        .bind(drop)
        .fetch_one(&mut *tx)
        .await?;
    if exists < 2 {
        return Err(crate::error::DbError::NotFound);
    }

    // Sources move wholesale (their global (provider, path) uniqueness is preserved).
    sqlx::query("UPDATE series_sources SET series_id = $1 WHERE series_id = $2")
        .bind(keep)
        .bind(drop)
        .execute(&mut *tx)
        .await?;

    // The merged series' canonical title becomes an alternative title of the survivor.
    sqlx::query(
        "INSERT INTO series_titles (series_id, title, normalized) \
         SELECT $1, canonical_title, normalized_title FROM series WHERE id = $2 \
         ON CONFLICT (series_id, normalized) DO NOTHING",
    )
    .bind(keep)
    .bind(drop)
    .execute(&mut *tx)
    .await?;
    sqlx::query(
        "INSERT INTO series_titles (series_id, title, normalized) \
         SELECT $1, title, normalized FROM series_titles WHERE series_id = $2 \
         ON CONFLICT (series_id, normalized) DO NOTHING",
    )
    .bind(keep)
    .bind(drop)
    .execute(&mut *tx)
    .await?;

    sqlx::query(
        "INSERT INTO series_tags (series_id, tag_id) \
         SELECT $1, tag_id FROM series_tags WHERE series_id = $2 \
         ON CONFLICT DO NOTHING",
    )
    .bind(keep)
    .bind(drop)
    .execute(&mut *tx)
    .await?;

    sqlx::query(
        "INSERT INTO watchlist_entries (user_id, series_id, status, notify, added_at) \
         SELECT user_id, $1, status, notify, added_at FROM watchlist_entries WHERE series_id = $2 \
         ON CONFLICT (user_id, series_id) DO NOTHING",
    )
    .bind(keep)
    .bind(drop)
    .execute(&mut *tx)
    .await?;

    sqlx::query(
        "INSERT INTO read_progress (user_id, series_id, last_read_number, updated_at) \
         SELECT user_id, $1, last_read_number, updated_at FROM read_progress WHERE series_id = $2 \
         ON CONFLICT (user_id, series_id) DO UPDATE \
            SET last_read_number = GREATEST(read_progress.last_read_number, EXCLUDED.last_read_number), \
                updated_at = now()",
    )
    .bind(keep)
    .bind(drop)
    .execute(&mut *tx)
    .await?;

    sqlx::query(
        "INSERT INTO sync_mappings (series_id, provider, external_id) \
         SELECT $1, provider, external_id FROM sync_mappings WHERE series_id = $2 \
         ON CONFLICT (series_id, provider) DO NOTHING",
    )
    .bind(keep)
    .bind(drop)
    .execute(&mut *tx)
    .await?;

    // Resolve every open candidate that referenced the vanishing series.
    sqlx::query(
        "UPDATE merge_candidates \
         SET resolved = true, resolved_by = $2, resolved_at = now() \
         WHERE (series_id = $1 OR candidate_id = $1) AND NOT resolved",
    )
    .bind(drop)
    .bind(actor.map(tankovault_domain::UserId::as_uuid))
    .execute(&mut *tx)
    .await?;

    // Delete the merged series; residual child rows cascade away.
    sqlx::query("DELETE FROM series WHERE id = $1")
        .bind(drop)
        .execute(&mut *tx)
        .await?;

    tx.commit().await?;
    Ok(())
}
