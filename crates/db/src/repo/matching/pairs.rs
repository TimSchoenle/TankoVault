//! The standing duplicate sweep and the pair-level facts an operator decision is scored on.

use crate::error::DbResult;
use serde_json::Value as Json;
use sqlx::{FromRow, PgExecutor};
use tankovault_domain::{ContentType, SeriesId};
use uuid::Uuid;

/// A pair of series worth re-scoring, in canonical id order.
pub type DuplicatePair = (SeriesId, SeriesId);

use super::MAX_KEY_FANOUT;

/// Returns every pair of *existing* series whose titles collide on the whitespace-insensitive key
/// and that the sweep has not already recorded a verdict for.
///
/// # Blocking on the compact key
///
/// A sweep cannot score every pair of the catalogue, so it needs a cheap predicate that admits
/// every pair worth a look and little else. The compact key, the normalized title without
/// spaces, is an equality, so an index lookup, and it admits the duplicate the create-time
/// matcher handles worst: a missing space between two HTML elements wrecks both the trigram
/// score and the token-set ratio.
///
/// Four collisions count: two canonical titles; one series' canonical title and another's
/// *alternative* title, the same work under its romaji and english names; two shared
/// alternative titles; and a source's *listing* title against another series' canonical or
/// listing title, a mirror that registered its own series after `AniList` renamed the one its
/// siblings sit under.
///
/// The result is a *shortlist*: every pair is re-scored with the tags, authors, years and
/// alternative titles the create-time path never had before anything is queued.
///
/// A key held by more than `MAX_KEY_FANOUT` series is dropped. Equality gives no protection
/// against one key held by thousands of series, an all-pairs clique behind a `LIMIT`; scraped
/// summary labels such as `Status` or `Genres` produce exactly that, and a title thousands of
/// series answer to identifies none of them.
///
/// # Progress
///
/// This is the *new-pair* shortlist, under a `LIMIT`, so every pair with a `merge_candidates`
/// row of any kind is excluded; an open one would otherwise return in the same prefix forever.
/// Open pairs are re-scored by [`open_merge_pairs`] and scorer-distinct ones by
/// [`distinct_merge_pairs`], each on its own budget.
///
/// There is no `ORDER BY`. A uuid order expresses no priority and forces the whole unioned,
/// anti-joined set to be built and sorted before the `LIMIT`. Every returned pair leaves with a
/// row the anti-join then excludes, so any budget-sized slice drains the shortlist. Add an
/// ordering only for a priority worth the sort.
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
        "WITH listing AS ( \
           SELECT DISTINCT series_id, replace(provider_title_normalized, ' ', '') AS key \
             FROM series_sources WHERE provider_title_normalized <> '' \
         ), over_shared AS ( \
           SELECT key FROM ( \
             SELECT replace(normalized_title, ' ', '') AS key, id AS series_id \
               FROM series WHERE normalized_title <> '' \
             UNION \
             SELECT replace(normalized, ' ', '') AS key, series_id \
               FROM series_titles WHERE normalized <> '' \
             UNION \
             SELECT key, series_id FROM listing \
           ) k \
           GROUP BY key HAVING count(*) > $2 \
         ), by_canonical AS ( \
           SELECT a.id AS lo, b.id AS hi \
           FROM series a JOIN series b \
             ON replace(a.normalized_title, ' ', '') = replace(b.normalized_title, ' ', '') \
            AND a.id < b.id \
           WHERE a.normalized_title <> '' \
             AND NOT EXISTS (SELECT 1 FROM over_shared o \
                              WHERE o.key = replace(a.normalized_title, ' ', '')) \
         ), by_alias AS ( \
           SELECT LEAST(s.id, st.series_id) AS lo, GREATEST(s.id, st.series_id) AS hi \
           FROM series s JOIN series_titles st \
             ON replace(st.normalized, ' ', '') = replace(s.normalized_title, ' ', '') \
           WHERE st.series_id <> s.id AND s.normalized_title <> '' \
             AND NOT EXISTS (SELECT 1 FROM over_shared o \
                              WHERE o.key = replace(s.normalized_title, ' ', '')) \
         ), by_shared_alias AS ( \
           SELECT x.series_id AS lo, y.series_id AS hi \
           FROM series_titles x JOIN series_titles y \
             ON replace(x.normalized, ' ', '') = replace(y.normalized, ' ', '') \
            AND x.series_id < y.series_id \
           WHERE x.normalized <> '' \
             AND NOT EXISTS (SELECT 1 FROM over_shared o \
                              WHERE o.key = replace(x.normalized, ' ', '')) \
         ), by_listing AS ( \
           SELECT LEAST(l.series_id, o.series_id) AS lo, \
                  GREATEST(l.series_id, o.series_id) AS hi \
           FROM listing l JOIN ( \
             SELECT id AS series_id, replace(normalized_title, ' ', '') AS key \
               FROM series WHERE normalized_title <> '' \
             UNION ALL \
             SELECT series_id, key FROM listing \
           ) o ON o.key = l.key AND o.series_id <> l.series_id \
           WHERE NOT EXISTS (SELECT 1 FROM over_shared x WHERE x.key = l.key) \
         ), pairs AS ( \
           SELECT lo, hi FROM by_canonical \
           UNION SELECT lo, hi FROM by_alias \
           UNION SELECT lo, hi FROM by_shared_alias \
           UNION SELECT lo, hi FROM by_listing \
         ) \
         SELECT p.lo AS \"lo!\", p.hi AS \"hi!\" FROM pairs p \
         WHERE NOT EXISTS ( \
           SELECT 1 FROM merge_candidates mc \
           WHERE mc.series_id = p.lo AND mc.candidate_id = p.hi \
         ) \
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

/// Series with no source paired with a sourced series of the same normalized title, as
/// `(orphan, twin)`, at most `limit` of them and one twin per orphan.
///
/// The twin rule of the duplicate sweep: an orphan with a sourced twin is a leftover copy, so an
/// exact title is enough unless both carry different `AniList` ids. **Not** capped by
/// `MAX_KEY_FANOUT`: such copies come in clusters larger than the cap, and lifting it globally
/// would let junk keys collapse unrelated works. Running uncapped is safe because one side is
/// sourceless, so this can never join two sourced series.
///
/// Among several sourced twins the one with the most sources wins, so every orphan of one title
/// lands on the same survivor.
///
/// # Errors
/// [`crate::DbError::Sqlx`] only.
pub async fn find_orphan_twins<'e, E: PgExecutor<'e>>(
    exec: E,
    limit: i64,
) -> DbResult<Vec<(SeriesId, SeriesId)>> {
    #[derive(FromRow)]
    struct Row {
        orphan: Uuid,
        twin: Uuid,
    }
    let rows = sqlx::query_as!(
        Row,
        // The compact-key equality is redundant with the exact one and is there for the
        // planner: `series_compact_title_idx` serves it, and nothing indexes the bare column.
        "SELECT DISTINCT ON (o.id) o.id AS \"orphan!\", t.id AS \"twin!\" \
           FROM series o \
           JOIN series t \
             ON replace(t.normalized_title, ' ', '') = replace(o.normalized_title, ' ', '') \
            AND t.normalized_title = o.normalized_title \
            AND t.id <> o.id \
          WHERE o.normalized_title <> '' \
            AND NOT EXISTS (SELECT 1 FROM series_sources x WHERE x.series_id = o.id) \
            AND EXISTS (SELECT 1 FROM series_sources x WHERE x.series_id = t.id) \
            AND NOT EXISTS ( \
              SELECT 1 FROM sync_mappings mo \
                JOIN sync_mappings mt ON mt.provider = mo.provider AND mt.series_id = t.id \
               WHERE mo.series_id = o.id AND mo.provider = 'anilist' \
                 AND mo.external_id <> mt.external_id) \
          ORDER BY o.id, \
                   (SELECT count(*) FROM series_sources x WHERE x.series_id = t.id) DESC, t.id \
          LIMIT $1",
        limit,
    )
    .fetch_all(exec)
    .await?;
    Ok(rows
        .into_iter()
        .map(|r| (SeriesId::from_uuid(r.orphan), SeriesId::from_uuid(r.twin)))
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
    /// The series these facts describe.
    pub series_id: SeriesId,
    /// Its display title, for the evidence the decision records.
    pub canonical_title: String,
    /// The key the base similarity is measured on.
    pub normalized_title: String,
    /// Its aliases, normalized, which an identity rule may match on alone.
    pub alt_normalized_titles: Vec<String>,
    /// Its sources' listing titles, normalized: primary titles, like the canonical one.
    pub source_titles: Vec<String>,
    /// Its `AniList` id from `sync_mappings`, which agrees with or vetoes the other side's.
    pub anilist_id: Option<String>,
    /// Its medium, which a guard can refuse a merge on.
    pub content_type: ContentType,
    /// Its year, `None` when the catalogue has none. A guard refuses on a mismatch,
    /// never on an absence.
    pub release_year: Option<i32>,
    /// Its genre names, for the overlap bonus.
    pub tags: Vec<String>,
    /// Its credits, for the shared-author signal.
    pub authors: Vec<String>,
    /// Providers carrying it, the first term in the survivor choice.
    pub source_count: i64,
    /// Chapter links under it, the second term.
    pub chapter_count: i64,
    /// Readers watching it, which is what a wrong survivor choice costs.
    pub watcher_count: i64,
}

impl SeriesMatchFacts {
    /// How much of the catalogue this series carries, most significant first.
    ///
    /// The merge survivor is chosen by this, not by age or id order. The merge unions both
    /// sides' data, but the absorbed id stops existing and every bookmark, notification and
    /// tracker mapping naming it breaks, so the id more of the world points at survives.
    #[must_use]
    pub const fn weight(&self) -> (i64, i64, i64) {
        (self.source_count, self.chapter_count, self.watcher_count)
    }
}

/// Loads [`SeriesMatchFacts`] for the given series in one round trip. Unknown ids are absent from
/// the result.
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
        source_titles: Vec<String>,
        anilist_id: Option<String>,
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
                COALESCE((SELECT array_agg(DISTINCT ss.provider_title_normalized) \
                 FROM series_sources ss WHERE ss.series_id = s.id \
                  AND ss.provider_title_normalized <> ''), '{}') AS \"source_titles!\", \
                (SELECT m.external_id FROM sync_mappings m \
                  WHERE m.series_id = s.id AND m.provider = 'anilist') AS anilist_id, \
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
            source_titles: r.source_titles,
            anilist_id: r.anilist_id,
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

/// Returns the trigram similarity of each given pair.
///
/// Canonical against canonical, and each side's alternative titles against the other's canonical
/// title, taking the best of the three. Source listing titles are not compared, so a pair
/// [`find_candidates`] scored `1` on a compact-key listing-title match can score lower here.
///
/// [`find_candidates`]: super::find_candidates
///
/// [`SeriesMatchFacts`] describes one series, but the trigram score is a property of the pair
/// and comes from the database. Scored as zero, a pair whose stored score came from a trigram
/// match would drop below the review floor on no new evidence and leave the queue. Batched, so
/// the sweep pays one query. Pairs whose ids do not both exist are absent.
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

/// Returns the pairs currently sitting open in the queue, for the sweep to re-score.
///
/// A candidate recorded at ingest was scored before the series had tags, authors, a release
/// year or alternative titles, so its stored score is a floor, not a verdict. Re-scoring lets an
/// ambiguous pair become an automatic merge once both sides are enriched, or the queue only
/// grows.
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

/// One pair the scorer has judged **not** a duplicate.
#[derive(Debug, Clone, Copy)]
pub struct DistinctVerdict<'a> {
    /// The pair in any order; [`record_distinct_pairs`] stores it in canonical id order.
    pub pair: DuplicatePair,
    /// The similarity it was judged at, so a later threshold change can reopen it.
    pub score: f32,
    /// The stable slugs of the scoring rules that fired.
    pub signals: &'a [&'a str],
}

/// Records the scorer's verdict that a batch of pairs are **not** duplicates, closing any open
/// queue row for each. Returns, per pair, whether a row was open before this call.
///
/// "Distinct" is the *scorer's* conclusion on evidence enrichment keeps changing, so the pair
/// must be reconsidered later, but as a row, not a deletion: a deleted verdict is invisible to
/// [`find_duplicate_pairs`], whose `LIMIT` would return the pair in the same prefix every run.
/// A row with `outcome = 'distinct'` is excluded from the new-pair shortlist and revisited by
/// [`distinct_merge_pairs`] on its own budget.
///
/// An operator's `dismissed` is never overwritten: a human said these are different works, and
/// re-entering the recheck rotation would undo that.
///
/// A pair naming a deleted series is dropped, as in
/// [`record_merge_candidates`](super::record_merge_candidates), since one foreign-key violation
/// would fail the whole batch.
///
/// # Errors
/// [`crate::DbError::Conflict`] when a pair names one series twice, or when two entries name the
/// same canonical pair, which one `ON CONFLICT DO UPDATE` cannot touch twice. Otherwise
/// [`crate::DbError::Sqlx`].
pub async fn record_distinct_pairs<'e, E: PgExecutor<'e>>(
    exec: E,
    verdicts: &[DistinctVerdict<'_>],
) -> DbResult<Vec<(DuplicatePair, bool)>> {
    if verdicts.is_empty() {
        return Ok(Vec::new());
    }
    let mut seen = std::collections::HashSet::with_capacity(verdicts.len());
    let mut rows = Vec::with_capacity(verdicts.len());
    for verdict in verdicts {
        let (a, b) = verdict.pair;
        if a == b {
            return Err(crate::error::DbError::Conflict(
                "cannot judge a series against itself".to_owned(),
            ));
        }
        if !seen.insert((a.min(b), a.max(b))) {
            return Err(crate::error::DbError::Conflict(
                "the same pair was judged twice in one batch".to_owned(),
            ));
        }
        rows.push(serde_json::json!({
            "id": Uuid::now_v7(),
            "a": a.as_uuid(),
            "b": b.as_uuid(),
            "score": verdict.score,
            "signals": verdict.signals,
        }));
    }

    // `prior` reads the pre-insert snapshot, so it reports the state the upsert replaces; a
    // data-modifying CTE runs whether or not the outer query selects from it. `live` is the
    // foreign-key guard: two primary-key lookups, so a pair with a side merged away since it was
    // judged leaves the batch instead of failing it.
    let rows = sqlx::query!(
        "WITH input AS ( \
           SELECT d.id, LEAST(d.a, d.b) AS series_id, GREATEST(d.a, d.b) AS candidate_id, \
                  d.score, d.signals \
             FROM jsonb_to_recordset($1::jsonb) AS d( \
                  id uuid, a uuid, b uuid, score real, signals text[]) \
         ), live AS ( \
           SELECT i.* FROM input i \
             JOIN series lo ON lo.id = i.series_id \
             JOIN series hi ON hi.id = i.candidate_id \
         ), prior AS ( \
           SELECT l.series_id, l.candidate_id, mc.resolved \
             FROM live l \
             LEFT JOIN merge_candidates mc \
               ON mc.series_id = l.series_id AND mc.candidate_id = l.candidate_id \
         ), upsert AS ( \
           INSERT INTO merge_candidates \
             (id, series_id, candidate_id, score, signals, reason, resolved, outcome, resolved_at) \
           SELECT id, series_id, candidate_id, score, signals, \
                  'duplicate sweep: below the review floor', true, 'distinct', now() \
             FROM live \
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
         SELECT p.series_id AS \"series_id!\", p.candidate_id AS \"candidate_id!\", \
                COALESCE(NOT p.resolved, false) AS \"was_open!\" \
           FROM prior p",
        Json::Array(rows),
    )
    .fetch_all(exec)
    .await?;

    Ok(rows
        .into_iter()
        .map(|r| {
            (
                (
                    SeriesId::from_uuid(r.series_id),
                    SeriesId::from_uuid(r.candidate_id),
                ),
                r.was_open,
            )
        })
        .collect())
}

/// Records the scorer's verdict that one pair is not a duplicate.
///
/// Answers whether a queue row was open before the call. `false` also when one of the two series
/// no longer exists, because the merge that removed it cascaded the pair's row away.
///
/// See [`record_distinct_pairs`] for the semantics; the sweep batches, this is for callers that
/// hold a single pair.
///
/// # Errors
/// As [`record_distinct_pairs`].
pub async fn record_distinct_pair<'e, E: PgExecutor<'e>>(
    exec: E,
    a: SeriesId,
    b: SeriesId,
    score: f32,
    signals: &[&str],
) -> DbResult<bool> {
    let outcomes = record_distinct_pairs(
        exec,
        &[DistinctVerdict {
            pair: (a, b),
            score,
            signals,
        }],
    )
    .await?;
    // No row means one of the two series has ceased to exist since the caller resolved it: the
    // verdict is not recorded and there was no open row to close. Every other case returns a row,
    // because the statement selects from a `LEFT JOIN` over its input.
    Ok(outcomes
        .into_iter()
        .next()
        .is_some_and(|(_, was_open)| was_open))
}

/// Returns the pairs the scorer has judged distinct, least-recently-scored first, for the sweep to
/// reconsider.
///
/// A verdict reached before both sides were enriched is a snapshot, not a fact. Re-scoring bumps
/// `updated_at`, so draining this oldest-first is a round-robin over the whole set, not a fixed
/// prefix.
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
