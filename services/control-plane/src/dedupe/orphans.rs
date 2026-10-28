//! Sourceless series: merged into a sourced twin when one exists, deleted when nothing refers to
//! them.

use tankovault_db::PgPool;
use tankovault_db::repo::matching::{self, NewMergeDecision, SeriesMatchFacts};
use tankovault_domain::UserId;
use tankovault_domain::matching::MergeVerdict;
use tankovault_matcher::Thresholds;
use uuid::Uuid;

use super::{
    journal::{JournalContext, JournalPayload, evidence_json, journal, policy_json, terms_json},
    pass::{best_reading, load_facts},
};

/// Orphan/twin pairs merged per batch of [`clean_up_orphans`].
const TWIN_BATCH: i64 = 500;

/// Unreferenced orphans deleted per statement of [`clean_up_orphans`].
const ORPHAN_DELETE_BATCH: i64 = 1_000;

/// Merge every sourceless series into its sourced twin, then delete the sourceless series nothing
/// refers to. Returns how many twins were merged.
///
/// Only the exhaustive run does this. The orphans are tens of thousands of copies left by a fixed
/// registration bug, and the scheduled run's merge ceiling would take weeks over them; each merge
/// here is still journalled with its undo, under the trigger `sweep_twin` and the reason
/// `orphan_twin`. See [`matching::find_orphan_twins`] for why the twin rule may ignore
/// `MAX_KEY_FANOUT`.
///
/// # Errors
///
/// Database failures. Merges and deletions already committed stay committed.
pub(super) async fn clean_up_orphans(
    pool: &PgPool,
    thresholds: Thresholds,
    actor: Option<UserId>,
) -> anyhow::Result<i64> {
    let ctx = JournalContext {
        sweep_id: Uuid::now_v7(),
        actor,
        // The thresholds in force, as every journal row carries them, so the replay harness can
        // read this row like any other.
        policy: policy_json(thresholds),
    };
    let mut merged = 0_i64;
    loop {
        let twins = matching::find_orphan_twins(pool, TWIN_BATCH).await?;
        let facts = load_facts(pool, &twins).await?;
        let mut progressed = false;
        for (orphan, twin) in twins {
            let (Some(drop), Some(keep)) = (facts.get(&orphan), facts.get(&twin)) else {
                continue;
            };
            // Re-read since the shortlist: a scan may have attached a source to the orphan.
            if drop.source_count != 0 || keep.source_count == 0 {
                continue;
            }
            merge_twin(pool, &ctx, keep, drop).await?;
            merged += 1;
            progressed = true;
        }
        if !progressed {
            break;
        }
    }

    let mut deleted = 0_i64;
    loop {
        let n = tankovault_db::repo::catalog::maintenance::delete_unreferenced_orphans(
            pool,
            ORPHAN_DELETE_BATCH,
        )
        .await?;
        deleted += n;
        if n == 0 {
            break;
        }
    }
    if merged > 0 || deleted > 0 {
        tracing::info!(merged, deleted, "cleaned up sourceless series");
    }
    Ok(merged)
}

/// Merge one sourceless series into its sourced twin and journal it with its undo.
async fn merge_twin(
    pool: &PgPool,
    ctx: &JournalContext,
    keep: &SeriesMatchFacts,
    drop: &SeriesMatchFacts,
) -> anyhow::Result<()> {
    let undo = matching::merge_series(
        pool,
        keep.series_id,
        drop.series_id,
        ctx.actor,
        "auto_merged",
    )
    .await?;
    let reading = best_reading(keep, drop, 1.0);
    let signals = reading.assessment.signals.labels();
    let payload = JournalPayload {
        outcome: "merged",
        terms: terms_json(&reading),
        evidence: evidence_json(keep, drop, 1.0, &reading, None, Some((keep, drop))),
    };
    tracing::info!(
        keep = %keep.series_id,
        dropped = %drop.series_id,
        title = %keep.canonical_title,
        undo_rows = undo.row_count(),
        "merged a sourceless series into its twin"
    );
    journal(
        pool,
        &[NewMergeDecision {
            sweep_id: Some(ctx.sweep_id),
            trigger: "sweep_twin",
            actor: ctx.actor,
            pair: (keep.series_id, drop.series_id),
            titles: (&keep.canonical_title, &drop.canonical_title),
            verdict: MergeVerdict::Auto.as_str(),
            reason: "orphan_twin",
            blocked_by: &[],
            outcome: payload.outcome,
            survivor_id: Some(keep.series_id),
            absorbed_id: Some(drop.series_id),
            score: reading.assessment.score,
            base_score: reading.base,
            signals: &signals,
            terms: &payload.terms,
            evidence: &payload.evidence,
            policy: &ctx.policy,
            undo: Some(&undo),
        }],
    )
    .await;
    Ok(())
}
