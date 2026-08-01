//! Candidate lookup for series canonicalisation (design §10, step 2), plus the merge-candidate
//! queue an ambiguous match feeds and the duplicate sweep that keeps it honest.
//!
//! This layer returns raw trigram candidates and performs whatever the caller's
//! [`Canonicaliser`](tankovault_domain::matching::Canonicaliser) decides; the scoring and the
//! thresholds live above it (`tankovault_matcher` and `tankovault_config::MatchingConfig`), so
//! it is unit-testable without a database and this crate links no scorer.
//!
//! The candidate type is [`tankovault_domain::matching::Candidate`] itself rather than a row
//! struct plus a `From` impl: a hand-written conversion duplicated across the worker's ingest
//! canonicalisation and `services/sync`'s remote-entry resolution would let adding a field
//! silently drop that signal from one of the two paths deciding whether two series are the
//! same.
//!
//! # Two ways a duplicate is found
//!
//! [`find_candidates`] is the *create-time* path: it runs while a scanned source is being filed
//! and answers "does this already exist?". It is necessarily blind to anything the catalogue
//! learns later — a series acquires its authors, its release year and its alternative titles
//! from a subsequent enrichment pass, and by then the decision has been taken.
//!
//! [`find_duplicate_pairs`] is the *standing* path, and exists because the first one is not
//! enough. It blocks the whole catalogue on the whitespace-insensitive title key (canonical
//! against canonical, canonical against alias, alias against alias) and hands back every pair
//! worth re-scoring with everything now known about both sides. On a 26k-series catalogue it
//! surfaced 59 pairs with byte-identical compact titles that the create-time path had never
//! queued at all.

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
        "SELECT s.id, s.normalized_title, s.content_type AS \"content_type: ContentType\", s.release_year, \
                GREATEST( \
                  similarity(s.normalized_title, $1), \
                  COALESCE((SELECT MAX(similarity(st.normalized, $1)) \
                            FROM series_titles st WHERE st.series_id = s.id), 0) \
                ) AS \"sim!\", \
                COALESCE((SELECT array_agg(st.normalized) FROM series_titles st \
                 WHERE st.series_id = s.id), '{}') AS \"alt_titles!\", \
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
        "SELECT q.norm AS \"query_title!\", c.id, c.normalized_title, \
                c.content_type AS \"content_type: ContentType\", c.release_year, \
                c.sim AS \"sim!\", c.alt_titles AS \"alt_titles!\", \
                c.tags AS \"tags!\", c.authors AS \"authors!\" \
         FROM UNNEST($1::text[]) AS q(norm) \
         CROSS JOIN LATERAL ( \
           SELECT s.id, s.normalized_title, s.content_type, s.release_year, \
                  GREATEST( \
                    similarity(s.normalized_title, q.norm), \
                    COALESCE((SELECT MAX(similarity(st.normalized, q.norm)) \
                              FROM series_titles st WHERE st.series_id = s.id), 0) \
                  ) AS sim, \
                  COALESCE((SELECT array_agg(st.normalized) FROM series_titles st \
                   WHERE st.series_id = s.id), '{}') AS alt_titles, \
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

/// Record — or refresh — an operator-review merge candidate for the pair `{a, b}`.
///
/// Returns whether the open queue actually changed: `false` means the pair is already resolved
/// and was left alone.
///
/// # Idempotent, and durably dismissed
///
/// A bare `INSERT` would create two failure modes from the same missing constraint: the same
/// ambiguity observed twice would insert two rows, with `(A,B)`/`(B,A)` counted as different
/// pairs, and an operator's dismissal would not be durable — a later scan could re-insert a
/// dismissed pair as a fresh open row.
///
/// The pair is stored in canonical id order and upserted against
/// `merge_candidates_pair_key`, with the update guarded by `NOT resolved`. Storage order is
/// deliberately *not* merge direction: which series survives a merge is decided from which one
/// carries more of the work (see [`MergeCandidateView::suggested_keep`]), not from which id
/// sorts lower.
///
/// # Errors
/// [`crate::DbError::Conflict`] when `a == b`, which the table's own check constraint would
/// reject anyway — returned here so the caller gets a domain error rather than a driver one.
/// Otherwise [`crate::DbError::Sqlx`].
pub async fn record_merge_candidate<'e, E: PgExecutor<'e>>(
    exec: E,
    a: SeriesId,
    b: SeriesId,
    score: f32,
    signals: &[&str],
    reason: &str,
) -> DbResult<bool> {
    if a == b {
        return Err(crate::error::DbError::Conflict(
            "cannot queue a series against itself".to_owned(),
        ));
    }
    let signals: Vec<String> = signals.iter().map(|s| (*s).to_owned()).collect();
    let changed = sqlx::query_scalar!(
        "INSERT INTO merge_candidates (id, series_id, candidate_id, score, signals, reason) \
         VALUES ($1, LEAST($2::uuid, $3::uuid), GREATEST($2::uuid, $3::uuid), $4, $5, $6) \
         ON CONFLICT (series_id, candidate_id) DO UPDATE \
            SET score = EXCLUDED.score, \
                signals = EXCLUDED.signals, \
                reason = EXCLUDED.reason, \
                updated_at = now() \
            WHERE NOT merge_candidates.resolved \
         RETURNING id",
        Uuid::now_v7(),
        a.as_uuid(),
        b.as_uuid(),
        score,
        &signals,
        reason,
    )
    .fetch_optional(exec)
    .await?;
    Ok(changed.is_some())
}

/// A pending merge candidate enriched with everything an operator needs to judge it without
/// opening both series (design §11 `GET /v1/admin/merge-candidates`).
///
/// The counts are the reason this is not just two titles and a number. Deciding a merge means
/// deciding which row *survives*, and the answer is whichever one carries more of the catalogue:
/// merging the richer series into the emptier one is not wrong in the data it preserves — the
/// merge unions everything either way — but it destroys the id that every existing bookmark,
/// notification and external mapping already names.
#[derive(Debug, Clone, serde::Serialize)]
pub struct MergeCandidateView {
    pub id: Uuid,
    pub series_id: SeriesId,
    pub series_title: String,
    pub series_sources: i64,
    pub series_chapters: i64,
    pub candidate_id: SeriesId,
    pub candidate_title: String,
    pub candidate_sources: i64,
    pub candidate_chapters: i64,
    pub score: f32,
    /// The stable slugs of the scoring rules that fired
    /// ([`tankovault_domain::matching::MatchSignals::labels`]).
    pub signals: Vec<String>,
    pub reason: Option<String>,
    /// Which of the two the console should offer to keep: the series with more sources, then
    /// more chapters, then the older id. Advisory — `POST /v1/admin/series/merge` takes an
    /// explicit direction — but it is the answer an operator would reach anyway, computed once
    /// here instead of eyeballed 2 600 times.
    pub suggested_keep: SeriesId,
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: time::OffsetDateTime,
    #[serde(with = "time::serde::rfc3339")]
    pub updated_at: time::OffsetDateTime,
}

/// List the open (unresolved) merge candidates, **highest confidence first**.
///
/// The ordering is the point: `created_at DESC` would put whatever the last scan happened to
/// observe at the top and bury certain duplicates in a queue of thousands of rows. `min_score`
/// narrows it further, so an operator can work the queue in confidence bands rather than as one
/// undifferentiated list.
///
/// # Errors
/// [`crate::DbError::Sqlx`] only — no other variant is reachable. An empty queue is an empty
/// `Vec`. Note that both series are inner-joined, so a candidate naming a deleted series
/// disappears from this list without being resolved — see the note on [`merge_series`].
pub async fn list_open_merge_candidates<'e, E: PgExecutor<'e>>(
    exec: E,
    limit: i64,
    min_score: f32,
) -> DbResult<Vec<MergeCandidateView>> {
    #[derive(sqlx::FromRow)]
    struct Row {
        id: Uuid,
        series_id: Uuid,
        series_title: String,
        series_sources: i64,
        series_chapters: i64,
        candidate_id: Uuid,
        candidate_title: String,
        candidate_sources: i64,
        candidate_chapters: i64,
        score: f32,
        signals: Vec<String>,
        reason: Option<String>,
        created_at: time::OffsetDateTime,
        updated_at: time::OffsetDateTime,
    }
    let rows = sqlx::query_as!(
        Row,
        "SELECT mc.id, mc.series_id, s1.canonical_title AS series_title, \
                mc.candidate_id, s2.canonical_title AS candidate_title, \
                mc.score, mc.signals, mc.reason, mc.created_at, mc.updated_at, \
                (SELECT count(*) FROM series_sources ss WHERE ss.series_id = s1.id) AS \"series_sources!\", \
                (SELECT count(*) FROM series_sources ss WHERE ss.series_id = s2.id) AS \"candidate_sources!\", \
                COALESCE((SELECT sum(ss.chapter_count) FROM series_sources ss \
                          WHERE ss.series_id = s1.id), 0) AS \"series_chapters!\", \
                COALESCE((SELECT sum(ss.chapter_count) FROM series_sources ss \
                          WHERE ss.series_id = s2.id), 0) AS \"candidate_chapters!\" \
         FROM merge_candidates mc \
         JOIN series s1 ON s1.id = mc.series_id \
         JOIN series s2 ON s2.id = mc.candidate_id \
         WHERE NOT mc.resolved AND mc.score >= $2 \
         ORDER BY mc.score DESC, mc.created_at DESC \
         LIMIT $1",
        limit,
        min_score,
    )
    .fetch_all(exec)
    .await?;
    Ok(rows
        .into_iter()
        .map(|r| {
            let series_id = SeriesId::from_uuid(r.series_id);
            let candidate_id = SeriesId::from_uuid(r.candidate_id);
            // More sources, then more chapters, then the lower id — the last only so the answer
            // is deterministic for two identically-sized series rather than an artefact of row
            // order.
            let keep_series = (
                r.series_sources,
                r.series_chapters,
                std::cmp::Reverse(r.series_id),
            ) >= (
                r.candidate_sources,
                r.candidate_chapters,
                std::cmp::Reverse(r.candidate_id),
            );
            MergeCandidateView {
                id: r.id,
                series_id,
                series_title: r.series_title,
                series_sources: r.series_sources,
                series_chapters: r.series_chapters,
                candidate_id,
                candidate_title: r.candidate_title,
                candidate_sources: r.candidate_sources,
                candidate_chapters: r.candidate_chapters,
                score: r.score,
                signals: r.signals,
                reason: r.reason,
                suggested_keep: if keep_series { series_id } else { candidate_id },
                created_at: r.created_at,
                updated_at: r.updated_at,
            }
        })
        .collect())
}

/// Dismiss a merge candidate (operator judged the two works distinct) without merging.
///
/// The dismissal is now **durable**: the row is kept, resolved and marked `dismissed`, and
/// [`record_merge_candidate`] will not reopen it. That is what the `outcome` column is for —
/// `resolved` alone could not distinguish "an operator said these are different works", which
/// must suppress the pair forever, from "these were merged", which needs no suppression because
/// one of the two series no longer exists.
///
/// # Errors
/// [`crate::DbError::Sqlx`] only — no other variant is reachable. An unknown id and an
/// already-resolved one are both `Ok(false)`, not [`crate::DbError::NotFound`]: the
/// `NOT resolved` predicate makes dismissal idempotent, so a double-click cannot report a
/// failure for work that was already done.
pub async fn dismiss_merge_candidate<'e, E: PgExecutor<'e>>(
    exec: E,
    id: Uuid,
    resolved_by: Option<tankovault_domain::UserId>,
) -> DbResult<bool> {
    let result = sqlx::query!(
        "UPDATE merge_candidates \
         SET resolved = true, outcome = 'dismissed', resolved_by = $2, resolved_at = now(), \
             updated_at = now() \
         WHERE id = $1 AND NOT resolved",
        id,
        resolved_by.map(tankovault_domain::UserId::as_uuid),
    )
    .execute(exec)
    .await?;
    Ok(result.rows_affected() > 0)
}

/// Suppress a pair permanently without an existing queue row.
///
/// The bulk-dismiss path needs this: an operator clearing a confidence band is saying "none of
/// these are duplicates", and the pairs must stay suppressed against the standing duplicate
/// sweep, which does not consult the queue for pairs it has never seen.
///
/// # Errors
/// [`crate::DbError::Conflict`] when `a == b`; otherwise [`crate::DbError::Sqlx`].
pub async fn suppress_pair<'e, E: PgExecutor<'e>>(
    exec: E,
    a: SeriesId,
    b: SeriesId,
    resolved_by: Option<tankovault_domain::UserId>,
) -> DbResult<()> {
    if a == b {
        return Err(crate::error::DbError::Conflict(
            "cannot suppress a series against itself".to_owned(),
        ));
    }
    sqlx::query!(
        "INSERT INTO merge_candidates \
            (id, series_id, candidate_id, score, reason, resolved, outcome, resolved_by, resolved_at) \
         VALUES ($1, LEAST($2::uuid, $3::uuid), GREATEST($2::uuid, $3::uuid), 0, 'operator marked distinct', true, \
                 'dismissed', $4, now()) \
         ON CONFLICT (series_id, candidate_id) DO UPDATE \
            SET resolved = true, outcome = 'dismissed', resolved_by = $4, resolved_at = now(), \
                updated_at = now()",
        Uuid::now_v7(),
        a.as_uuid(),
        b.as_uuid(),
        resolved_by.map(tankovault_domain::UserId::as_uuid),
    )
    .execute(exec)
    .await?;
    Ok(())
}

/// A pair of series worth re-scoring, in canonical id order.
pub type DuplicatePair = (SeriesId, SeriesId);

/// Every pair of *existing* series whose titles collide on the whitespace-insensitive key,
/// excluding pairs an operator has already resolved.
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
        "WITH by_canonical AS ( \
           SELECT a.id AS lo, b.id AS hi \
           FROM series a JOIN series b \
             ON replace(a.normalized_title, ' ', '') = replace(b.normalized_title, ' ', '') \
            AND a.id < b.id \
           WHERE a.normalized_title <> '' \
         ), by_alias AS ( \
           SELECT LEAST(s.id, st.series_id) AS lo, GREATEST(s.id, st.series_id) AS hi \
           FROM series s JOIN series_titles st \
             ON replace(st.normalized, ' ', '') = replace(s.normalized_title, ' ', '') \
           WHERE st.series_id <> s.id AND s.normalized_title <> '' \
         ), by_shared_alias AS ( \
           SELECT x.series_id AS lo, y.series_id AS hi \
           FROM series_titles x JOIN series_titles y \
             ON replace(x.normalized, ' ', '') = replace(y.normalized, ' ', '') \
            AND x.series_id < y.series_id \
           WHERE x.normalized <> '' \
         ), pairs AS ( \
           SELECT lo, hi FROM by_canonical \
           UNION SELECT lo, hi FROM by_alias \
           UNION SELECT lo, hi FROM by_shared_alias \
         ) \
         SELECT p.lo AS \"lo!\", p.hi AS \"hi!\" FROM pairs p \
         WHERE NOT EXISTS ( \
           SELECT 1 FROM merge_candidates mc \
           WHERE mc.series_id = p.lo AND mc.candidate_id = p.hi AND mc.resolved \
         ) \
         ORDER BY p.lo, p.hi \
         LIMIT $1",
        limit,
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

/// The trigram similarity of each given pair, computed exactly as [`find_candidates`] computes
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

/// Drop an open queue row for a pair the sweep has re-judged as distinct.
///
/// Deleted rather than resolved: "distinct" here is the *scorer's* conclusion, not an
/// operator's, and recording it as a dismissal would suppress the pair permanently on evidence
/// that may change. Deleting lets a later sweep reconsider it.
///
/// # Errors
/// [`crate::DbError::Sqlx`] only.
pub async fn withdraw_merge_candidate<'e, E: PgExecutor<'e>>(
    exec: E,
    a: SeriesId,
    b: SeriesId,
) -> DbResult<bool> {
    let result = sqlx::query!(
        "DELETE FROM merge_candidates \
         WHERE series_id = LEAST($1::uuid, $2::uuid) AND candidate_id = GREATEST($1::uuid, $2::uuid) AND NOT resolved",
        a.as_uuid(),
        b.as_uuid(),
    )
    .execute(exec)
    .await?;
    Ok(result.rows_affected() > 0)
}

/// What a normalized-key rebuild changed.
#[derive(Debug, Clone, Copy, Default, serde::Serialize)]
pub struct KeyRebuildReport {
    pub series_scanned: i64,
    pub series_updated: i64,
    pub titles_scanned: i64,
    pub titles_updated: i64,
    /// Alternative titles dropped because the corrected rules collapsed them onto a key the
    /// same series already had.
    pub titles_deduplicated: i64,
}

/// Re-derive every stored normalized key through `normalizer`, which is
/// [`tankovault_domain::normalize_title`].
///
/// # Why this exists as an operator action
///
/// `normalized_title` is a *persisted* key: it is written once, at series creation, and every
/// later match is against the stored value. So a change to the normalization rules — like
/// making an apostrophe join a word instead of splitting one — leaves the whole catalogue on
/// keys derived by the previous rules, and the improvement only reaches rows that happen to be
/// re-scanned. `0023_merge_queue.up.sql` bootstraps the rebuild in SQL, but the SQL there is a
/// twin of the Rust function rather than the function itself; this is the authoritative pass,
/// and it is safe to run repeatedly because it only writes rows whose key actually changed.
///
/// Chunked by id so a 26k-row catalogue does not hold one transaction open across the whole
/// rebuild.
///
/// # Errors
/// [`crate::DbError::Sqlx`] only.
pub async fn rebuild_normalized_keys(
    pool: &sqlx::PgPool,
    normalizer: fn(&str) -> String,
) -> DbResult<KeyRebuildReport> {
    const CHUNK: i64 = 500;
    let mut report = KeyRebuildReport::default();

    let mut cursor = Uuid::nil();
    loop {
        let rows = sqlx::query!(
            "SELECT id, canonical_title, normalized_title FROM series \
             WHERE id > $1 ORDER BY id LIMIT $2",
            cursor,
            CHUNK,
        )
        .fetch_all(pool)
        .await?;
        if rows.is_empty() {
            break;
        }
        for row in &rows {
            cursor = row.id;
            report.series_scanned += 1;
            let fresh = normalizer(&row.canonical_title);
            if fresh != row.normalized_title {
                sqlx::query!(
                    "UPDATE series SET normalized_title = $2 WHERE id = $1",
                    row.id,
                    fresh,
                )
                .execute(pool)
                .await?;
                report.series_updated += 1;
            }
        }
    }

    // Alternative titles are keyed by `(series_id, normalized)`, so a rewritten key can collide
    // with a row the same series already holds. That is not an error — the two titles now *are*
    // the same key — so the colliding row is dropped rather than the update failing.
    let mut cursor = (Uuid::nil(), String::new());
    loop {
        let rows = sqlx::query!(
            "SELECT series_id, title, normalized FROM series_titles \
             WHERE (series_id, normalized) > ($1, $2) ORDER BY series_id, normalized LIMIT $3",
            cursor.0,
            cursor.1,
            CHUNK,
        )
        .fetch_all(pool)
        .await?;
        if rows.is_empty() {
            break;
        }
        for row in &rows {
            cursor = (row.series_id, row.normalized.clone());
            report.titles_scanned += 1;
            let fresh = normalizer(&row.title);
            if fresh == row.normalized {
                continue;
            }
            let inserted = sqlx::query!(
                "INSERT INTO series_titles (series_id, title, normalized) VALUES ($1,$2,$3) \
                 ON CONFLICT (series_id, normalized) DO NOTHING",
                row.series_id,
                row.title,
                fresh,
            )
            .execute(pool)
            .await?;
            sqlx::query!(
                "DELETE FROM series_titles WHERE series_id = $1 AND normalized = $2",
                row.series_id,
                row.normalized,
            )
            .execute(pool)
            .await?;
            if inserted.rows_affected() > 0 {
                report.titles_updated += 1;
            } else {
                report.titles_deduplicated += 1;
            }
        }
    }

    Ok(report)
}

/// Transactionally merge `drop_id` into `keep_id` (design §10 operator merge): re-parent
/// the merged series' sources, union its titles and tags, migrate user watchlist/progress,
/// sync state and external mappings, resolve any related merge candidates, then delete it. All
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
/// Getting this wrong produces a `(whole, part)` pair §A.1 forbids (e.g. `(6, 4.5)`) that every
/// read model is entitled to assume cannot occur.
///
/// # Tables that must move with the merge
///
/// `series_sync_overrides`, `sync_history`, `sync_remote_entries` and `notification_dedup` all
/// reference `series`; omitting any of them silently destroys a user's per-series sync
/// exclusions and visible sync history, and orphans remote tracker entries matched to the
/// absorbed series (`ON DELETE SET NULL` turns them *unmatched*, re-resolved from scratch on
/// the next pull).
///
/// # Merge candidates
///
/// The `UPDATE merge_candidates` below is belt-and-braces: both of that table's series columns are
/// `ON DELETE CASCADE`, so every row naming `drop_id` is removed by the `DELETE FROM series` that
/// follows regardless. What matters — and what `repo_matching.rs` asserts — is that no *unresolved*
/// candidate is left naming a series that no longer exists, because
/// [`list_open_merge_candidates`] inner-joins both sides and such a row would silently vanish from
/// the operator's queue while staying open in the table.
///
/// # Errors
/// [`crate::DbError::Conflict`] — a 409 — when `keep_id == drop_id`, checked before the
/// transaction opens. [`crate::DbError::NotFound`] — a 404 — when either series is missing,
/// which is one `count(*) = 2` check rather than two lookups so a series deleted between them
/// cannot slip through. Otherwise [`crate::DbError::Sqlx`] from any statement in the
/// transaction, which rolls back whole: a partial merge would leave sources re-parented to a
/// series whose titles and progress had not moved, so there is no partial-success return.
// A straight-line sequence of per-table union inserts reads more clearly as one function
// than split across arbitrary helpers just to dodge the line-count lint.
#[expect(
    clippy::too_many_lines,
    reason = "one straight-line sequence of per-table union inserts; splitting it to satisfy \
              a line count would hide the order the tables must be moved in"
)]
pub async fn merge_series(
    pool: &sqlx::PgPool,
    keep_id: SeriesId,
    drop_id: SeriesId,
    actor: Option<tankovault_domain::UserId>,
    outcome: &str,
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

    // A user's decision to exclude a series from a tracker is theirs, not the catalogue's, and
    // must survive the catalogue deciding two rows were one. `excluded` is kept if *either*
    // row excluded, because the conservative reading of "do not sync this" is to keep not
    // syncing it.
    sqlx::query!(
        "INSERT INTO series_sync_overrides (user_id, series_id, provider, excluded) \
         SELECT user_id, $1, provider, excluded FROM series_sync_overrides WHERE series_id = $2 \
         ON CONFLICT (user_id, series_id, provider) DO UPDATE \
            SET excluded = series_sync_overrides.excluded OR EXCLUDED.excluded",
        keep,
        drop,
    )
    .execute(&mut *tx)
    .await?;

    // The user-visible sync log. Re-pointed rather than unioned: these rows have their own
    // primary key and no uniqueness to collide on.
    sqlx::query!(
        "UPDATE sync_history SET series_id = $1 WHERE series_id = $2",
        keep,
        drop,
    )
    .execute(&mut *tx)
    .await?;

    // Remote tracker entries already matched to the absorbed series. Without this the FK's
    // `ON DELETE SET NULL` turns them into *unmatched* entries, and the next pull re-resolves
    // them from the title — which is the same guess that produced the duplicate in the first
    // place.
    sqlx::query!(
        "UPDATE sync_remote_entries SET series_id = $1 WHERE series_id = $2",
        keep,
        drop,
    )
    .execute(&mut *tx)
    .await?;

    // Unresolved conflicts move where they can. The partial unique index only admits one open
    // conflict per (user, series, provider, field), so a collision means the survivor already
    // has an open conflict about the same field and the absorbed one is redundant.
    sqlx::query!(
        "UPDATE sync_conflicts SET series_id = $1 WHERE series_id = $2 \
         AND (resolved_at IS NOT NULL \
              OR NOT EXISTS (SELECT 1 FROM sync_conflicts c2 \
                             WHERE c2.user_id = sync_conflicts.user_id \
                               AND c2.series_id = $1 \
                               AND c2.provider = sync_conflicts.provider \
                               AND c2.field = sync_conflicts.field \
                               AND c2.resolved_at IS NULL))",
        keep,
        drop,
    )
    .execute(&mut *tx)
    .await?;

    // Notification suppression. Not moving these re-notifies every watcher of the survivor for
    // every chapter the absorbed series had already announced — which, on an automatic merge, is
    // a mail-out nobody asked for.
    sqlx::query!(
        "INSERT INTO notification_dedup (user_id, series_id, chapter_number, created_at) \
         SELECT user_id, $1, chapter_number, created_at FROM notification_dedup WHERE series_id = $2 \
         ON CONFLICT (user_id, series_id, chapter_number) DO NOTHING",
        keep,
        drop,
    )
    .execute(&mut *tx)
    .await?;

    // Resolve every open candidate that referenced the vanishing series.
    sqlx::query!(
        "UPDATE merge_candidates \
         SET resolved = true, outcome = $3, resolved_by = $2, resolved_at = now(), \
             updated_at = now() \
         WHERE (series_id = $1 OR candidate_id = $1) AND NOT resolved",
        drop,
        actor.map(tankovault_domain::UserId::as_uuid),
        outcome,
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
