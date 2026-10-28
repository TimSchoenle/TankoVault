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
/// The counts and `suggested_keep` are not decoration. Acting on a candidate means choosing
/// which of the two rows survives, and the absorbed id *stops existing* — every bookmark,
/// notification and external tracker mapping naming it breaks. The right answer is whichever
/// series carries more of the catalogue, which is a comparison the server can make once instead
/// of an operator eyeballing it per row.
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
    /// Stable slugs for the scoring rules that fired — `exact_title`, `compact_identity`,
    /// `alias_identity`, `near_identical`, `shared_author`, and so on. Rendered as badges; the
    /// set is `tankovault_domain::matching::MatchSignals::labels`.
    pub signals: Vec<String>,
    /// The rule that put the pair here, `null` for a row from before the journal existed.
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
/// Returned by `POST /v1/admin/merge-candidates/sweep-all`. The run is detached, so this answers
/// only whether one *started*: it draws rounds until every shortlist has been walked out, which
/// is far longer than a request may be held open for. What the run goes on to do — rounds,
/// counters and how it ended — is on `GET /v1/admin/merge-candidates/sweep-all`.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, ToSchema)]
pub struct MergeFullSweepView {
    /// `false` when a live run already held the claim. The correct response is to watch that
    /// run, not to retry: it is doing this one's work, and starting a second would spend the
    /// automatic-merge ceiling twice over.
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
    /// Pairs merged without asking, because a structural identity rule fired *and* the score
    /// cleared the automatic-merge threshold.
    pub auto_merged: i64,
    /// Pairs the sweep put in the review queue that had never been there.
    ///
    /// Counted apart from `requeued` because only these lengthen the queue: the sweep re-scores
    /// rows that are already open on every run, and folding those into one "queued" number
    /// reports a queue growing by hundreds when it grew by tens. Net change in queue length is
    /// `queued + reopened - withdrawn`.
    pub queued: i64,
    /// Rows already open that this sweep re-scored in place. The queue is no longer for these.
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
    /// threshold, and a signal saying the two are different works anyway. These are the near
    /// misses, and they are the rows to read first — each is either a duplicate the guard is
    /// costing you or a merge the guard just prevented.
    pub blocked: i64,
    /// Pairs skipped because a merge earlier in this same run absorbed one of their two series.
    ///
    /// Their facts were loaded before that merge and are now stale, so re-judging them here would
    /// score the survivor as it was rather than as it is. A three-way duplicate therefore takes
    /// more than one pass; non-zero means another pass has something to do, and the scheduler
    /// runs one rather than leaving the chain until the next tick.
    pub chains_deferred: i64,
}

/// One knob of the automatic-merge policy, as the console shows it.
///
/// Assembled by the control plane rather than by the API, because the effective value is the
/// stored override *layered over this deployment's configured* `matching` block — and the
/// control plane is the service that holds both. An API that resolved it against the compiled
/// registry instead would report a default the sweep does not use.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[schema(as = MergePolicyKnob)]
pub struct MergePolicyView {
    /// The persisted key, e.g. `matching.auto_merge`.
    pub key: String,
    /// Section-local label.
    pub title: String,
    /// What the value does, written to be read immediately before someone changes production.
    pub description: String,
    // Tokens rather than the domain enums: utoipa publishes a `///` as the public description,
    // and a rustdoc link to a crate the client cannot see is noise in a generated client.
    /// `ratio` for the threshold, `toggle` for a guard — what the console renders, a field or a
    /// switch.
    pub kind: String,
    /// When a change reaches the sweep. Every knob here is `next_sweep`.
    pub applies: String,
    /// The effective value, already clamped: exactly what the next sweep will apply.
    pub value: f64,
    /// What this deployment falls back to when no override is stored — its configured
    /// `matching` value, which is the compiled default unless the deployment set it. This is
    /// what resetting the knob returns it to, so it is not the same number as the compiled
    /// default and must not be shown as one.
    pub default_value: f64,
    /// Inclusive bounds. Enforced by the control plane, not only by the UI.
    pub min: f64,
    /// Inclusive upper bound.
    pub max: f64,
    /// Whether an operator has explicitly decided this one, as opposed to it following the
    /// deployment's configuration.
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
/// `normalized_title` is a persisted matching key, so a change to the normalization rules only
/// reaches rows that happen to be re-scanned until this runs.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, ToSchema)]
pub struct KeyRebuildView {
    /// Canonical titles read.
    pub series_scanned: i64,
    /// Of those, whose key the corrected rules changed.
    pub series_updated: i64,
    /// Alternative titles read.
    pub titles_scanned: i64,
    /// Of those, whose key the corrected rules changed.
    pub titles_updated: i64,
    /// Alternative titles dropped because the corrected rules collapsed them onto a key the
    /// same series already held.
    pub titles_deduplicated: i64,
    /// Source listing titles read.
    pub sources_scanned: i64,
    /// Of those, whose listing key was missing or changed.
    pub sources_updated: i64,
}

/// One segment of a merge's undo journal.
///
/// `kind` is the journal's own field name rather than a display string: a later journal version
/// can add a segment without the console needing a release to render it, and the wording lives
/// on the reading side where the reader's language is known.
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
/// The journal answers the question a score cannot: *why*. `terms` itemises how the number was
/// reached, `reason` names the rule that turned it into a verdict, `blocked_by` names the guards
/// that overrode it, and `evidence` carries both sides' facts and which title actually matched.
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
    /// What was actually done, which is not always the verdict: `merged`, `queued`, `requeued`,
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
    /// `[{rule, delta, detail}]` — every term the scorer applied, in order.
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
/// The last three counts are not part of the merge's inverse. A revert is an operator saying the
/// two rows are different works, and it acts on that: the names both rows answered to come off
/// the survivor, and the sources those names attracted go back with their chapters. Reported
/// because it happens without review — the survivor loses rows nobody listed first.
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
    /// too. Left in place, these have the next scan re-attaching what the revert just separated:
    /// suppression binds the duplicate sweep, not the create-time attach path.
    pub titles_removed: i64,
    /// Sources moved off the survivor and back onto the restored series, because their provider
    /// title *is* one of those shared names.
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
