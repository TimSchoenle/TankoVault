//! The merge-candidate queue an ambiguous match feeds, and the operator actions that drain it.

use crate::error::DbResult;
use sqlx::PgExecutor;
use tankovault_domain::SeriesId;
use uuid::Uuid;

/// What [`record_merge_candidate`] did to the review queue.
///
/// The distinction is not cosmetic: [`Added`](Self::Added) and [`Reopened`](Self::Reopened) each
/// make the queue one row longer, [`Refreshed`](Self::Refreshed) leaves its length alone, and a
/// caller reporting the three as one number tells an operator the queue grew by the count of
/// rows it merely re-scored.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QueueOutcome {
    /// The pair had no row at all. The queue is one longer.
    Added,
    /// An open row was re-scored in place. The queue is the same length.
    Refreshed,
    /// A pair the *scorer* had closed as distinct is open again. The queue is one longer.
    Reopened,
    /// Resolved by an operator, or already merged — left untouched.
    Unchanged,
}

/// Record — or refresh — an operator-review merge candidate for the pair `{a, b}`.
///
/// Returns which of the four things in [`QueueOutcome`] happened.
///
/// # Idempotent, and durably dismissed
///
/// A bare `INSERT` would create two failure modes from the same missing constraint: the same
/// ambiguity observed twice would insert two rows, with `(A,B)`/`(B,A)` counted as different
/// pairs, and an operator's dismissal would not be durable — a later scan could re-insert a
/// dismissed pair as a fresh open row.
///
/// The pair is stored in canonical id order and upserted against `merge_candidates_pair_key`.
/// Storage order is deliberately *not* merge direction: which series survives a merge is decided
/// from which one carries more of the work (see [`MergeCandidateView::suggested_keep`]), not from
/// which id sorts lower.
///
/// The update is guarded so that only two states can be written over: an open row, and one the
/// *scorer* previously closed as `distinct`. Reopening the latter is the point of
/// [`record_distinct_pair`](super::record_distinct_pair) keeping a row at all — a pair judged apart before enrichment gave
/// both sides authors and synonyms has to be able to come back — while `dismissed`, `merged` and
/// `auto_merged` stay untouchable, the first because a human decided it and the other two
/// because the merge already happened.
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
) -> DbResult<QueueOutcome> {
    if a == b {
        return Err(crate::error::DbError::Conflict(
            "cannot queue a series against itself".to_owned(),
        ));
    }
    let signals: Vec<String> = signals.iter().map(|s| (*s).to_owned()).collect();
    // `prior` reads the pre-insert snapshot, so it reports the state the upsert is about to
    // replace; a data-modifying CTE runs whether or not the outer query selects from it.
    let outcome = sqlx::query_scalar!(
        "WITH prior AS ( \
           SELECT resolved, outcome FROM merge_candidates \
            WHERE series_id = LEAST($2::uuid, $3::uuid) \
              AND candidate_id = GREATEST($2::uuid, $3::uuid) \
         ), upsert AS ( \
           INSERT INTO merge_candidates (id, series_id, candidate_id, score, signals, reason) \
           VALUES ($1, LEAST($2::uuid, $3::uuid), GREATEST($2::uuid, $3::uuid), $4, $5, $6) \
           ON CONFLICT (series_id, candidate_id) DO UPDATE \
              SET score = EXCLUDED.score, \
                  signals = EXCLUDED.signals, \
                  reason = EXCLUDED.reason, \
                  resolved = false, \
                  outcome = NULL, \
                  resolved_by = NULL, \
                  resolved_at = NULL, \
                  updated_at = now() \
              WHERE NOT merge_candidates.resolved OR merge_candidates.outcome = 'distinct' \
           RETURNING 1 AS touched \
         ) \
         SELECT CASE \
            WHEN NOT EXISTS (SELECT 1 FROM prior) THEN 'added' \
            WHEN (SELECT NOT resolved FROM prior) THEN 'refreshed' \
            WHEN (SELECT outcome FROM prior) = 'distinct' THEN 'reopened' \
            ELSE 'unchanged' END AS \"outcome!\"",
        Uuid::now_v7(),
        a.as_uuid(),
        b.as_uuid(),
        score,
        &signals,
        reason,
    )
    .fetch_one(exec)
    .await?;
    Ok(match outcome.as_str() {
        "added" => QueueOutcome::Added,
        "refreshed" => QueueOutcome::Refreshed,
        "reopened" => QueueOutcome::Reopened,
        _ => QueueOutcome::Unchanged,
    })
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
/// disappears from this list without being resolved — see the note on [`merge_series`](super::merge_series).
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
