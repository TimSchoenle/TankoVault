//! The merge decision journal: what the duplicate sweep decided about a pair, why, and — for the
//! decisions that actually merged something — how to take it back.

use serde_json::Value as Json;
use sqlx::PgExecutor;
use time::OffsetDateTime;
use uuid::Uuid;

use crate::error::{DbError, DbResult};
use crate::repo::matching::undo::{MergeUndo, revert_merge};
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

/// One decision as the console renders it.
///
/// The undo journal is deliberately absent: it is the largest column in the table by an order of
/// magnitude (it carries every row of the absorbed series) and no list view needs it. `undo_rows`
/// carries the one fact a list *does* need from it — how much a revert would put back.
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
        reverted_at: Option<OffsetDateTime>,
        reverted_by: Option<Uuid>,
        revert_reason: Option<String>,
        flagged_at: Option<OffsetDateTime>,
        flagged_by: Option<Uuid>,
        flag_reason: Option<String>,
    }
    // `undo_rows` is counted in the database rather than by deserialising the journal: the point
    // of leaving `undo` out of the projection is not to ship it to the caller at all.
    let rows = sqlx::query_as!(
        Row,
        "SELECT d.id, d.decided_at, d.sweep_id, d.trigger, d.actor_id AS actor, \
                d.left_id, d.right_id, d.left_title, d.right_title, \
                d.verdict, d.reason, d.blocked_by, d.outcome, d.survivor_id, d.absorbed_id, \
                d.score, d.base_score, d.signals, d.terms, d.evidence, d.policy, \
                (d.undo IS NOT NULL AND d.reverted_at IS NULL) AS \"revertible!\", \
                COALESCE(( \
                  SELECT sum(jsonb_array_length(v)) \
                    FROM jsonb_each(COALESCE(d.undo, '{}'::jsonb)) AS e(k, v) \
                   WHERE jsonb_typeof(v) = 'array' \
                ), 0)::bigint AS \"undo_rows!\", \
                d.reverted_at, d.reverted_by, d.revert_reason, \
                d.flagged_at, d.flagged_by, d.flag_reason \
           FROM merge_decisions d \
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

    Ok(rows
        .into_iter()
        .map(|r| MergeDecisionRow {
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
            reverted_at: r.reverted_at,
            reverted_by: r.reverted_by,
            revert_reason: r.revert_reason,
            flagged_at: r.flagged_at,
            flagged_by: r.flagged_by,
            flag_reason: r.flag_reason,
        })
        .collect())
}

/// Undo the merge a decision performed, and suppress the pair so the sweep cannot re-make it.
///
/// # Why the suppression is part of the revert
///
/// Reverting alone puts the two series back and changes nothing about why they were merged: the
/// titles still agree, the score is still above the threshold, and the very next sweep merges
/// them again. An operator undoing a merge is stating that the two are different works, so the
/// revert records that as a durable dismissal — the same one the console's "not a duplicate"
/// button writes, and the one all three sweep shortlists exclude.
///
/// # Errors
/// [`DbError::NotFound`] when no such decision exists. [`DbError::Conflict`] when it carries no
/// undo journal (it merged nothing), when it has already been reverted, or when the absorbed id
/// is live again — see [`revert_merge`]. Otherwise [`DbError::Sqlx`].
pub async fn revert_merge_decision(
    pool: &sqlx::PgPool,
    id: Uuid,
    actor: Option<UserId>,
    reason: &str,
) -> DbResult<MergeUndo> {
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

    revert_merge(pool, &undo).await?;

    // Stamped after the revert commits. The other order would leave a decision marked reverted
    // by a transaction that then failed, which reads as "already undone" and blocks the retry.
    sqlx::query!(
        "UPDATE merge_decisions \
            SET reverted_at = now(), reverted_by = $2, revert_reason = $3 \
          WHERE id = $1",
        id,
        actor.map(UserId::as_uuid),
        reason,
    )
    .execute(pool)
    .await?;

    super::suppress_pair(
        pool,
        SeriesId::from_uuid(row.left_id),
        SeriesId::from_uuid(row.right_id),
        actor,
    )
    .await?;

    Ok(undo)
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
