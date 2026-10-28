//! The merge-candidate queue an ambiguous match feeds, and the operator actions that drain it.

use crate::error::DbResult;
use serde_json::Value as Json;
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
    /// Resolved by an operator, or already merged, and left untouched. Also what the single-pair
    /// [`record_merge_candidate`] reports when one of the two series is gone.
    Unchanged,
}

/// One pair to record or refresh in the review queue.
#[derive(Debug, Clone, Copy)]
pub struct NewMergeCandidate<'a> {
    /// The pair in any order; [`record_merge_candidates`] stores it in canonical id order.
    pub pair: (SeriesId, SeriesId),
    /// Similarity in `[0,1]`, after every scoring term.
    pub score: f32,
    /// The stable slugs of the scoring rules that fired.
    pub signals: &'a [&'a str],
    /// The human sentence the console shows.
    pub reason: &'a str,
}

/// Records or refreshes a batch of operator-review merge candidates, returning what happened to
/// each in canonical pair order.
///
/// The pair is stored in canonical id order and upserted against `merge_candidates_pair_key`, so
/// one ambiguity seen twice is one row and an operator's dismissal is durable. Storage order is
/// not merge direction: the survivor is the series carrying more of the work (see
/// [`MergeCandidateView::suggested_keep`]).
///
/// Only two states can be written over: an open row, and one the *scorer* closed as
/// `distinct`, which [`record_distinct_pairs`](super::record_distinct_pairs) keeps so a pair
/// judged apart before enrichment can come back. `dismissed` is a human's decision, and `merged`
/// and `auto_merged` have happened, so those stay untouched.
///
/// A pair naming a deleted series is dropped from the result, not an error. Both id columns
/// reference `series`, and one violating row would fail the whole statement, aborting a sweep
/// that judged pairs against facts loaded up front while merges deleted series throughout.
///
/// # Errors
/// [`crate::DbError::Conflict`] when a pair names one series twice, or when two entries name
/// the same canonical pair, which one `ON CONFLICT DO UPDATE` cannot touch twice. Otherwise
/// [`crate::DbError::Sqlx`].
pub async fn record_merge_candidates<'e, E: PgExecutor<'e>>(
    exec: E,
    candidates: &[NewMergeCandidate<'_>],
) -> DbResult<Vec<(SeriesId, SeriesId, QueueOutcome)>> {
    if candidates.is_empty() {
        return Ok(Vec::new());
    }
    let mut seen = std::collections::HashSet::with_capacity(candidates.len());
    let mut rows = Vec::with_capacity(candidates.len());
    for candidate in candidates {
        let (a, b) = candidate.pair;
        if a == b {
            return Err(crate::error::DbError::Conflict(
                "cannot queue a series against itself".to_owned(),
            ));
        }
        if !seen.insert((a.min(b), a.max(b))) {
            return Err(crate::error::DbError::Conflict(
                "the same pair was queued twice in one batch".to_owned(),
            ));
        }
        rows.push(serde_json::json!({
            "id": Uuid::now_v7(),
            "a": a.as_uuid(),
            "b": b.as_uuid(),
            "score": candidate.score,
            "signals": candidate.signals,
            "reason": candidate.reason,
        }));
    }

    // `prior` reads the pre-insert snapshot, so it reports the state the upsert replaces; a
    // data-modifying CTE runs whether or not the outer query selects from it. `live` is the
    // foreign-key guard: two primary-key lookups, so a pair with a side merged away since it was
    // judged leaves the batch instead of failing it.
    let rows = sqlx::query!(
        "WITH input AS ( \
           SELECT d.id, LEAST(d.a, d.b) AS series_id, GREATEST(d.a, d.b) AS candidate_id, \
                  d.score, d.signals, d.reason \
             FROM jsonb_to_recordset($1::jsonb) AS d( \
                  id uuid, a uuid, b uuid, score real, signals text[], reason text) \
         ), live AS ( \
           SELECT i.* FROM input i \
             JOIN series lo ON lo.id = i.series_id \
             JOIN series hi ON hi.id = i.candidate_id \
         ), prior AS ( \
           SELECT l.series_id, l.candidate_id, mc.resolved, mc.outcome \
             FROM live l \
             LEFT JOIN merge_candidates mc \
               ON mc.series_id = l.series_id AND mc.candidate_id = l.candidate_id \
         ), upsert AS ( \
           INSERT INTO merge_candidates (id, series_id, candidate_id, score, signals, reason) \
           SELECT id, series_id, candidate_id, score, signals, reason FROM live \
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
         SELECT p.series_id AS \"series_id!\", p.candidate_id AS \"candidate_id!\", \
                CASE WHEN p.resolved IS NULL THEN 'added' \
                     WHEN NOT p.resolved THEN 'refreshed' \
                     WHEN p.outcome = 'distinct' THEN 'reopened' \
                     ELSE 'unchanged' END AS \"outcome!\" \
           FROM prior p",
        Json::Array(rows),
    )
    .fetch_all(exec)
    .await?;

    Ok(rows
        .into_iter()
        .map(|r| {
            (
                SeriesId::from_uuid(r.series_id),
                SeriesId::from_uuid(r.candidate_id),
                match r.outcome.as_str() {
                    "added" => QueueOutcome::Added,
                    "refreshed" => QueueOutcome::Refreshed,
                    "reopened" => QueueOutcome::Reopened,
                    _ => QueueOutcome::Unchanged,
                },
            )
        })
        .collect())
}

/// Records or refreshes a single operator-review merge candidate for the pair `{a, b}`.
///
/// The create-time ingest path resolves one series at a time, so it has exactly one pair to
/// record; every other caller batches. See [`record_merge_candidates`] for the semantics.
///
/// # Errors
/// As [`record_merge_candidates`].
pub async fn record_merge_candidate<'e, E: PgExecutor<'e>>(
    exec: E,
    a: SeriesId,
    b: SeriesId,
    score: f32,
    signals: &[&str],
    reason: &str,
) -> DbResult<QueueOutcome> {
    let outcomes = record_merge_candidates(
        exec,
        &[NewMergeCandidate {
            pair: (a, b),
            score,
            signals,
            reason,
        }],
    )
    .await?;
    // No row means the pair named a series that has ceased to exist since the caller resolved it.
    // Nothing was written, which is what `Unchanged` reports; every other case returns a row,
    // because the statement selects from a `LEFT JOIN` over its input.
    Ok(outcomes
        .into_iter()
        .next()
        .map_or(QueueOutcome::Unchanged, |(_, _, outcome)| outcome))
}

/// A pending merge candidate enriched with everything an operator needs to judge it without
/// opening both series (`docs/ARCHITECTURE.md` §11 `GET /v1/admin/merge-candidates`).
///
/// The counts decide which row *survives*: the one carrying more of the catalogue. The merge
/// unions everything either way, but absorbing the richer series destroys the id its bookmarks,
/// notifications and external mappings name.
#[derive(Debug, Clone, serde::Serialize)]
pub struct MergeCandidateView {
    /// The queue row, which is what a resolve call names.
    pub id: Uuid,
    /// One side of the pair, in canonical id order.
    pub series_id: SeriesId,
    /// Its canonical title.
    pub series_title: String,
    /// Providers carrying that side.
    pub series_sources: i64,
    /// Chapter links under that side.
    pub series_chapters: i64,
    /// The other side.
    pub candidate_id: SeriesId,
    /// Its canonical title.
    pub candidate_title: String,
    /// Providers carrying that side.
    pub candidate_sources: i64,
    /// Chapter links under that side.
    pub candidate_chapters: i64,
    /// Similarity in `[0,1]`, after every scoring term.
    pub score: f32,
    /// The stable slugs of the scoring rules that fired
    /// ([`tankovault_domain::matching::MatchSignals::labels`]).
    pub signals: Vec<String>,
    /// The sentence the console shows, `None` for a row from before it was stored.
    pub reason: Option<String>,
    /// Which of the two the console should offer to keep: the series with more sources, then
    /// more chapters, then the older id. Advisory, since `POST /v1/admin/series/merge` takes an
    /// explicit direction.
    pub suggested_keep: SeriesId,
    /// When the pair first entered the queue.
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: time::OffsetDateTime,
    /// When a sweep last re-scored the row in place.
    #[serde(with = "time::serde::rfc3339")]
    pub updated_at: time::OffsetDateTime,
}

/// Lists the open (unresolved) merge candidates, **highest confidence first**.
///
/// `created_at DESC` would bury certain duplicates under whatever the last scan observed.
/// `min_score` narrows further, so an operator can work the queue in confidence bands. Both
/// series are inner-joined, so a candidate naming a deleted series is absent; see
/// [`merge_series`](super::merge_series).
///
/// # Errors
/// [`crate::DbError::Sqlx`] only; an empty queue is an empty `Vec`.
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
            // More sources, then more chapters, then the lower id, so two identically sized
            // series get a deterministic answer.
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

/// Dismisses a merge candidate the operator judged distinct, without merging.
///
/// The dismissal is **durable**: the row is kept, resolved and marked `dismissed`, and
/// [`record_merge_candidate`] will not reopen it. The `outcome` column tells that apart from a
/// merge, which needs no suppression since one series is gone. An unknown id and a resolved one
/// both return `false`, so a double-click reports no failure.
///
/// # Errors
/// [`crate::DbError::Sqlx`] only; an unknown id and an already-resolved one are both `Ok(false)`,
/// not [`crate::DbError::NotFound`].
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

/// Suppresses a pair permanently without an existing queue row.
///
/// For bulk dismissal: an operator clearing a confidence band says none of these are
/// duplicates, and the standing sweep does not consult the queue for pairs it has never seen.
///
/// A pair with a deleted series suppresses nothing, since the sweep shortlists existing series
/// only. Flagging an unreverted merge asks for exactly that, and the guard keeps it from failing
/// on the foreign key.
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
         SELECT $1, LEAST($2::uuid, $3::uuid), GREATEST($2::uuid, $3::uuid), 0, \
                'operator marked distinct', true, 'dismissed', $4, now() \
          WHERE EXISTS (SELECT 1 FROM series WHERE id = $2::uuid) \
            AND EXISTS (SELECT 1 FROM series WHERE id = $3::uuid) \
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
