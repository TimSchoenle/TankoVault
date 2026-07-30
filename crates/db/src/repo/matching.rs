//! Candidate lookup for series canonicalisation (design §10, step 2), plus the merge-candidate
//! queue an ambiguous match feeds.
//!
//! This layer returns raw trigram candidates and performs whatever the caller's
//! [`Canonicaliser`](tankovault_domain::matching::Canonicaliser) decides; the scoring and the
//! thresholds live above it (`tankovault_matcher` and `tankovault_config::MatchingConfig`), so
//! it is unit-testable without a database and this crate links no scorer.
//!
//! The candidate type is [`tankovault_domain::matching::Candidate`] itself rather than a row
//! struct plus a `From` impl. That conversion used to be written out by hand, field for field,
//! in **two** places — the worker's ingest canonicalisation and `services/sync`'s remote-entry
//! resolution — so adding a field to it silently dropped that signal from one of the two paths
//! that decide whether two series are the same. ARCH-16 step 1 deduplicated the conversion;
//! step 3 removed the need for one at all.

use crate::error::DbResult;
use sqlx::{FromRow, PgExecutor};
use tankovault_domain::{ContentType, SeriesId, matching::Candidate};
use uuid::Uuid;

/// Find existing series whose canonical or alternative normalized titles are
/// trigram-similar to `normalized`, ordered by best similarity.
pub async fn find_candidates<'e, E: PgExecutor<'e>>(
    exec: E,
    normalized: &str,
    limit: i64,
) -> DbResult<Vec<Candidate>> {
    #[derive(FromRow)]
    struct Row {
        id: Uuid,
        normalized_title: String,
        content_type: ContentType,
        release_year: Option<i32>,
        sim: f32,
        tags: Vec<String>,
        authors: Vec<String>,
    }
    let rows = sqlx::query_as!(
        Row,
        "SELECT s.id, s.normalized_title, s.content_type AS \"content_type: ContentType\", s.release_year, \
                GREATEST( \
                  similarity(s.normalized_title, $1), \
                  COALESCE((SELECT MAX(similarity(st.normalized, $1)) \
                            FROM series_titles st WHERE st.series_id = s.id), 0) \
                ) AS \"sim!\", \
                COALESCE((SELECT array_agg(t.name) FROM series_tags stg JOIN tags t ON t.id = stg.tag_id \
                 WHERE stg.series_id = s.id), '{}') AS \"tags!\", \
                COALESCE((SELECT array_agg(a.name) FROM series_authors sa JOIN authors a ON a.id = sa.author_id \
                 WHERE sa.series_id = s.id), '{}') AS \"authors!\" \
         FROM series s \
         WHERE s.normalized_title % $1 \
            OR EXISTS (SELECT 1 FROM series_titles st \
                       WHERE st.series_id = s.id AND st.normalized % $1) \
         ORDER BY 5 DESC \
         LIMIT $2",
        normalized,
        limit,
    )
    .fetch_all(exec)
    .await?;

    Ok(rows
        .into_iter()
        .map(|r| Candidate {
            series_id: SeriesId::from_uuid(r.id),
            normalized_title: r.normalized_title,
            similarity: r.sim,
            content_type: r.content_type,
            release_year: r.release_year,
            tags: r.tags,
            authors: r.authors,
        })
        .collect())
}

/// As [`find_candidates`], but for several query titles in **one** round trip: returns each
/// title paired with its own top-`limit` candidates.
///
/// # Why this exists
///
/// External-sync entries carry a whole family of titles (romaji, english, native, plus every
/// synonym), and the engine has to score each of them to attach an entry when *any* title
/// matches. Doing that one title at a time meant K sequential trigram scans per remote entry —
/// 3–8 for a typical `AniList` row, so 1 500–4 000 scans for a 500-entry library (PERF-13).
///
/// The lateral join is load-bearing: `LIMIT` has to apply *per title*, exactly as K separate
/// queries did, so a title with many weak candidates cannot crowd out another title's strong
/// one. Similarity is still computed against the same expression, so scores are identical to
/// the per-title path.
pub async fn find_candidates_multi<'e, E: PgExecutor<'e>>(
    exec: E,
    normalized: &[String],
    limit: i64,
) -> DbResult<Vec<(String, Vec<Candidate>)>> {
    #[derive(FromRow)]
    struct Row {
        query_title: String,
        id: Uuid,
        normalized_title: String,
        content_type: ContentType,
        release_year: Option<i32>,
        sim: f32,
        tags: Vec<String>,
        authors: Vec<String>,
    }
    if normalized.is_empty() {
        return Ok(Vec::new());
    }
    let rows = sqlx::query_as!(
        Row,
        "SELECT q.norm AS \"query_title!\", c.id, c.normalized_title, \
                c.content_type AS \"content_type: ContentType\", c.release_year, \
                c.sim AS \"sim!\", c.tags AS \"tags!\", c.authors AS \"authors!\" \
         FROM UNNEST($1::text[]) AS q(norm) \
         CROSS JOIN LATERAL ( \
           SELECT s.id, s.normalized_title, s.content_type, s.release_year, \
                  GREATEST( \
                    similarity(s.normalized_title, q.norm), \
                    COALESCE((SELECT MAX(similarity(st.normalized, q.norm)) \
                              FROM series_titles st WHERE st.series_id = s.id), 0) \
                  ) AS sim, \
                  COALESCE((SELECT array_agg(t.name) FROM series_tags stg JOIN tags t ON t.id = stg.tag_id \
                   WHERE stg.series_id = s.id), '{}') AS tags, \
                  COALESCE((SELECT array_agg(a.name) FROM series_authors sa JOIN authors a ON a.id = sa.author_id \
                   WHERE sa.series_id = s.id), '{}') AS authors \
           FROM series s \
           WHERE s.normalized_title % q.norm \
              OR EXISTS (SELECT 1 FROM series_titles st \
                         WHERE st.series_id = s.id AND st.normalized % q.norm) \
           ORDER BY sim DESC \
           LIMIT $2 \
         ) c",
        normalized,
        limit,
    )
    .fetch_all(exec)
    .await?;

    // Preserve the caller's title order, and keep a bucket for every requested title so a title
    // with no candidates is still reported (as an empty list) rather than silently dropped.
    let mut buckets: Vec<(String, Vec<Candidate>)> =
        normalized.iter().map(|t| (t.clone(), Vec::new())).collect();
    for row in rows {
        if let Some((_, bucket)) = buckets.iter_mut().find(|(t, _)| *t == row.query_title) {
            bucket.push(Candidate {
                series_id: SeriesId::from_uuid(row.id),
                normalized_title: row.normalized_title,
                similarity: row.sim,
                content_type: row.content_type,
                release_year: row.release_year,
                tags: row.tags,
                authors: row.authors,
            });
        }
    }
    Ok(buckets)
}

/// Record an operator-review merge candidate (ambiguous confidence band).
pub async fn record_merge_candidate<'e, E: PgExecutor<'e>>(
    exec: E,
    series_id: SeriesId,
    candidate_id: SeriesId,
    score: f32,
    reason: &str,
) -> DbResult<()> {
    sqlx::query!(
        "INSERT INTO merge_candidates (id, series_id, candidate_id, score, reason) \
         VALUES ($1,$2,$3,$4,$5)",
        Uuid::now_v7(),
        series_id.as_uuid(),
        candidate_id.as_uuid(),
        score,
        reason,
    )
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
    let rows = sqlx::query_as!(
        Row,
        "SELECT mc.id, mc.series_id, s1.canonical_title AS series_title, \
                mc.candidate_id, s2.canonical_title AS candidate_title, \
                mc.score, mc.reason, mc.created_at \
         FROM merge_candidates mc \
         JOIN series s1 ON s1.id = mc.series_id \
         JOIN series s2 ON s2.id = mc.candidate_id \
         WHERE NOT mc.resolved \
         ORDER BY mc.created_at DESC \
         LIMIT $1",
        limit,
    )
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
    let result = sqlx::query!(
        "UPDATE merge_candidates \
         SET resolved = true, resolved_by = $2, resolved_at = now() \
         WHERE id = $1 AND NOT resolved",
        id,
        resolved_by.map(tankovault_domain::UserId::as_uuid),
    )
    .execute(exec)
    .await?;
    Ok(result.rows_affected() > 0)
}

/// Transactionally merge `drop_id` into `keep_id` (design §10 operator merge): re-parent
/// the merged series' sources, union its titles and tags, migrate user watchlist/progress
/// and external mappings, resolve any related merge candidates, then delete it. All
/// child-table moves are idempotent (`ON CONFLICT`), and read-progress keeps the furthest
/// point.
///
/// # The read-progress merge
///
/// Both frontiers take the furthest of the two rows, and the part frontier is then dropped if the
/// merged **whole** frontier covers it — the same staleness rule
/// [`progress_set`](crate::repo::tracking::progress_set) and
/// [`progress_mark_read`](crate::repo::tracking::progress_mark_read) apply (`floor(part) <=
/// whole`), so all three write paths uphold §A.1 identically.
///
/// The condition used to be `whole >= floor(part) **AND** part = 0`, which only ever cleared the
/// frontier when there was no part frontier at all — and the `>= floor(part)` half, the actual
/// staleness test, could therefore never fire. Merging a user who was at whole `6` on the survivor
/// with their own row at part `4.5` on the absorbed series produced `(6, 4.5)`, which §A.1 forbids.
/// It changed no answer (`covers` and every read model already treat `4.5` as read at `floor(4.5)
/// <= 6`) and the next `progress_set` cleared it, which is why it was invisible; the invariant is
/// documented, so a read model is entitled to trust it.
///
/// # Merge candidates
///
/// The `UPDATE merge_candidates` below is belt-and-braces: both of that table's series columns are
/// `ON DELETE CASCADE`, so every row naming `drop_id` is removed by the `DELETE FROM series` that
/// follows regardless. What matters — and what `repo_matching.rs` asserts — is that no *unresolved*
/// candidate is left naming a series that no longer exists, because
/// [`list_open_merge_candidates`] inner-joins both sides and such a row would silently vanish from
/// the operator's queue while staying open in the table.
// A straight-line sequence of per-table union inserts reads more clearly as one function
// than split across arbitrary helpers just to dodge the line-count lint.
#[allow(clippy::too_many_lines)]
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
    let exists = sqlx::query_scalar!(
        "SELECT count(*) AS \"count!\" FROM series WHERE id = $1 OR id = $2",
        keep,
        drop,
    )
    .fetch_one(&mut *tx)
    .await?;
    if exists < 2 {
        return Err(crate::error::DbError::NotFound);
    }

    // Sources move wholesale (their global (provider, path) uniqueness is preserved).
    sqlx::query!(
        "UPDATE series_sources SET series_id = $1 WHERE series_id = $2",
        keep,
        drop,
    )
    .execute(&mut *tx)
    .await?;

    // The merged series' canonical title becomes an alternative title of the survivor.
    sqlx::query!(
        "INSERT INTO series_titles (series_id, title, normalized) \
         SELECT $1, canonical_title, normalized_title FROM series WHERE id = $2 \
         ON CONFLICT (series_id, normalized) DO NOTHING",
        keep,
        drop,
    )
    .execute(&mut *tx)
    .await?;
    sqlx::query!(
        "INSERT INTO series_titles (series_id, title, normalized) \
         SELECT $1, title, normalized FROM series_titles WHERE series_id = $2 \
         ON CONFLICT (series_id, normalized) DO NOTHING",
        keep,
        drop,
    )
    .execute(&mut *tx)
    .await?;

    sqlx::query!(
        "INSERT INTO series_tags (series_id, tag_id) \
         SELECT $1, tag_id FROM series_tags WHERE series_id = $2 \
         ON CONFLICT DO NOTHING",
        keep,
        drop,
    )
    .execute(&mut *tx)
    .await?;

    sqlx::query!(
        "INSERT INTO series_authors (series_id, author_id) \
         SELECT $1, author_id FROM series_authors WHERE series_id = $2 \
         ON CONFLICT DO NOTHING",
        keep,
        drop,
    )
    .execute(&mut *tx)
    .await?;

    sqlx::query!(
        "INSERT INTO watchlist_entries (user_id, series_id, status, notify, added_at) \
         SELECT user_id, $1, status, notify, added_at FROM watchlist_entries WHERE series_id = $2 \
         ON CONFLICT (user_id, series_id) DO NOTHING",
        keep,
        drop,
    )
    .execute(&mut *tx)
    .await?;

    sqlx::query!(
        "INSERT INTO read_progress \
            (user_id, series_id, last_read_whole_number, last_read_part_number, updated_at) \
         SELECT user_id, $1, last_read_whole_number, last_read_part_number, updated_at \
            FROM read_progress WHERE series_id = $2 \
         ON CONFLICT (user_id, series_id) DO UPDATE \
            SET last_read_whole_number = \
                    GREATEST(read_progress.last_read_whole_number, EXCLUDED.last_read_whole_number), \
                last_read_part_number = CASE \
                    WHEN floor(GREATEST(COALESCE(read_progress.last_read_part_number, 0), \
                                        COALESCE(EXCLUDED.last_read_part_number, 0))) \
                         <= GREATEST(read_progress.last_read_whole_number, \
                                     EXCLUDED.last_read_whole_number) \
                    THEN NULL \
                    ELSE GREATEST(COALESCE(read_progress.last_read_part_number, 0), \
                                  COALESCE(EXCLUDED.last_read_part_number, 0)) END, \
                updated_at = now()",
        keep,
        drop,
    )
    .execute(&mut *tx)
    .await?;

    sqlx::query!(
        "INSERT INTO sync_mappings (series_id, provider, external_id) \
         SELECT $1, provider, external_id FROM sync_mappings WHERE series_id = $2 \
         ON CONFLICT (series_id, provider) DO NOTHING",
        keep,
        drop,
    )
    .execute(&mut *tx)
    .await?;

    // Resolve every open candidate that referenced the vanishing series.
    sqlx::query!(
        "UPDATE merge_candidates \
         SET resolved = true, resolved_by = $2, resolved_at = now() \
         WHERE (series_id = $1 OR candidate_id = $1) AND NOT resolved",
        drop,
        actor.map(tankovault_domain::UserId::as_uuid),
    )
    .execute(&mut *tx)
    .await?;

    // Delete the merged series; residual child rows cascade away.
    sqlx::query!("DELETE FROM series WHERE id = $1", drop)
        .execute(&mut *tx)
        .await?;

    tx.commit().await?;
    Ok(())
}
