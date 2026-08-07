//! The standing duplicate sweep and the pair-level facts an operator decision is scored on.

use crate::error::DbResult;
use sqlx::{FromRow, PgExecutor};
use tankovault_domain::{ContentType, SeriesId};
use uuid::Uuid;

/// A pair of series worth re-scoring, in canonical id order.
pub type DuplicatePair = (SeriesId, SeriesId);

/// How many distinct series may share one compact title key before [`find_duplicate_pairs`]
/// stops blocking on it.
///
use super::MAX_KEY_FANOUT;

/// Every pair of *existing* series whose titles collide on the whitespace-insensitive key and
/// that the sweep has not already recorded a verdict for.
///
/// # Why blocking on the compact key
///
/// A duplicate sweep cannot score 26 418 × 26 418 pairs, so it needs a cheap predicate that
/// admits every pair worth looking at and almost nothing else. The compact key — the normalized
/// title with its spaces removed — is exactly that: it is an equality (so it is an index lookup,
/// not a similarity scan), and the class of duplicate it admits is the one the create-time
/// matcher is worst at, because a missing space between two HTML elements destroys both the
/// trigram score and the token-set ratio at once.
///
/// Three collisions count, and they are three different provider behaviours: two canonical
/// titles agreeing, one series' canonical title agreeing with another's *alternative* title
/// (the same work listed under its romaji name on one site and its english name on another),
/// and two series sharing an alternative title.
///
/// The result is only a *shortlist*. Every pair is re-scored in full — with the tags, authors,
/// release years and alternative titles that the create-time path never had — before anything
/// is queued, let alone merged.
///
/// # Why an over-shared key is dropped
///
/// A blocking key is only cheap while it is selective, and equality gives no protection against
/// one key being held by thousands of series: that is not a shortlist, it is an all-pairs clique
/// with a `LIMIT` in front of it. Six such keys — `Status`, `Alternative`, `Genres`, `View`,
/// `Rating`, `Release`, scraped as alternative titles out of a summary block's labels — took the
/// live shortlist from 4 352 pairs to 15 176 110, and buried a byte-identical duplicate at
/// position 8.9 million of a list the sweep reads 500 of. [`MAX_KEY_FANOUT`] is the ceiling, and
/// the justification is independent of what produced the key: a title thousands of series answer
/// to does not identify any of them.
///
/// # Why already-recorded pairs are excluded entirely
///
/// This is the *new-pair* shortlist, and it is ordered with a `LIMIT`, so it needs a progress
/// guarantee. Excluding only resolved pairs did not give it one — a pair queued for review is
/// still open, so it came back in the same prefix on the next run, forever. Every pair with a
/// row of any kind is now this function's business no longer: the open ones are re-scored by
/// [`open_merge_pairs`] and the scorer-distinct ones by [`distinct_merge_pairs`], each on its
/// own budget and least-recently-scored first.
///
/// # Errors
/// [`crate::DbError::Sqlx`] only.
pub async fn find_duplicate_pairs<'e, E: PgExecutor<'e>>(
    exec: E,
    limit: i64,
) -> DbResult<Vec<DuplicatePair>> {
    #[derive(FromRow)]
    struct Row {
        lo: Uuid,
        hi: Uuid,
    }
    let rows = sqlx::query_as!(
        Row,
        // `UNION` over (key, series_id) already deduplicates, so `count(*)` is the distinct
        // series count without a second pass.
        "WITH over_shared AS ( \
           SELECT key FROM ( \
             SELECT replace(normalized_title, ' ', '') AS key, id AS series_id \
               FROM series WHERE normalized_title <> '' \
             UNION \
             SELECT replace(normalized, ' ', '') AS key, series_id \
               FROM series_titles WHERE normalized <> '' \
           ) k \
           GROUP BY key HAVING count(*) > $2 \
         ), by_canonical AS ( \
           SELECT a.id AS lo, b.id AS hi \
           FROM series a JOIN series b \
             ON replace(a.normalized_title, ' ', '') = replace(b.normalized_title, ' ', '') \
            AND a.id < b.id \
           WHERE a.normalized_title <> '' \
             AND replace(a.normalized_title, ' ', '') NOT IN (SELECT key FROM over_shared) \
         ), by_alias AS ( \
           SELECT LEAST(s.id, st.series_id) AS lo, GREATEST(s.id, st.series_id) AS hi \
           FROM series s JOIN series_titles st \
             ON replace(st.normalized, ' ', '') = replace(s.normalized_title, ' ', '') \
           WHERE st.series_id <> s.id AND s.normalized_title <> '' \
             AND replace(s.normalized_title, ' ', '') NOT IN (SELECT key FROM over_shared) \
         ), by_shared_alias AS ( \
           SELECT x.series_id AS lo, y.series_id AS hi \
           FROM series_titles x JOIN series_titles y \
             ON replace(x.normalized, ' ', '') = replace(y.normalized, ' ', '') \
            AND x.series_id < y.series_id \
           WHERE x.normalized <> '' \
             AND replace(x.normalized, ' ', '') NOT IN (SELECT key FROM over_shared) \
         ), pairs AS ( \
           SELECT lo, hi FROM by_canonical \
           UNION SELECT lo, hi FROM by_alias \
           UNION SELECT lo, hi FROM by_shared_alias \
         ) \
         SELECT p.lo AS \"lo!\", p.hi AS \"hi!\" FROM pairs p \
         WHERE NOT EXISTS ( \
           SELECT 1 FROM merge_candidates mc \
           WHERE mc.series_id = p.lo AND mc.candidate_id = p.hi \
         ) \
         ORDER BY p.lo, p.hi \
         LIMIT $1",
        limit,
        MAX_KEY_FANOUT,
    )
    .fetch_all(exec)
    .await?;
    Ok(rows
        .into_iter()
        .map(|r| (SeriesId::from_uuid(r.lo), SeriesId::from_uuid(r.hi)))
        .collect())
}

/// Everything the scorer and the survivor choice need about one series.
///
/// Assembled for *both* sides of a pair, which is what makes the standing sweep strictly better
/// informed than the create-time path: `resolve_canonical_series` scores a bare
/// [`SeriesUpsert`](crate::repo::catalog::SeriesUpsert) whose tags and authors have not been
/// written yet, so those bonuses can never fire there. Here they always can.
#[derive(Debug, Clone)]
pub struct SeriesMatchFacts {
    pub series_id: SeriesId,
    pub canonical_title: String,
    pub normalized_title: String,
    pub alt_normalized_titles: Vec<String>,
    pub content_type: ContentType,
    pub release_year: Option<i32>,
    pub tags: Vec<String>,
    pub authors: Vec<String>,
    pub source_count: i64,
    pub chapter_count: i64,
    pub watcher_count: i64,
}

impl SeriesMatchFacts {
    /// How much of the catalogue this series carries, most significant first.
    ///
    /// The merge survivor is chosen by this rather than by which row is older or which id sorts
    /// lower. Both sides' data is preserved either way — the merge unions everything — but the
    /// absorbed id *stops existing*, and every bookmark, notification and external tracker
    /// mapping that already names it breaks. Keeping the series with more sources, chapters and
    /// watchers is keeping the id more of the world already points at.
    #[must_use]
    pub const fn weight(&self) -> (i64, i64, i64) {
        (self.source_count, self.chapter_count, self.watcher_count)
    }
}

/// Load [`SeriesMatchFacts`] for the given series, in one round trip. Unknown ids are absent
/// from the result rather than an error.
///
/// # Errors
/// [`crate::DbError::Sqlx`] only.
pub async fn series_match_facts<'e, E: PgExecutor<'e>>(
    exec: E,
    ids: &[SeriesId],
) -> DbResult<Vec<SeriesMatchFacts>> {
    #[derive(FromRow)]
    struct Row {
        id: Uuid,
        canonical_title: String,
        normalized_title: String,
        alt_titles: Vec<String>,
        content_type: ContentType,
        release_year: Option<i32>,
        tags: Vec<String>,
        authors: Vec<String>,
        source_count: i64,
        chapter_count: i64,
        watcher_count: i64,
    }
    if ids.is_empty() {
        return Ok(Vec::new());
    }
    let uuids: Vec<Uuid> = ids.iter().map(|id| id.as_uuid()).collect();
    let rows = sqlx::query_as!(
        Row,
        "SELECT s.id, s.canonical_title, s.normalized_title, \
                s.content_type AS \"content_type: ContentType\", s.release_year, \
                COALESCE((SELECT array_agg(st.normalized) FROM series_titles st \
                 WHERE st.series_id = s.id), '{}') AS \"alt_titles!\", \
                COALESCE((SELECT array_agg(t.name) FROM series_tags stg JOIN tags t ON t.id = stg.tag_id \
                 WHERE stg.series_id = s.id), '{}') AS \"tags!\", \
                COALESCE((SELECT array_agg(a.name) FROM series_authors sa JOIN authors a ON a.id = sa.author_id \
                 WHERE sa.series_id = s.id), '{}') AS \"authors!\", \
                (SELECT count(*) FROM series_sources ss WHERE ss.series_id = s.id) AS \"source_count!\", \
                COALESCE((SELECT sum(ss.chapter_count) FROM series_sources ss \
                          WHERE ss.series_id = s.id), 0) AS \"chapter_count!\", \
                (SELECT count(*) FROM watchlist_entries w WHERE w.series_id = s.id) AS \"watcher_count!\" \
         FROM series s WHERE s.id = ANY($1)",
        &uuids,
    )
    .fetch_all(exec)
    .await?;

    Ok(rows
        .into_iter()
        .map(|r| SeriesMatchFacts {
            series_id: SeriesId::from_uuid(r.id),
            canonical_title: r.canonical_title,
            normalized_title: r.normalized_title,
            alt_normalized_titles: r.alt_titles,
            content_type: r.content_type,
            release_year: r.release_year,
            tags: r.tags,
            authors: r.authors,
            source_count: r.source_count,
            chapter_count: r.chapter_count,
            watcher_count: r.watcher_count,
        })
        .collect())
}

/// The trigram similarity of each given pair, computed exactly as [`find_candidates`](super::find_candidates) computes
/// it: canonical against canonical, and each side's alternative titles against the other's
/// canonical title, taking the best.
///
/// # Why the sweep needs this
///
/// [`SeriesMatchFacts`] carries everything about *one* series, and the scorer's base is the
/// strongest of three views — of which one, the trigram score, is a property of the *pair* and
/// comes from the database. Scoring a pair with that term set to zero would systematically
/// under-score it, and the sweep withdraws queue rows that fall below the review floor: a pair
/// whose stored score came from a trigram match would be re-scored lower on no new evidence and
/// quietly dropped out of the queue. Pairs are batched into one round trip so this costs the
/// sweep a query, not a query per pair.
///
/// Pairs whose ids do not both exist are absent from the result.
///
/// # Errors
/// [`crate::DbError::Sqlx`] only.
pub async fn pair_similarities<'e, E: PgExecutor<'e>>(
    exec: E,
    pairs: &[DuplicatePair],
) -> DbResult<Vec<(DuplicatePair, f32)>> {
    #[derive(FromRow)]
    struct Row {
        lo: Uuid,
        hi: Uuid,
        sim: f32,
    }
    if pairs.is_empty() {
        return Ok(Vec::new());
    }
    let los: Vec<Uuid> = pairs.iter().map(|(a, _)| a.as_uuid()).collect();
    let his: Vec<Uuid> = pairs.iter().map(|(_, b)| b.as_uuid()).collect();
    let rows = sqlx::query_as!(
        Row,
        "SELECT p.lo AS \"lo!\", p.hi AS \"hi!\", \
                GREATEST( \
                  similarity(a.normalized_title, b.normalized_title), \
                  COALESCE((SELECT MAX(similarity(x.normalized, b.normalized_title)) \
                            FROM series_titles x WHERE x.series_id = a.id), 0), \
                  COALESCE((SELECT MAX(similarity(y.normalized, a.normalized_title)) \
                            FROM series_titles y WHERE y.series_id = b.id), 0) \
                ) AS \"sim!\" \
         FROM UNNEST($1::uuid[], $2::uuid[]) AS p(lo, hi) \
         JOIN series a ON a.id = p.lo \
         JOIN series b ON b.id = p.hi",
        &los,
        &his,
    )
    .fetch_all(exec)
    .await?;
    Ok(rows
        .into_iter()
        .map(|r| {
            (
                (SeriesId::from_uuid(r.lo), SeriesId::from_uuid(r.hi)),
                r.sim,
            )
        })
        .collect())
}

/// The pairs currently sitting open in the queue, for the sweep to re-score.
///
/// A candidate recorded at ingest was scored before the series had tags, authors, a release year
/// or alternative titles, so its stored score is a floor rather than a verdict. Re-scoring is
/// how a pair that was genuinely ambiguous in January becomes an automatic merge once both sides
/// have been enriched — without it, the queue only ever grows.
///
/// # Errors
/// [`crate::DbError::Sqlx`] only.
pub async fn open_merge_pairs<'e, E: PgExecutor<'e>>(
    exec: E,
    limit: i64,
) -> DbResult<Vec<DuplicatePair>> {
    #[derive(FromRow)]
    struct Row {
        series_id: Uuid,
        candidate_id: Uuid,
    }
    let rows = sqlx::query_as!(
        Row,
        "SELECT series_id, candidate_id FROM merge_candidates \
         WHERE NOT resolved ORDER BY updated_at ASC LIMIT $1",
        limit,
    )
    .fetch_all(exec)
    .await?;
    Ok(rows
        .into_iter()
        .map(|r| {
            (
                SeriesId::from_uuid(r.series_id),
                SeriesId::from_uuid(r.candidate_id),
            )
        })
        .collect())
}

/// Record the scorer's verdict that a pair is **not** a duplicate, closing any open queue row
/// for it. Returns whether a row was open before this call.
///
/// # Why this is a row rather than a deletion
///
/// It used to delete, so that a later sweep could reconsider the pair. Reconsidering it is
/// right — "distinct" here is the *scorer's* conclusion, not an operator's, reached on evidence
/// that enrichment keeps changing — but deletion is not how to get it. It also made the verdict
/// invisible to [`find_duplicate_pairs`], which is ordered with a `LIMIT`: every distinct pair
/// came straight back in the same prefix on the next run, and the shortlist never advanced past
/// the pairs it had already judged. A row with `outcome = 'distinct'` is instead durable against
/// the new-pair shortlist and revisited by [`distinct_merge_pairs`] on its own budget.
///
/// An operator's `dismissed` is never overwritten. The two look alike and are not: a dismissal
/// is a human saying these are different works, and it must suppress the pair permanently, which
/// is exactly what re-entering the recheck rotation would undo.
///
/// # Errors
/// [`crate::DbError::Conflict`] when `a == b`; otherwise [`crate::DbError::Sqlx`].
pub async fn record_distinct_pair<'e, E: PgExecutor<'e>>(
    exec: E,
    a: SeriesId,
    b: SeriesId,
    score: f32,
    signals: &[&str],
) -> DbResult<bool> {
    if a == b {
        return Err(crate::error::DbError::Conflict(
            "cannot judge a series against itself".to_owned(),
        ));
    }
    let signals: Vec<String> = signals.iter().map(|s| (*s).to_owned()).collect();
    // `prior` reads the pre-insert snapshot, so it reports the state the upsert is about to
    // replace; a data-modifying CTE runs whether or not the outer query selects from it.
    let was_open = sqlx::query_scalar!(
        "WITH prior AS ( \
           SELECT resolved FROM merge_candidates \
            WHERE series_id = LEAST($2::uuid, $3::uuid) \
              AND candidate_id = GREATEST($2::uuid, $3::uuid) \
         ), upsert AS ( \
           INSERT INTO merge_candidates \
             (id, series_id, candidate_id, score, signals, reason, resolved, outcome, resolved_at) \
           VALUES ($1, LEAST($2::uuid, $3::uuid), GREATEST($2::uuid, $3::uuid), $4, $5, \
                   'duplicate sweep: below the review floor', true, 'distinct', now()) \
           ON CONFLICT (series_id, candidate_id) DO UPDATE \
              SET score = EXCLUDED.score, \
                  signals = EXCLUDED.signals, \
                  reason = EXCLUDED.reason, \
                  resolved = true, \
                  outcome = 'distinct', \
                  resolved_at = now(), \
                  updated_at = now() \
              WHERE merge_candidates.outcome IS DISTINCT FROM 'dismissed' \
           RETURNING 1 AS touched \
         ) \
         SELECT COALESCE((SELECT NOT resolved FROM prior), false) AS \"was_open!\"",
        Uuid::now_v7(),
        a.as_uuid(),
        b.as_uuid(),
        score,
        &signals,
    )
    .fetch_one(exec)
    .await?;
    Ok(was_open)
}

/// The pairs the scorer has judged distinct, least-recently-scored first, for the sweep to
/// reconsider.
///
/// The counterpart to [`record_distinct_pair`] keeping the row: a verdict reached before both
/// sides were enriched is a snapshot, not a fact, and re-scoring bumps `updated_at`, so draining
/// this oldest-first is a round-robin over the whole set rather than a fixed prefix — which is
/// the property [`find_duplicate_pairs`] lacked.
///
/// # Errors
/// [`crate::DbError::Sqlx`] only.
pub async fn distinct_merge_pairs<'e, E: PgExecutor<'e>>(
    exec: E,
    limit: i64,
) -> DbResult<Vec<DuplicatePair>> {
    #[derive(FromRow)]
    struct Row {
        series_id: Uuid,
        candidate_id: Uuid,
    }
    let rows = sqlx::query_as!(
        Row,
        "SELECT series_id, candidate_id FROM merge_candidates \
         WHERE outcome = 'distinct' ORDER BY updated_at ASC LIMIT $1",
        limit,
    )
    .fetch_all(exec)
    .await?;
    Ok(rows
        .into_iter()
        .map(|r| {
            (
                SeriesId::from_uuid(r.series_id),
                SeriesId::from_uuid(r.candidate_id),
            )
        })
        .collect())
}
