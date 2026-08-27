//! The merge decision journal: what the duplicate sweep decided about a pair, why, and — for the
//! decisions that actually merged something — how to take it back.

use serde_json::Value as Json;
use sqlx::PgExecutor;
use time::OffsetDateTime;
use uuid::Uuid;

use crate::error::{DbError, DbResult};
use crate::repo::matching::disentangle::{Disentangled, disentangle};
use crate::repo::matching::undo::{MergeUndo, revert_merge_in};
use tankovault_domain::{SeriesId, UserId};

/// A decision to record. Borrowed throughout: every field comes from a value the caller already
/// holds, and a sweep records one of these per pair it acted on.
#[derive(Debug)]
pub struct NewMergeDecision<'a> {
    /// Groups every decision of one sweep run; `None` for an operator's console merge.
    pub sweep_id: Option<Uuid>,
    /// `sweep_new` | `sweep_requeue` | `sweep_recheck` | `operator`.
    pub trigger: &'a str,
    /// The operator behind an `operator` trigger, `None` for a sweep.
    pub actor: Option<UserId>,
    /// The pair in any order — [`record_merge_decisions`] puts it in canonical order itself.
    pub pair: (SeriesId, SeriesId),
    /// Both canonical titles, in the same order as `pair`, so the row still reads
    /// after one of the two series stops existing.
    pub titles: (&'a str, &'a str),
    /// `auto` | `review` | `distinct`, from `tankovault_matcher::Adjudication`.
    pub verdict: &'a str,
    /// The stable slug of the rule that produced the verdict.
    pub reason: &'a str,
    /// Guards that fired. Non-empty on a `review` means the pair cleared the score
    /// and identity bar and was held back anyway.
    pub blocked_by: &'a [&'a str],
    /// What was actually done, which is not always the verdict.
    pub outcome: &'a str,
    /// The series that survived, `None` for anything but a merge.
    pub survivor_id: Option<SeriesId>,
    /// The series that stopped existing, `None` for anything but a merge.
    pub absorbed_id: Option<SeriesId>,
    /// The final similarity in `[0,1]`, after every term in `terms`.
    pub score: f32,
    /// The similarity the score started from, before any bonus or penalty.
    pub base_score: f32,
    /// Stable slugs for the scoring rules that fired.
    pub signals: &'a [&'a str],
    /// `[{rule, delta, detail}]`, from `tankovault_matcher::Explanation::terms`.
    pub terms: &'a Json,
    /// Both sides' facts, which titles matched, and how the survivor was chosen.
    pub evidence: &'a Json,
    /// The thresholds and guards in force when the decision was taken.
    pub policy: &'a Json,
    /// The undo journal, for a decision that merged something. `None` means the decision
    /// cannot be reverted — which is correct for every outcome but `merged`.
    pub undo: Option<&'a MergeUndo>,
}

/// Record a batch of decisions in one statement, returning their ids in the order given.
///
/// # Why this is a batch and not a row at a time
///
/// A sweep journals one decision per pair it judges, and a run judges thousands. As one
/// statement per decision the journal cost the sweep more round trips than the scoring it
/// describes — and it is the part of the run nothing waits on, so paying latency for it row by
/// row bought nothing. The payload travels as one `jsonb` document rather than twenty-one
/// parallel arrays because four of the columns are themselves `jsonb` and two are `text[]`,
/// which `UNNEST` cannot carry without a jagged-array workaround per column.
///
/// # Errors
/// [`DbError::Conflict`] when a decision names one series twice, which the table's own check
/// constraint would reject anyway — returned here so the caller gets a domain error rather than
/// a driver one. [`DbError::Sqlx`] otherwise, including a failure to serialise an undo journal.
pub async fn record_merge_decisions<'e, E: PgExecutor<'e>>(
    exec: E,
    decisions: &[NewMergeDecision<'_>],
) -> DbResult<Vec<Uuid>> {
    if decisions.is_empty() {
        return Ok(Vec::new());
    }
    let mut ids = Vec::with_capacity(decisions.len());
    let mut rows = Vec::with_capacity(decisions.len());
    for decision in decisions {
        let id = Uuid::now_v7();
        rows.push(decision_row(id, decision)?);
        ids.push(id);
    }

    // The column definition list is what types the document; the `INSERT` column list is what
    // sqlx checks against the table, so a column renamed out from under this still fails to
    // compile.
    sqlx::query!(
        "INSERT INTO merge_decisions \
            (id, sweep_id, trigger, actor_id, left_id, right_id, left_title, right_title, \
             verdict, reason, blocked_by, outcome, survivor_id, absorbed_id, \
             score, base_score, signals, terms, evidence, policy, undo) \
         SELECT d.id, d.sweep_id, d.trigger, d.actor_id, d.left_id, d.right_id, \
                d.left_title, d.right_title, d.verdict, d.reason, d.blocked_by, d.outcome, \
                d.survivor_id, d.absorbed_id, d.score, d.base_score, d.signals, \
                d.terms, d.evidence, d.policy, d.undo \
           FROM jsonb_to_recordset($1::jsonb) AS d( \
                id uuid, sweep_id uuid, trigger text, actor_id uuid, \
                left_id uuid, right_id uuid, left_title text, right_title text, \
                verdict text, reason text, blocked_by text[], outcome text, \
                survivor_id uuid, absorbed_id uuid, score real, base_score real, \
                signals text[], terms jsonb, evidence jsonb, policy jsonb, undo jsonb)",
        Json::Array(rows),
    )
    .execute(exec)
    .await?;
    Ok(ids)
}

/// One decision as the row the batch insert reads back out of its `jsonb` payload.
fn decision_row(id: Uuid, decision: &NewMergeDecision<'_>) -> DbResult<Json> {
    let (a, b) = decision.pair;
    if a == b {
        return Err(DbError::Conflict(
            "cannot record a merge decision about one series".to_owned(),
        ));
    }
    // Canonical order, and the titles travel with their ids rather than with their argument
    // position — a swapped pair whose titles did not swap is a record that reads as nonsense.
    let ((left, left_title), (right, right_title)) = if a < b {
        ((a, decision.titles.0), (b, decision.titles.1))
    } else {
        ((b, decision.titles.1), (a, decision.titles.0))
    };

    let undo = decision
        .undo
        .map(serde_json::to_value)
        .transpose()
        .map_err(|e| DbError::Conflict(format!("undo journal is not serialisable: {e}")))?;

    Ok(serde_json::json!({
        "id": id,
        "sweep_id": decision.sweep_id,
        "trigger": decision.trigger,
        "actor_id": decision.actor.map(UserId::as_uuid),
        "left_id": left.as_uuid(),
        "right_id": right.as_uuid(),
        "left_title": left_title,
        "right_title": right_title,
        "verdict": decision.verdict,
        "reason": decision.reason,
        "blocked_by": decision.blocked_by,
        "outcome": decision.outcome,
        "survivor_id": decision.survivor_id.map(SeriesId::as_uuid),
        "absorbed_id": decision.absorbed_id.map(SeriesId::as_uuid),
        "score": decision.score,
        "base_score": decision.base_score,
        "signals": decision.signals,
        "terms": decision.terms,
        "evidence": decision.evidence,
        "policy": decision.policy,
        "undo": undo,
    }))
}

/// One segment of an undo journal: a table the revert would write to, and how many rows.
///
/// The key is the journal's own field name (`watchlist`, `moved_sources`, …) rather than a
/// display string, because the console must not need a release to name a segment a later journal
/// version adds.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct UndoSegment {
    /// The journal key, which is the table the rows belong to.
    pub kind: String,
    /// How many rows that key holds.
    pub rows: i64,
}

/// One decision as the console renders it.
///
/// The undo journal is deliberately absent: it is the largest column in the table by an order of
/// magnitude (it carries every row of the absorbed series) and no list view needs it. `undo_rows`
/// carries the one fact a list *does* need from it — how much a revert would put back — and
/// `undo_breakdown` the itemisation an operator needs before deciding to spend it.
#[derive(Debug, Clone, serde::Serialize)]
pub struct MergeDecisionRow {
    /// The journal row, which is what a revert or a flag names.
    pub id: Uuid,
    /// When the verdict was taken.
    #[serde(with = "time::serde::rfc3339")]
    pub decided_at: OffsetDateTime,
    /// Groups every decision of one sweep run, `None` for a console merge.
    pub sweep_id: Option<Uuid>,
    /// What produced the decision: a sweep pass, or an operator.
    pub trigger: String,
    /// The operator behind an `operator` trigger, `None` for a sweep.
    pub actor: Option<Uuid>,
    /// One side of the pair, in the canonical order the insert imposed.
    pub left_id: SeriesId,
    /// The other side.
    pub right_id: SeriesId,
    /// Left title as it read then, kept even after that series is absorbed.
    pub left_title: String,
    /// Right title as it read then.
    pub right_title: String,
    /// What the scorer concluded: `auto`, `review` or `distinct`.
    pub verdict: String,
    /// The stable slug of the rule that produced that verdict.
    pub reason: String,
    /// Guards that fired.
    pub blocked_by: Vec<String>,
    /// What was actually done, which is not always the verdict.
    pub outcome: String,
    /// The series that survived, `None` for anything but a merge.
    pub survivor_id: Option<SeriesId>,
    /// The series that stopped existing, `None` for anything but a merge.
    pub absorbed_id: Option<SeriesId>,
    /// The final similarity in `[0,1]`.
    pub score: f32,
    /// The similarity it started from, before any bonus or penalty.
    pub base_score: f32,
    /// Stable slugs for the scoring rules that fired.
    pub signals: Vec<String>,
    /// Every term the scorer applied, in order.
    pub terms: Json,
    /// Both sides' facts, which titles matched, and how the survivor was chosen.
    pub evidence: Json,
    /// The thresholds and guards in force when the decision was taken.
    pub policy: Json,
    /// Whether this decision still has an unspent undo journal.
    pub revertible: bool,
    /// How many rows a revert would restore or move back.
    pub undo_rows: i64,
    /// Those rows itemised by journal key, largest segment first, empty segments dropped.
    pub undo_breakdown: Vec<UndoSegment>,
    /// When the merge was undone, `None` while it stands.
    #[serde(with = "time::serde::rfc3339::option")]
    pub reverted_at: Option<OffsetDateTime>,
    /// Who undid it.
    pub reverted_by: Option<Uuid>,
    /// What they gave as the reason.
    pub revert_reason: Option<String>,
    /// When an operator marked it wrong, `None` if nobody has.
    #[serde(with = "time::serde::rfc3339::option")]
    pub flagged_at: Option<OffsetDateTime>,
    /// Who marked it.
    pub flagged_by: Option<Uuid>,
    /// What they gave as the reason.
    pub flag_reason: Option<String>,
}

/// How the console narrows the journal.
#[derive(Debug, Default, Clone)]
pub struct MergeDecisionFilter {
    /// Restrict to one outcome (`merged`, `queued`, `deferred`, …).
    pub outcome: Option<String>,
    /// Only decisions naming this series on either side. Survives the merge: an absorbed id is
    /// still on the row that absorbed it, which is the one an operator goes looking for.
    pub series_id: Option<SeriesId>,
    /// Only merges that can still be undone.
    pub revertible_only: bool,
    /// Only decisions an operator has flagged wrong.
    pub flagged_only: bool,
    /// Only decisions a guard held back — the near-misses.
    pub blocked_only: bool,
}

/// One row of the projection above, before the domain newtypes are put back on.
///
/// At module level rather than inside the query function because `query_as!` needs a named
/// struct and the conversion back is long enough to be worth its own item.
struct Row {
    id: Uuid,
    decided_at: OffsetDateTime,
    sweep_id: Option<Uuid>,
    trigger: String,
    actor: Option<Uuid>,
    left_id: Uuid,
    right_id: Uuid,
    left_title: String,
    right_title: String,
    verdict: String,
    reason: String,
    blocked_by: Vec<String>,
    outcome: String,
    survivor_id: Option<Uuid>,
    absorbed_id: Option<Uuid>,
    score: f32,
    base_score: f32,
    signals: Vec<String>,
    terms: Json,
    evidence: Json,
    policy: Json,
    revertible: bool,
    undo_rows: i64,
    undo_breakdown: Json,
    reverted_at: Option<OffsetDateTime>,
    reverted_by: Option<Uuid>,
    revert_reason: Option<String>,
    flagged_at: Option<OffsetDateTime>,
    flagged_by: Option<Uuid>,
    flag_reason: Option<String>,
}

impl From<Row> for MergeDecisionRow {
    fn from(r: Row) -> Self {
        Self {
            id: r.id,
            decided_at: r.decided_at,
            sweep_id: r.sweep_id,
            trigger: r.trigger,
            actor: r.actor,
            left_id: SeriesId::from_uuid(r.left_id),
            right_id: SeriesId::from_uuid(r.right_id),
            left_title: r.left_title,
            right_title: r.right_title,
            verdict: r.verdict,
            reason: r.reason,
            blocked_by: r.blocked_by,
            outcome: r.outcome,
            survivor_id: r.survivor_id.map(SeriesId::from_uuid),
            absorbed_id: r.absorbed_id.map(SeriesId::from_uuid),
            score: r.score,
            base_score: r.base_score,
            signals: r.signals,
            terms: r.terms,
            evidence: r.evidence,
            policy: r.policy,
            revertible: r.revertible,
            undo_rows: r.undo_rows,
            // The aggregate is built by the statement above, so a shape it cannot parse is a
            // bug in this file rather than bad data; an empty itemisation degrades to the
            // total, which the row already carries.
            undo_breakdown: serde_json::from_value(r.undo_breakdown).unwrap_or_default(),
            reverted_at: r.reverted_at,
            reverted_by: r.reverted_by,
            revert_reason: r.revert_reason,
            flagged_at: r.flagged_at,
            flagged_by: r.flagged_by,
            flag_reason: r.flag_reason,
        }
    }
}

/// A page of the journal, newest first.
///
/// # Errors
/// [`DbError::Sqlx`] only; no match is an empty `Vec`.
pub async fn list_merge_decisions<'e, E: PgExecutor<'e>>(
    exec: E,
    filter: &MergeDecisionFilter,
    limit: i64,
    offset: i64,
) -> DbResult<Vec<MergeDecisionRow>> {
    // `undo_rows` is counted in the database rather than by deserialising the journal: the point
    // of leaving `undo` out of the projection is not to ship it to the caller at all.
    //
    // The segment sizes go through a `CASE` rather than a `WHERE jsonb_typeof(...) = 'array'`
    // guard, and that is load-bearing: Postgres orders the clauses of one qual by cost, so a
    // guard standing beside `jsonb_array_length(e.v) > 0` in the same `WHERE` was planned
    // *behind* it and the statement died with `cannot get array length of a non-array` on the
    // journal's first non-array field. Nothing can reorder around a `CASE`. Reading both
    // aggregates out of one lateral also detoasts `undo` — by far the widest column in the
    // table — once per row rather than once per subquery.
    let rows = sqlx::query_as!(
        Row,
        "SELECT d.id, d.decided_at, d.sweep_id, d.trigger, d.actor_id AS actor, \
                d.left_id, d.right_id, d.left_title, d.right_title, \
                d.verdict, d.reason, d.blocked_by, d.outcome, d.survivor_id, d.absorbed_id, \
                d.score, d.base_score, d.signals, d.terms, d.evidence, d.policy, \
                (d.undo IS NOT NULL AND d.reverted_at IS NULL) AS \"revertible!\", \
                u.undo_rows AS \"undo_rows!\", u.undo_breakdown AS \"undo_breakdown!\", \
                d.reverted_at, d.reverted_by, d.revert_reason, \
                d.flagged_at, d.flagged_by, d.flag_reason \
           FROM merge_decisions d \
           CROSS JOIN LATERAL ( \
             SELECT COALESCE(sum(s.rows), 0)::bigint AS undo_rows, \
                    COALESCE(jsonb_agg( \
                               jsonb_build_object('kind', s.kind, 'rows', s.rows) \
                               ORDER BY s.rows DESC, s.kind \
                             ) FILTER (WHERE s.rows > 0), '[]'::jsonb) AS undo_breakdown \
               FROM ( \
                 SELECT e.k AS kind, \
                        CASE WHEN jsonb_typeof(e.v) = 'array' \
                             THEN jsonb_array_length(e.v) ELSE 0 END AS rows \
                   FROM jsonb_each(COALESCE(d.undo, '{}'::jsonb)) AS e(k, v) \
               ) AS s \
           ) AS u \
          WHERE ($3::text IS NULL OR d.outcome = $3) \
            AND ($4::uuid IS NULL OR d.left_id = $4 OR d.right_id = $4) \
            AND (NOT $5::boolean OR (d.undo IS NOT NULL AND d.reverted_at IS NULL)) \
            AND (NOT $6::boolean OR d.flagged_at IS NOT NULL) \
            AND (NOT $7::boolean OR cardinality(d.blocked_by) > 0) \
          ORDER BY d.decided_at DESC \
          LIMIT $1 OFFSET $2",
        limit,
        offset,
        filter.outcome.as_deref(),
        filter.series_id.map(SeriesId::as_uuid),
        filter.revertible_only,
        filter.flagged_only,
        filter.blocked_only,
    )
    .fetch_all(exec)
    .await?;

    Ok(rows.into_iter().map(MergeDecisionRow::from).collect())
}

/// Undo the merge a decision performed, suppress the pair, and take back what made the two look
/// like one.
///
/// # Why the revert is more than the inverse
///
/// Reverting alone puts the two series back and changes nothing about why they were merged, so
/// three things have to happen together or the operator's judgement does not survive the next
/// hour:
///
/// 1. The inverse — [`revert_merge`](super::revert_merge)'s statements — moves back exactly what
///    the merge moved.
/// 2. [`suppress_pair`](super::suppress_pair) records the durable dismissal — the same one the
///    console's "not a duplicate" button writes, and the one all three sweep shortlists exclude.
///    Without it the titles still agree, the score is still above the threshold, and the very
///    next sweep merges them again.
/// 3. The `disentangle` module takes the names the two rows share off the survivor and returns
///    the sources those names attracted. Suppression only binds the *sweep*; the create-time
///    attach path consults `series_titles` and nothing else, so a shared alias left in place has
///    the next scan re-attaching what the operator just separated — and the sources already filed
///    under it stay put, which is what makes a bare revert look like it did nothing at all.
///
/// # One transaction
///
/// All four writes, the journal stamp included. The stamp used to follow a committed revert, so a
/// failure between them left a restored catalogue with an unreverted decision row — and the retry
/// then failed on the live absorbed id, with no way forward but SQL.
///
/// # Errors
/// [`DbError::NotFound`] when no such decision exists. [`DbError::Conflict`] when it carries no
/// undo journal (it merged nothing), when it has already been reverted, or when the absorbed id
/// is live again — see [`revert_merge`](super::revert_merge). Otherwise [`DbError::Sqlx`].
pub async fn revert_merge_decision(
    pool: &sqlx::PgPool,
    id: Uuid,
    actor: Option<UserId>,
    reason: &str,
) -> DbResult<(MergeUndo, Disentangled)> {
    let row = sqlx::query!(
        "SELECT undo, reverted_at, left_id, right_id FROM merge_decisions WHERE id = $1",
        id,
    )
    .fetch_optional(pool)
    .await?
    .ok_or(DbError::NotFound)?;

    if row.reverted_at.is_some() {
        return Err(DbError::Conflict(
            "this merge decision has already been reverted".to_owned(),
        ));
    }
    let undo: MergeUndo = row
        .undo
        .map(serde_json::from_value)
        .transpose()
        .map_err(|e| DbError::Conflict(format!("undo journal is unreadable: {e}")))?
        .ok_or_else(|| {
            DbError::Conflict(
                "this decision merged nothing, so there is nothing to undo".to_owned(),
            )
        })?;

    let survivor = SeriesId::from_uuid(undo.survivor_id);
    let restored = SeriesId::from_uuid(undo.absorbed_id);

    let mut tx = pool.begin().await?;
    revert_merge_in(&mut tx, &undo).await?;
    // After the revert, which is what puts the restored row's titles back for it to read.
    let cleaned = disentangle(&mut tx, survivor, restored).await?;

    sqlx::query!(
        "UPDATE merge_decisions \
            SET reverted_at = now(), reverted_by = $2, revert_reason = $3 \
          WHERE id = $1",
        id,
        actor.map(UserId::as_uuid),
        reason,
    )
    .execute(&mut *tx)
    .await?;

    super::suppress_pair(
        &mut *tx,
        SeriesId::from_uuid(row.left_id),
        SeriesId::from_uuid(row.right_id),
        actor,
    )
    .await?;
    tx.commit().await?;

    Ok((undo, cleaned))
}

/// Mark a decision wrong, with the operator's reason, and suppress the pair.
///
/// Deliberately independent of the revert: a merge can be correct and still worth undoing as a
/// precaution, and a merge can be wrong and not worth the disruption of undoing — an operator
/// flagging it is tuning evidence either way. The suppression is unconditional, because a pair
/// judged wrong must not be re-merged whether or not the previous merge was taken back.
///
/// Returns whether this call was the one that flagged it.
///
/// # Errors
/// [`DbError::NotFound`] when no such decision exists; otherwise [`DbError::Sqlx`].
pub async fn flag_merge_decision(
    pool: &sqlx::PgPool,
    id: Uuid,
    actor: Option<UserId>,
    reason: &str,
) -> DbResult<bool> {
    let row = sqlx::query!(
        "UPDATE merge_decisions \
            SET flagged_at = now(), flagged_by = $2, flag_reason = $3 \
          WHERE id = $1 AND flagged_at IS NULL \
          RETURNING left_id, right_id",
        id,
        actor.map(UserId::as_uuid),
        reason,
    )
    .fetch_optional(pool)
    .await?;

    let Some(row) = row else {
        // Either unknown or already flagged; distinguish so a double click is not a 404.
        let exists = sqlx::query_scalar!(
            "SELECT count(*) AS \"count!\" FROM merge_decisions WHERE id = $1",
            id,
        )
        .fetch_one(pool)
        .await?;
        return if exists > 0 {
            Ok(false)
        } else {
            Err(DbError::NotFound)
        };
    };

    super::suppress_pair(
        pool,
        SeriesId::from_uuid(row.left_id),
        SeriesId::from_uuid(row.right_id),
        actor,
    )
    .await?;
    Ok(true)
}
