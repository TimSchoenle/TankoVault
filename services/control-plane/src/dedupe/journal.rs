//! The `merge_decisions` rows a sweep writes: the itemised score, the deciding rule, the guards and
//! the undo.

use std::collections::{HashMap, HashSet};

use serde_json::json;
use tankovault_db::PgPool;
use tankovault_db::repo::matching::{
    self, DuplicatePair, MergeUndo, NewMergeDecision, QueueOutcome, SeriesMatchFacts,
};
use tankovault_domain::UserId;
use tankovault_domain::matching::LiveTiming;
use tankovault_matcher::{Adjudication, PairReading, Thresholds};
use uuid::Uuid;

use super::{
    judgement::{Action, Judged},
    pass::QueueWrite,
};

/// The parts of a journal row that are the same for every decision in a run.
pub(super) struct JournalContext {
    pub(super) sweep_id: Uuid,
    pub(super) actor: Option<UserId>,
    pub(super) policy: serde_json::Value,
}

/// The parts of a journal row derived from what actually happened to a pair.
pub(super) struct JournalPayload {
    pub(super) outcome: &'static str,
    pub(super) terms: serde_json::Value,
    pub(super) evidence: serde_json::Value,
}

impl Judged<'_> {
    /// The journal row for this judgement.
    pub(super) fn journal<'b>(
        &'b self,
        ctx: &'b JournalContext,
        payload: &'b JournalPayload,
        undo: Option<&'b MergeUndo>,
    ) -> NewMergeDecision<'b> {
        let (survivor, absorbed) = match self.action {
            Action::Merged { keep, drop } => (Some(keep.series_id), Some(drop.series_id)),
            _ => (None, None),
        };
        NewMergeDecision {
            sweep_id: Some(ctx.sweep_id),
            trigger: self.trigger.as_str(),
            actor: ctx.actor,
            pair: (self.left.series_id, self.right.series_id),
            titles: (&self.left.canonical_title, &self.right.canonical_title),
            verdict: self.verdict.verdict.as_str(),
            reason: self.verdict.reason,
            blocked_by: &self.verdict.blocked_by,
            outcome: payload.outcome,
            survivor_id: survivor,
            absorbed_id: absorbed,
            score: self.reading.assessment.score,
            base_score: self.reading.base,
            signals: &self.signals,
            terms: &payload.terms,
            evidence: &payload.evidence,
            policy: &ctx.policy,
            undo,
        }
    }

    /// The itemised score and both sides' facts, as the journal stores them.
    pub(super) fn payload(&self, outcome: &'static str) -> JournalPayload {
        let merged = match self.action {
            Action::Merged { keep, drop } => Some((keep, drop)),
            _ => None,
        };
        JournalPayload {
            outcome,
            terms: terms_json(&self.reading),
            evidence: evidence_json(
                self.left,
                self.right,
                self.trigram,
                &self.reading,
                self.live_timing,
                merged,
            ),
        }
    }
}

/// The journal payloads for everything the batched writes decided, each with the judgement it
/// belongs to.
///
/// Merges are absent: they were journalled as they happened. What is left out entirely is the
/// noise — a row re-scored to the same place every hour is not a decision, and re-confirming that
/// two unrelated series are unrelated on every run would bury the decisions that matter under
/// hundreds of rows a day that say nothing changed. A deferral is noise once the pair's latest
/// row already says it was deferred (`repeat_deferrals`).
///
/// The payloads are returned rather than turned into [`NewMergeDecision`]s here because those
/// borrow their payload, so it has to outlive the batch that names it.
pub(super) fn journal_payloads<'a>(
    judged: &'a [Judged<'a>],
    writes: &HashMap<DuplicatePair, QueueWrite>,
    repeat_deferrals: &HashSet<DuplicatePair>,
) -> Vec<(&'a Judged<'a>, JournalPayload)> {
    judged
        .iter()
        .filter(|j| {
            !(matches!(j.action, Action::Deferred) && repeat_deferrals.contains(&canonical(j.pair)))
        })
        .filter_map(|j| journal_outcome(j, writes).map(|outcome| (j, j.payload(outcome))))
        .collect()
}

/// A pair in the id order the journal stores it in.
pub(super) fn canonical((a, b): DuplicatePair) -> DuplicatePair {
    (a.min(b), a.max(b))
}

/// The outcome to journal for one judgement, or `None` when it is not worth a row.
fn journal_outcome(
    judged: &Judged<'_>,
    writes: &HashMap<DuplicatePair, QueueWrite>,
) -> Option<&'static str> {
    match judged.action {
        // Journalled inline, at the moment of the merge.
        Action::Merged { .. } => None,
        Action::Deferred => Some("deferred"),
        Action::Review => match writes.get(&judged.pair) {
            Some(QueueWrite::Reviewed(QueueOutcome::Added)) => Some("queued"),
            Some(QueueWrite::Reviewed(QueueOutcome::Reopened)) => Some("reopened"),
            // Only a guard makes a refresh worth journalling, because that is the near-miss an
            // operator is looking for.
            Some(QueueWrite::Reviewed(QueueOutcome::Refreshed)) => {
                (!judged.verdict.blocked_by.is_empty()).then_some("requeued")
            }
            _ => None,
        },
        Action::Distinct => match writes.get(&judged.pair) {
            Some(QueueWrite::Judged { was_open: true }) => Some("withdrawn"),
            _ => None,
        },
    }
}

/// Write a batch of journal rows.
///
/// Best-effort, like the sync history it sits beside: the record of a merge must not be able to
/// fail the merge. Batching moves what a failure costs — one statement now carries a run's
/// review and distinct decisions rather than one apiece — which is why the merges, the only rows
/// carrying an undo, are written separately as they happen.
pub(super) async fn journal(pool: &PgPool, decisions: &[NewMergeDecision<'_>]) {
    if decisions.is_empty() {
        return;
    }
    if let Err(e) = matching::record_merge_decisions(pool, decisions).await {
        tracing::warn!(
            error = %e,
            decisions = decisions.len(),
            "could not journal merge decisions",
        );
    }
}

/// The human sentence the review queue shows, naming the rule rather than the sweep.
pub(super) fn review_reason(verdict: &Adjudication) -> String {
    match verdict.reason {
        "no_structural_identity" => {
            "duplicate sweep: titles are similar but not the same string".to_owned()
        }
        "below_auto_merge_threshold" => {
            "duplicate sweep: identical titles, below the automatic-merge threshold".to_owned()
        }
        "alias_uncorroborated" => {
            "duplicate sweep: an alternative title matches, and nothing else confirms it".to_owned()
        }
        veto @ ("external_id_conflict" | "live_timing_conflict") => {
            format!("duplicate sweep: titles match, but {veto} says these are different works")
        }
        guard => format!("duplicate sweep: would have merged automatically, held back by {guard}"),
    }
}

/// The scorer's terms, as the decision journal stores them.
pub(super) fn terms_json(reading: &PairReading) -> serde_json::Value {
    serde_json::Value::Array(
        reading
            .terms
            .iter()
            .map(|t| json!({ "rule": t.rule, "delta": t.delta, "detail": t.detail }))
            .collect(),
    )
}

/// Both sides' facts, which title matched, and how the survivor was chosen.
pub(super) fn evidence_json(
    left: &SeriesMatchFacts,
    right: &SeriesMatchFacts,
    trigram: f32,
    reading: &PairReading,
    live_timing: Option<LiveTiming>,
    merged: Option<(&SeriesMatchFacts, &SeriesMatchFacts)>,
) -> serde_json::Value {
    let side = |f: &SeriesMatchFacts| {
        json!({
            "id": f.series_id,
            "title": f.canonical_title,
            "normalized_title": f.normalized_title,
            "alt_normalized_titles": f.alt_normalized_titles,
            "source_titles": f.source_titles,
            "anilist_id": f.anilist_id,
            "content_type": f.content_type.as_str(),
            "release_year": f.release_year,
            "tags": f.tags,
            "authors": f.authors,
            "sources": f.source_count,
            "chapters": f.chapter_count,
            "watchers": f.watcher_count,
        })
    };
    json!({
        "trigram_similarity": trigram,
        "scored_from": if reading.query_is_left { "left" } else { "right" },
        "matched_query_title": reading.matched_query_title,
        "matched_candidate_title": reading.matched_candidate_title,
        "via_alias": reading.via_alias,
        "left": side(left),
        "right": side(right),
        // Read back by the replay harness, which cannot re-measure it once chapters move.
        "live_timing": live_timing.map(|t| json!({
            "shared_live_chapters": t.shared_live_chapters,
            "median_gap_hours": t.median_gap_hours,
        })),
        // Why *this* id survived. The absorbed id stops existing, so the weights that chose it
        // are the part of the decision a reader with a broken bookmark will come looking for.
        "survivor_choice": merged.map(|(keep, drop)| json!({
            "kept": keep.series_id,
            "absorbed": drop.series_id,
            "kept_weight": { "sources": keep.source_count, "chapters": keep.chapter_count,
                             "watchers": keep.watcher_count },
            "absorbed_weight": { "sources": drop.source_count, "chapters": drop.chapter_count,
                                 "watchers": drop.watcher_count },
        })),
    })
}

/// The thresholds and guards in force, so a decision can be re-judged after they change.
pub(super) fn policy_json(thresholds: Thresholds) -> serde_json::Value {
    json!({
        "high": thresholds.high,
        "low": thresholds.low,
        "auto_merge": thresholds.auto_merge,
        "guards": {
            "numeric_conflict": thresholds.guards.numeric_conflict,
            "author_conflict": thresholds.guards.author_conflict,
            "year_conflict": thresholds.guards.year_conflict,
            "type_conflict": thresholds.guards.type_conflict,
        },
    })
}
