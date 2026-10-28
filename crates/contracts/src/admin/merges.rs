//! The duplicate sweep: candidates, sweep reports, the merge policy, decisions and their reverts.

use serde::{Deserialize, Serialize};
use serde_json::Value as Json;
use tankovault_domain::SeriesId;
use time::OffsetDateTime;
use utoipa::ToSchema;
use uuid::Uuid;

/// A pending merge candidate, enriched with everything the console needs to triage it without
/// opening both series (`docs/ARCHITECTURE.md` §11 `GET /v1/admin/merge-candidates`).
///
/// Acting on a candidate means choosing which row survives, and the absorbed id stops existing:
/// every bookmark, notification and tracker mapping naming it breaks. The counts and
/// `suggested_keep` let the server pick the series carrying more of the catalogue once, instead
/// of an operator comparing them per row.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct MergeCandidateView {
    /// The queue row, which is what a resolve call names.
    pub id: Uuid,
    /// One side of the pair.
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
    /// Similarity in `[0,1]` after every scoring term.
    pub score: f32,
    /// Stable slugs for the scoring rules that fired, such as `exact_title`, `compact_identity`
    /// and `alias_identity`. Rendered as badges; the set is
    /// `tankovault_domain::matching::MatchSignals::labels`.
    pub signals: Vec<String>,
    /// The rule that put the pair here, `null` for a row the journal does not cover.
    pub reason: Option<String>,
    /// Which side the console should offer to keep. Advisory: the merge endpoint takes an
    /// explicit direction.
    pub suggested_keep: SeriesId,
    /// When the pair first entered the queue.
    #[serde(with = "time::serde::rfc3339")]
    #[schema(value_type = String)]
    pub created_at: OffsetDateTime,
    /// When a sweep last re-scored the row in place.
    #[serde(with = "time::serde::rfc3339")]
    #[schema(value_type = String)]
    pub updated_at: OffsetDateTime,
}

/// Whether an exhaustive duplicate sweep was started.
///
/// Returned by `POST /v1/admin/merge-candidates/sweep-all`. The run is detached, so this says
/// only whether one started: it draws rounds until every shortlist is walked out, far longer
/// than a request may be held open. Its rounds, counters and outcome are on
/// `GET /v1/admin/merge-candidates/sweep-all`.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, ToSchema)]
pub struct MergeFullSweepView {
    /// `false` when a live run already held the claim. Watch that run instead of retrying: it
    /// is doing this one's work, and a second would spend the automatic-merge ceiling twice.
    pub started: bool,
}

/// What one duplicate-reconciliation sweep did.
///
/// Returned by `POST /v1/admin/merge-candidates/sweep` and logged by the scheduled sweep, so an
/// operator can see the effect of a threshold change on a single run before leaving it to the
/// schedule.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, ToSchema)]
pub struct MergeSweepView {
    /// Pairs re-scored. The shortlist is produced by blocking on the whitespace-insensitive
    /// title key, so this is far smaller than the number of series.
    pub pairs_examined: i64,
    /// Pairs merged without asking, because adjudication returned `Auto`. A corroborated alias
    /// counts here below the automatic-merge threshold too.
    pub auto_merged: i64,
    /// Pairs the sweep put in the review queue that had never been there.
    ///
    /// Counted apart from `requeued` because only these lengthen the queue: every run re-scores
    /// the rows already open, and one combined count would report a queue growing by hundreds
    /// when it grew by tens. The net change in queue length is `queued + reopened - withdrawn`.
    pub queued: i64,
    /// Rows already open that this sweep re-scored in place. They do not lengthen the queue.
    pub requeued: i64,
    /// Pairs the scorer had closed as distinct that enrichment has brought back into review.
    pub reopened: i64,
    /// Open queue rows removed because re-scoring with everything now known about both series
    /// put the pair below the review floor.
    pub withdrawn: i64,
    /// Pairs the sweep judged distinct and left alone.
    pub distinct: i64,
    /// Pairs skipped because the sweep's per-run automatic-merge budget was exhausted. Non-zero
    /// means the next sweep has more to do, not that anything failed.
    pub deferred: i64,
    /// Pairs an auto-merge guard held back: identical titles, a score above the automatic
    /// threshold, and a signal saying the two are different works anyway. Read these first:
    /// each is either a duplicate the guard is costing you or a wrong merge it prevented.
    pub blocked: i64,
    /// Pairs skipped because a merge earlier in this same run absorbed one of their two series.
    ///
    /// Their facts were loaded before that merge, so re-judging them would score the survivor as
    /// it was. A three-way duplicate therefore takes more than one pass. Non-zero means another
    /// pass has work, and the scheduler runs it without waiting for the next tick.
    pub chains_deferred: i64,
}

/// One knob of the automatic-merge policy, as the console shows it.
///
/// The control plane assembles it, because the effective value is the stored override layered
/// over this deployment's configured `matching` block, and only the control plane holds both.
/// Resolved against the compiled registry, it would report a default the sweep does not use.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[schema(as = MergePolicyKnob)]
pub struct MergePolicyView {
    /// The persisted key, such as `matching.auto_merge`.
    pub key: String,
    /// Section-local label.
    pub title: String,
    /// What the value does, written to be read immediately before someone changes production.
    pub description: String,
    // Tokens, not the domain enums: utoipa publishes a `///` as the public description, and a
    // rustdoc link to a crate the client cannot see is noise in a generated client.
    /// `ratio` for the threshold, rendered as a field, or `toggle` for a guard, rendered as a
    /// switch.
    pub kind: String,
    /// When a change reaches the sweep. Every knob here is `next_sweep`.
    pub applies: String,
    /// The effective value, already clamped: exactly what the next sweep will apply.
    pub value: f64,
    /// What this deployment falls back to when no override is stored: its configured `matching`
    /// value, which is the compiled default only when the deployment left it unset. Resetting
    /// the knob returns it to this value, so never show it as the compiled default.
    pub default_value: f64,
    /// Inclusive bounds. Enforced by the control plane, not only by the UI.
    pub min: f64,
    /// Inclusive upper bound.
    pub max: f64,
    /// Whether an operator set this knob explicitly. `false` means it follows the deployment's
    /// configuration.
    pub overridden: bool,
    /// Why it was last changed, if the operator said.
    pub note: Option<String>,
    /// Username of the operator who last changed it; `None` once that account is erased.
    pub updated_by: Option<String>,
    /// RFC 3339. Absent while the knob follows the configuration.
    pub updated_at: Option<String>,
}

/// What a normalized-key rebuild changed.
///
/// `normalized_title` is a persisted matching key, so until this runs, a change to the
/// normalization rules reaches only rows that are re-scanned.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, ToSchema)]
pub struct KeyRebuildView {
    /// Canonical titles read.
    pub series_scanned: i64,
    /// Of those, the ones whose key the current rules changed.
    pub series_updated: i64,
    /// Alternative titles read.
    pub titles_scanned: i64,
    /// Of those, the ones whose key the current rules changed.
    pub titles_updated: i64,
    /// Alternative titles dropped because the current rules collapsed them onto a key the same
    /// series already held.
    pub titles_deduplicated: i64,
    /// Source listing titles read.
    pub sources_scanned: i64,
    /// Of those, the ones whose listing key was missing or changed.
    pub sources_updated: i64,
}

/// One segment of a merge's undo journal.
///
/// `kind` is the journal's own field name, not a display string. A later journal version can add
/// a segment without a console release, and the wording lives on the reading side, where the
/// reader's language is known.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[schema(as = UndoSegment)]
pub struct UndoSegmentView {
    /// The journal key, which is the table the rows belong to.
    pub kind: String,
    /// How many rows that key holds.
    pub rows: i64,
}

/// One row of the automatic-merge decision journal.
///
/// The journal answers the question a score cannot: why. `terms` itemises how the number was
/// reached, `reason` names the rule that turned it into a verdict, `blocked_by` names the guards
/// that overrode it, and `evidence` carries both sides' facts and which titles matched.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[schema(as = MergeDecision)]
pub struct MergeDecisionView {
    /// The journal row, which is what a revert or a flag names.
    pub id: Uuid,
    /// When the verdict was taken.
    #[serde(with = "time::serde::rfc3339")]
    #[schema(value_type = String)]
    pub decided_at: OffsetDateTime,
    /// Groups every decision of one sweep run; absent for an operator's console merge.
    pub sweep_id: Option<Uuid>,
    /// `sweep_new` | `sweep_requeue` | `sweep_recheck` | `sweep_twin` | `operator`.
    pub trigger: String,
    /// The operator behind an `operator` trigger, `null` for a sweep.
    pub actor: Option<Uuid>,
    /// One side of the pair, as it was at decision time.
    pub left_id: SeriesId,
    /// The other side.
    pub right_id: SeriesId,
    /// Left title as it read then, kept even after the series is absorbed.
    pub left_title: String,
    /// Right title as it read then.
    pub right_title: String,
    /// `auto` | `review` | `distinct`.
    pub verdict: String,
    /// The stable slug of the rule that produced the verdict.
    pub reason: String,
    /// Guards that fired. Non-empty on a `review` row means the pair cleared the score and
    /// identity bar and was held back anyway.
    pub blocked_by: Vec<String>,
    /// What was done, which is not always the verdict: `merged`, `queued`, `requeued`,
    /// `reopened`, `withdrawn`, `distinct`, `deferred`.
    pub outcome: String,
    /// The series that survived, `null` for anything but a merge.
    pub survivor_id: Option<SeriesId>,
    /// The series that stopped existing, `null` for anything but a merge.
    pub absorbed_id: Option<SeriesId>,
    /// The final similarity in `[0,1]`, after every term in `terms`.
    pub score: f32,
    /// The similarity the score started from, before any bonus or penalty.
    pub base_score: f32,
    /// Stable slugs for the scoring rules that fired.
    pub signals: Vec<String>,
    /// Every term the scorer applied, in order, as `[{rule, delta, detail}]`.
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
    pub undo_breakdown: Vec<UndoSegmentView>,
    /// When the merge was undone, `null` while it stands.
    #[serde(with = "time::serde::rfc3339::option")]
    #[schema(value_type = Option<String>)]
    pub reverted_at: Option<OffsetDateTime>,
    /// Who undid it.
    pub reverted_by: Option<Uuid>,
    /// What they gave as the reason.
    pub revert_reason: Option<String>,
    /// When an operator marked the decision wrong, `null` if nobody has.
    #[serde(with = "time::serde::rfc3339::option")]
    #[schema(value_type = Option<String>)]
    pub flagged_at: Option<OffsetDateTime>,
    /// Who marked it.
    pub flagged_by: Option<Uuid>,
    /// What they gave as the reason.
    pub flag_reason: Option<String>,
}

/// What undoing an automatic merge put back, and what it took off the survivor.
///
/// The last three counts are not part of the merge's inverse. A revert says the two rows are
/// different works and acts on it: the names both rows answered to come off the survivor, and
/// the sources those names attracted go back with their chapters. They are reported because
/// this happens without review, so the survivor loses rows nobody listed first.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[schema(as = MergeReverted)]
pub struct MergeRevertedView {
    /// The journal row that was undone.
    pub decision_id: Uuid,
    /// The series that exists again, under its original id.
    pub restored_id: SeriesId,
    /// The series the rows went back off.
    pub survivor_id: SeriesId,
    /// Rows restored or moved back off the survivor.
    pub rows_restored: i64,
    /// Always true: a revert also suppresses the pair, or the next sweep would merge it again.
    pub pair_suppressed: bool,
    /// Alternative titles removed from the survivor because the restored series answers to them
    /// too. Left in place, they would let the next scan re-attach what the revert separated,
    /// since suppression binds the duplicate sweep, not the create-time attach path.
    pub titles_removed: i64,
    /// Sources moved off the survivor and back onto the restored series, because their provider
    /// title is one of those shared names.
    pub sources_returned: i64,
    /// Chapters that travelled with those sources. Chapters hang off sources, so this is what an
    /// operator sees change on the survivor's page.
    pub chapters_returned: i64,
}

/// What a batch revert did, or would do, with one decision.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
#[schema(as = MergeRevertBatchOutcome)]
pub enum MergeRevertBatchOutcomeView {
    /// Reverted. On a dry run: would be attempted.
    Reverted,
    /// No such decision.
    NotFound,
    /// Already reverted, or merged nothing.
    NothingToRevert,
    /// The revert was attempted and refused; `detail` says why.
    Refused,
}

/// One decision of a batch revert, in the order the batch handled it.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[schema(as = MergeRevertBatchItem)]
pub struct MergeRevertBatchItemView {
    /// The journal row.
    pub decision_id: Uuid,
    /// What happened to it.
    pub outcome: MergeRevertBatchOutcomeView,
    /// Why it was refused, when it was.
    pub detail: Option<String>,
    /// What was put back, for a decision a real run reverted.
    pub reverted: Option<MergeRevertedView>,
}

/// The result of a batch revert.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[schema(as = MergeRevertBatch)]
pub struct MergeRevertBatchView {
    /// Whether this was a dry run, which changes nothing.
    pub dry_run: bool,
    /// Decisions reverted, or on a dry run the ones that would be attempted.
    pub reverted: i64,
    /// Decisions not reverted, for any reason.
    pub skipped: i64,
    /// Every requested decision, newest first; unknown ids last.
    pub items: Vec<MergeRevertBatchItemView>,
}
