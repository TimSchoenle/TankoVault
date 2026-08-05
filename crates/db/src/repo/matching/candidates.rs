//! The create-time candidate lookup: raw trigram matches for a normalized title.

use crate::error::DbResult;
use sqlx::{FromRow, PgExecutor};
use tankovault_domain::{ContentType, SeriesId, matching::Candidate};
use uuid::Uuid;

/// Find existing series whose canonical or alternative normalized titles are
/// trigram-similar to `normalized`, ordered by best similarity.
///
/// # Errors
/// [`crate::DbError::Sqlx`] only — no other variant is reachable. "No candidate cleared the
/// trigram threshold" is an empty `Vec`, which the canonicaliser reads as "this is a new
/// series", so a caller must not fold an `Err` into the same path: a failed lookup that looks
/// like no match creates a duplicate series instead of attaching a source.
pub async fn find_candidates<'e, E: PgExecutor<'e>>(
    exec: E,
    normalized: &str,
    limit: i64,
) -> DbResult<Vec<Candidate>> {
    #[derive(FromRow)]
    struct Row {
        id: Uuid,
        normalized_title: String,
        alt_titles: Vec<String>,
        content_type: ContentType,
        release_year: Option<i32>,
        sim: f32,
        tags: Vec<String>,
        authors: Vec<String>,
    }
    let rows = sqlx::query_as!(
        Row,
        // The two trigram predicates are a UNION of index-driven scans, never `a % $1 OR EXISTS
        // (… % $1)`: an `EXISTS` under `OR` cannot be pulled up into a semi-join, so the planner
        // falls back to a sequential scan of `series` and evaluates `similarity` per row — 54k
        // rows and ~450 ms measured, plus enough estimated cost to trigger 260 ms of pointless
        // JIT. The `LIMIT` is likewise applied *before* the array aggregates so those run for the
        // returned rows only, not for every row that cleared the threshold.
        //
        // Each branch carries its own `similarity`, and `max(…) GROUP BY id` is what combines
        // them. The obvious spelling — `GREATEST(similarity(s.normalized_title, $1), (SELECT
        // MAX(similarity(st.normalized, $1)) …))` over the matched ids — runs a correlated scan
        // of `series_titles` for **every** id the GIN indexes returned, before the `LIMIT`
        // discards all but `$2` of them; on a broad title that is thousands of scans to keep ten
        // rows, and it was 1.9–3.5 s in the slow-statement log.
        //
        // The scores are identical, not merely close, and the reason is worth keeping: `a % b`
        // *is* `similarity(a,b) >= threshold`. A title omitted here is one that failed `%`, so it
        // scored below the threshold — and the row only reached the union at all through a title
        // that scored at or above it, so it can never have been the `GREATEST`. `UNION ALL`,
        // because the `GROUP BY` already collapses the duplicates a `UNION` would have sorted for.
        "WITH matched AS ( \
           SELECT s.id, similarity(s.normalized_title, $1) AS sim \
             FROM series s WHERE s.normalized_title % $1 \
           UNION ALL \
           SELECT st.series_id, similarity(st.normalized, $1) \
             FROM series_titles st WHERE st.normalized % $1 \
         ), ranked AS ( \
           SELECT m.id, max(m.sim) AS sim \
           FROM matched m \
           GROUP BY m.id \
           ORDER BY sim DESC \
           LIMIT $2 \
         ) \
         SELECT s.id, s.normalized_title, \
                s.content_type AS \"content_type: ContentType\", s.release_year, \
                r.sim AS \"sim!\", \
                COALESCE((SELECT array_agg(st.normalized) FROM series_titles st \
                 WHERE st.series_id = r.id), '{}') AS \"alt_titles!\", \
                COALESCE((SELECT array_agg(t.name) FROM series_tags stg JOIN tags t ON t.id = stg.tag_id \
                 WHERE stg.series_id = r.id), '{}') AS \"tags!\", \
                COALESCE((SELECT array_agg(a.name) FROM series_authors sa JOIN authors a ON a.id = sa.author_id \
                 WHERE sa.series_id = r.id), '{}') AS \"authors!\" \
         FROM ranked r JOIN series s ON s.id = r.id \
         ORDER BY r.sim DESC",
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
            alt_normalized_titles: r.alt_titles,
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
///
/// # Errors
/// [`crate::DbError::Sqlx`] only — no other variant is reachable, and the caution on
/// [`find_candidates`] about treating `Err` as "no match" applies here too. An empty
/// `normalized` returns `Ok(empty)` with no round trip, and a title with no candidates is
/// **absent** from the result rather than present with an empty bucket.
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
        alt_titles: Vec<String>,
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
        // Same UNION-of-index-scans shape as `find_candidates`, scored the same way and for the
        // same reasons; see the comment there. Here it is per lateral iteration, so the
        // sequential scan it replaces was paid once per query title — and so was the correlated
        // `MAX(similarity(…))` the `max(…) GROUP BY` now replaces.
        "SELECT q.norm AS \"query_title!\", c.id, c.normalized_title, \
                c.content_type AS \"content_type: ContentType\", c.release_year, \
                c.sim AS \"sim!\", c.alt_titles AS \"alt_titles!\", \
                c.tags AS \"tags!\", c.authors AS \"authors!\" \
         FROM UNNEST($1::text[]) AS q(norm) \
         CROSS JOIN LATERAL ( \
           SELECT r.id, r.normalized_title, r.content_type, r.release_year, r.sim, \
                  COALESCE((SELECT array_agg(st.normalized) FROM series_titles st \
                   WHERE st.series_id = r.id), '{}') AS alt_titles, \
                  COALESCE((SELECT array_agg(t.name) FROM series_tags stg JOIN tags t ON t.id = stg.tag_id \
                   WHERE stg.series_id = r.id), '{}') AS tags, \
                  COALESCE((SELECT array_agg(a.name) FROM series_authors sa JOIN authors a ON a.id = sa.author_id \
                   WHERE sa.series_id = r.id), '{}') AS authors \
           FROM ( \
             SELECT s.id, s.normalized_title, s.content_type, s.release_year, m.sim \
             FROM ( SELECT u.id, max(u.sim) AS sim \
                    FROM ( SELECT s2.id, similarity(s2.normalized_title, q.norm) AS sim \
                             FROM series s2 WHERE s2.normalized_title % q.norm \
                           UNION ALL \
                           SELECT st2.series_id, similarity(st2.normalized, q.norm) \
                             FROM series_titles st2 WHERE st2.normalized % q.norm \
                         ) u \
                    GROUP BY u.id \
                    ORDER BY sim DESC \
                    LIMIT $2 \
                  ) m \
             JOIN series s ON s.id = m.id \
           ) r \
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
                alt_normalized_titles: row.alt_titles,
                content_type: row.content_type,
                release_year: row.release_year,
                tags: row.tags,
                authors: row.authors,
            });
        }
    }
    Ok(buckets)
}
