//! The create-time candidate lookup: raw trigram matches for a normalized title.

use super::MAX_KEY_FANOUT;
use crate::error::DbResult;
use sqlx::{FromRow, PgExecutor};
use tankovault_domain::{ContentType, SeriesId, matching::Candidate};
use uuid::Uuid;

/// Finds existing series whose canonical or alternative normalized titles are trigram-similar to
/// `normalized`, ordered by best similarity.
///
/// An *alternative* title held by more than `MAX_KEY_FANOUT` series is not searched: a name that
/// many works answer to identifies none of them, and shortlisting on one lets a single row
/// absorb unrelated sources. Canonical titles are not capped, since a series keeps its own name
/// however many others share it.
///
/// An empty result means no candidate cleared the threshold, which the canonicaliser reads as a
/// new series, so a caller must not fold an error into it or a failed lookup creates a
/// duplicate.
///
/// # Errors
/// [`crate::DbError::Sqlx`] only.
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
        source_titles: Vec<String>,
        content_type: ContentType,
        release_year: Option<i32>,
        sim: f32,
        tags: Vec<String>,
        authors: Vec<String>,
    }
    let rows = sqlx::query_as!(
        Row,
        // The trigram predicates are a UNION of index-driven scans, never `a % $1 OR EXISTS
        // (… % $1)`: an `EXISTS` under `OR` cannot become a semi-join, so the planner would scan
        // all of `series` and evaluate `similarity` per row. The `LIMIT` applies before the array
        // aggregates, so those run for the returned rows only.
        //
        // Each branch carries its own `similarity`, combined by `max(…) GROUP BY id`. A
        // `GREATEST(…, (SELECT MAX(similarity(…)) …))` over the matched ids would run a
        // correlated scan of `series_titles` for every id the GIN indexes returned, before the
        // `LIMIT` discards all but `$2` of them.
        //
        // The scores equal the `GREATEST` form exactly: `a % b` *is* `similarity(a,b) >=
        // threshold`, so a title omitted here scored below a title that brought the row into
        // the union. `UNION ALL`, because the `GROUP BY` already collapses duplicates.
        //
        // The alias branch drops keys held by more than `MAX_KEY_FANOUT` series. The window
        // runs over the rows the GIN index returned, so it costs no extra scan, and on an exact
        // hit on a junk key every holder is in that set, so the count is the true fan-out.
        //
        // The third branch is another source's listing title, by compact-key equality: what a
        // mirror lists a work under is what its siblings list it under, whatever AniList renamed
        // the series to. It is capped on distinct *series*, hence the `GROUP BY` before the
        // window. Its similarity is 1, since the keys are equal and a lower number could let the
        // `LIMIT` drop the one candidate the title identifies.
        "WITH matched AS ( \
           SELECT s.id, similarity(s.normalized_title, $1) AS sim \
             FROM series s WHERE s.normalized_title % $1 \
           UNION ALL \
           SELECT a.series_id, a.sim FROM ( \
             SELECT st.series_id, similarity(st.normalized, $1) AS sim, \
                    count(*) OVER (PARTITION BY st.normalized) AS fanout \
               FROM series_titles st WHERE st.normalized % $1 \
           ) a WHERE a.fanout <= $3 \
           UNION ALL \
           SELECT l.series_id, l.sim FROM ( \
             SELECT ss.series_id, 1::real AS sim, count(*) OVER () AS fanout \
               FROM series_sources ss \
              WHERE replace(ss.provider_title_normalized, ' ', '') = replace($1, ' ', '') \
              GROUP BY ss.series_id \
           ) l WHERE l.fanout <= $3 \
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
                COALESCE((SELECT array_agg(DISTINCT ss.provider_title_normalized) \
                 FROM series_sources ss WHERE ss.series_id = r.id \
                  AND ss.provider_title_normalized <> ''), '{}') AS \"source_titles!\", \
                COALESCE((SELECT array_agg(t.name) FROM series_tags stg JOIN tags t ON t.id = stg.tag_id \
                 WHERE stg.series_id = r.id), '{}') AS \"tags!\", \
                COALESCE((SELECT array_agg(a.name) FROM series_authors sa JOIN authors a ON a.id = sa.author_id \
                 WHERE sa.series_id = r.id), '{}') AS \"authors!\" \
         FROM ranked r JOIN series s ON s.id = r.id \
         ORDER BY r.sim DESC",
        normalized,
        limit,
        MAX_KEY_FANOUT,
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
            source_titles: r.source_titles,
            content_type: r.content_type,
            release_year: r.release_year,
            tags: r.tags,
            authors: r.authors,
        })
        .collect())
}

/// Returns each of several query titles with its own top-`limit` candidates, as
/// [`find_candidates`] does for one, in **one** round trip.
///
/// An external-sync entry carries a family of titles (romaji, english, native and every
/// synonym), and attaches when any of them matches; one title at a time would cost 3 to 8
/// trigram scans per remote entry. The lateral join applies `LIMIT` per title, so a title with
/// many weak candidates cannot crowd out another's strong one, and the scores equal the
/// per-title path.
///
/// An empty `normalized` returns empty with no round trip. The caution on [`find_candidates`]
/// about reading an error as no match applies here too.
///
/// # Errors
/// [`crate::DbError::Sqlx`] only.
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
        source_titles: Vec<String>,
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
        // The UNION-of-index-scans shape of `find_candidates`, scored the same way; see the
        // comment there. Here it runs once per lateral iteration, so each query title avoids
        // its own sequential scan.
        "SELECT q.norm AS \"query_title!\", c.id, c.normalized_title, \
                c.content_type AS \"content_type: ContentType\", c.release_year, \
                c.sim AS \"sim!\", c.alt_titles AS \"alt_titles!\", \
                c.source_titles AS \"source_titles!\", \
                c.tags AS \"tags!\", c.authors AS \"authors!\" \
         FROM UNNEST($1::text[]) AS q(norm) \
         CROSS JOIN LATERAL ( \
           SELECT r.id, r.normalized_title, r.content_type, r.release_year, r.sim, \
                  COALESCE((SELECT array_agg(st.normalized) FROM series_titles st \
                   WHERE st.series_id = r.id), '{}') AS alt_titles, \
                  COALESCE((SELECT array_agg(DISTINCT ss.provider_title_normalized) \
                   FROM series_sources ss WHERE ss.series_id = r.id \
                    AND ss.provider_title_normalized <> ''), '{}') AS source_titles, \
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
                           SELECT a.series_id, a.sim FROM ( \
                             SELECT st2.series_id, similarity(st2.normalized, q.norm) AS sim, \
                                    count(*) OVER (PARTITION BY st2.normalized) AS fanout \
                               FROM series_titles st2 WHERE st2.normalized % q.norm \
                           ) a WHERE a.fanout <= $3 \
                           UNION ALL \
                           SELECT l.series_id, l.sim FROM ( \
                             SELECT ss2.series_id, 1::real AS sim, count(*) OVER () AS fanout \
                               FROM series_sources ss2 \
                              WHERE replace(ss2.provider_title_normalized, ' ', '') \
                                    = replace(q.norm, ' ', '') \
                              GROUP BY ss2.series_id \
                           ) l WHERE l.fanout <= $3 \
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
        MAX_KEY_FANOUT,
    )
    .fetch_all(exec)
    .await?;

    // Keep the caller's title order and a bucket for every requested title, so a title with no
    // candidates is reported as an empty list.
    let mut buckets: Vec<(String, Vec<Candidate>)> =
        normalized.iter().map(|t| (t.clone(), Vec::new())).collect();
    for row in rows {
        if let Some((_, bucket)) = buckets.iter_mut().find(|(t, _)| *t == row.query_title) {
            bucket.push(Candidate {
                series_id: SeriesId::from_uuid(row.id),
                normalized_title: row.normalized_title,
                similarity: row.sim,
                alt_normalized_titles: row.alt_titles,
                source_titles: row.source_titles,
                content_type: row.content_type,
                release_year: row.release_year,
                tags: row.tags,
                authors: row.authors,
            });
        }
    }
    Ok(buckets)
}
