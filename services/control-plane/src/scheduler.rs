//! The background scheduler: fast and full scans, duplicate sweeps and model builds, each on its
//! own interval.

use std::time::Duration;

use tankovault_control_plane::config::SchedulerConfig;
use tankovault_control_plane::recsys;
use tankovault_domain::{Feature, Provider, ScanMode};
use tankovault_service::metrics::names::{
    RECSYS_BUILD_DURATION, RECSYS_BUILD_SERIES, RECSYS_MODEL_SERIES,
};
use tracing::Instrument as _;

use super::{
    AppState,
    maintenance::{
        maybe_prune_scan_history, maybe_reconcile, maybe_reconcile_unread,
        maybe_resolve_watchlist_imports, maybe_sweep_unlocks, maybe_verify_chapter_rollup,
        maybe_verify_series_browse,
    },
    planning::{plan_run, planned},
};
use crate::{dedupe, leader, policy};

/// Periodic scheduler loop.
///
/// Exits on `shutdown` rather than being killed mid-sweep: a severed sweep leaves
/// planned-but-unpublished runs behind, which the aggregator then waits on forever.
pub(super) async fn run_scheduler(
    state: AppState,
    cfg: SchedulerConfig,
    leadership: leader::Leadership,
    shutdown: tokio_util::sync::CancellationToken,
) {
    if cfg.fast_interval_secs == 0
        && cfg.full_interval_secs == 0
        && cfg.merge_sweep_interval_secs == 0
        && cfg.merge_sweep_rotation_interval_secs == 0
        && cfg.recsys_incremental_interval_secs == 0
        && cfg.recsys_full_interval_secs == 0
        && cfg.reconcile_interval_secs == 0
        && (cfg.scan_history_prune_interval_secs == 0 || cfg.scan_history_retention_days == 0)
        && cfg.unread_unlock_interval_secs == 0
        && cfg.unread_reconcile_interval_secs == 0
        && cfg.watchlist_import_resolve_interval_secs == 0
        && cfg.chapter_rollup_verify_interval_secs == 0
        && cfg.series_browse_verify_interval_secs == 0
    {
        tracing::info!("scheduler disabled");
        return;
    }
    let mut fast = interval_or_never(cfg.fast_interval_secs);
    let mut full = interval_or_never(cfg.full_interval_secs);
    let mut merge = interval_or_never(cfg.merge_sweep_interval_secs);
    let mut merge_rotation = interval_or_never(cfg.merge_sweep_rotation_interval_secs);
    let mut repair = interval_or_never(cfg.reconcile_interval_secs);
    let mut prune = interval_or_never(if cfg.scan_history_retention_days == 0 {
        0
    } else {
        cfg.scan_history_prune_interval_secs
    });
    let mut unlocks = interval_or_never(cfg.unread_unlock_interval_secs);
    let mut unread_repair = interval_or_never(cfg.unread_reconcile_interval_secs);
    let mut import_resolve = interval_or_never(cfg.watchlist_import_resolve_interval_secs);
    let mut rollup_verify = interval_or_never(cfg.chapter_rollup_verify_interval_secs);
    let mut rollup_cursor = None;
    let mut browse_verify = interval_or_never(cfg.series_browse_verify_interval_secs);
    let mut browse_cursor = None;
    let mut recsys_incremental = interval_or_never(cfg.recsys_incremental_interval_secs);
    let mut recsys_full = interval_or_never(cfg.recsys_full_interval_secs);

    loop {
        // Republished on every tick rather than on acquisition, so a replica that lost the
        // lock without noticing still reports the truth. Summed over the job this must be
        // exactly 1; 0 means nothing is planning scans and the pipeline is quietly stopped.
        metrics::gauge!("scheduler_leader").set(f64::from(u8::from(leadership.is_leader())));
        tokio::select! {
            () = shutdown.cancelled() => {
                tracing::info!("scheduler stopping");
                return;
            }
            () = tick(&mut fast) => maybe_sweep(&state, &leadership, ScanMode::Fast).await,
            () = tick(&mut full) => maybe_sweep(&state, &leadership, ScanMode::Full).await,
            () = tick(&mut merge) => {
                maybe_merge_sweep(&state, &leadership, dedupe::SweepScope::Full).await;
            }
            () = tick(&mut merge_rotation) => {
                maybe_merge_sweep(&state, &leadership, dedupe::SweepScope::Rotation).await;
            }
            () = tick(&mut repair) => maybe_reconcile(&state, &leadership).await,
            () = tick(&mut unlocks) => maybe_sweep_unlocks(&state, &leadership).await,
            () = tick(&mut unread_repair) => maybe_reconcile_unread(&state, &leadership).await,
            () = tick(&mut import_resolve) => {
                maybe_resolve_watchlist_imports(&state, &leadership).await;
            }
            () = tick(&mut rollup_verify) => {
                maybe_verify_chapter_rollup(&state, &leadership, &mut rollup_cursor).await;
            }
            () = tick(&mut browse_verify) => {
                maybe_verify_series_browse(&state, &leadership, &mut browse_cursor).await;
            }
            () = tick(&mut prune) => {
                maybe_prune_scan_history(&state, &leadership, cfg.scan_history_retention_days).await;
            }
            () = tick(&mut recsys_incremental) => {
                maybe_recsys_build(&state, &cfg, &leadership, false).await;
            }
            () = tick(&mut recsys_full) => {
                maybe_recsys_build(&state, &cfg, &leadership, true).await;
            }
        }
    }
}

/// Runs a model build only when this replica holds leadership *and* recommendations are on.
///
/// Leadership matters for the same reason it does for the duplicate sweep, and for one more:
/// a build claims `rec_build_state` and two replicas racing would have one of them do nothing
/// while believing it had run. The claim is the real mutual exclusion; leadership keeps the
/// wasted attempt from happening at all.
async fn maybe_recsys_build(
    state: &AppState,
    cfg: &SchedulerConfig,
    leadership: &leader::Leadership,
    full: bool,
) {
    let kind = if full { "full" } else { "incremental" };
    if !state.features.is_enabled(Feature::CatalogueRecommendations) {
        tracing::debug!(
            kind,
            "skipping model build; recommendations are switched off"
        );
        return;
    }
    if !leadership.is_leader() {
        tracing::debug!(kind, "skipping model build; not scheduler leader");
        return;
    }

    let tuning = recsys::BuildTuning::read(
        &state.tunables,
        cfg.recsys_batch,
        cfg.recsys_incremental_max,
    );
    let started = std::time::Instant::now();
    match recsys::build(&state.pool, tuning, full)
        .instrument(tracing::info_span!(
            "recsys_build",
            "sentry.op" = "cron",
            kind
        ))
        .await
    {
        Ok(Some(report)) => {
            tracing::info!(
                kind,
                generation = report.generation,
                series = report.series_built,
                vocabulary = report.vocabulary,
                dims = report.dense_dims,
                "recommendation model built"
            );
            metrics::counter!(RECSYS_BUILD_SERIES, "stage" => kind, "result" => "built")
                .increment(u64::try_from(report.series_built).unwrap_or(0));
            // A gauge takes an `f64`; the count is an `i64` that cannot be negative, and a
            // catalogue large enough to lose precision here is far past anything this build
            // completes in a day.
            #[expect(
                clippy::cast_precision_loss,
                reason = "series counts are far below f64's exact-integer range"
            )]
            metrics::gauge!(RECSYS_MODEL_SERIES, "table" => "series_embedding")
                .set(report.series_built.max(0) as f64);
        }
        // Not a failure: another replica holds the claim and is doing this run's work.
        Ok(None) => tracing::debug!(kind, "model build already in progress"),
        Err(e) => {
            tracing::warn!(kind, error = %e, "recommendation model build failed");
            metrics::counter!(RECSYS_BUILD_SERIES, "stage" => kind, "result" => "failed")
                .increment(1);
        }
    }
    metrics::histogram!(RECSYS_BUILD_DURATION, "stage" => kind)
        .record(started.elapsed().as_secs_f64());
}

/// Runs a duplicate sweep only when this replica holds leadership *and* automatic
/// merging is switched on.
///
/// Leadership matters more than for a scan sweep: two replicas merging the same pair
/// race on a destructive transaction, and the loser finds its series already gone.
async fn maybe_merge_sweep(
    state: &AppState,
    leadership: &leader::Leadership,
    scope: dedupe::SweepScope,
) {
    if !state.features.is_enabled(Feature::ScanningAutoMerge) {
        tracing::debug!("skipping duplicate sweep; automatic merging is switched off");
        return;
    }
    if !leadership.is_leader() {
        tracing::debug!("skipping duplicate sweep; not scheduler leader");
        return;
    }
    let budget = match scope {
        dedupe::SweepScope::Full => state.merge_budget,
        dedupe::SweepScope::Rotation => state.rotation_budget,
    };
    let started = std::time::Instant::now();
    let thresholds = policy::thresholds(&state.matching, &state.tunables);
    let sweep_span = tracing::info_span!(
        "merge_sweep",
        "sentry.op" = "cron",
        scope = match scope {
            dedupe::SweepScope::Full => "full",
            dedupe::SweepScope::Rotation => "rotation",
        },
    );
    match dedupe::sweep(&state.pool, thresholds, budget, scope, None)
        .instrument(sweep_span)
        .await
    {
        Ok(dedupe::SweepRun { report, .. }) => {
            tracing::info!(
                examined = report.pairs_examined,
                auto_merged = report.auto_merged,
                queued = report.queued,
                withdrawn = report.withdrawn,
                deferred = report.deferred,
                chains_deferred = report.chains_deferred,
                "duplicate sweep complete"
            );
            // `auto_merged` is the destructive one and the only arm bounded by the merge
            // ceiling, so it is worth watching on its own rather than as a sweep total.
            for (action, count) in [
                ("examined", report.pairs_examined),
                ("auto_merged", report.auto_merged),
                ("queued", report.queued),
                ("withdrawn", report.withdrawn),
                ("deferred", report.deferred),
                ("chains_deferred", report.chains_deferred),
            ] {
                // The report's counts are `i64` and cannot be negative; a saturating
                // conversion keeps a future signed field from wrapping into a huge counter
                // jump, which on a monotonic counter reads as a reset.
                metrics::counter!("merge_sweep_actions_total", "action" => action)
                    .increment(u64::try_from(count).unwrap_or(0));
            }
        }
        Err(e) => tracing::warn!(error = %e, "duplicate sweep failed"),
    }
    // Labelled by scope: a rotation pass and a full sweep have different costs by design, and
    // one histogram over both reads as bimodal noise rather than as either.
    metrics::histogram!(
        "merge_sweep_duration_seconds",
        "scope" => match scope {
            dedupe::SweepScope::Full => "full",
            dedupe::SweepScope::Rotation => "rotation",
        },
    )
    .record(started.elapsed().as_secs_f64());
}

/// Runs a sweep only when this replica holds scheduler leadership *and* scheduled scanning
/// is switched on.
///
/// Checked per sweep, not when the loop is built, so switching `scanning.scheduler` off
/// takes effect on the next tick without a redeploy, and resumes on its own when it's back.
async fn maybe_sweep(state: &AppState, leadership: &leader::Leadership, mode: ScanMode) {
    if !state.features.is_enabled(Feature::ScanningScheduler) {
        tracing::debug!(?mode, "skipping sweep; scheduled scanning is switched off");
        return;
    }
    // A scheduled *full* sweep is the expensive one. `scanning.full` lets an operator stop it
    // while leaving the cheap latest-feed pass running, which is the usual way to back off.
    if mode == ScanMode::Full && !state.features.is_enabled(Feature::ScanningFull) {
        tracing::debug!("skipping sweep; full catalogue scans are switched off");
        return;
    }
    if leadership.is_leader() {
        // The trace root for the whole scan pipeline: the tasks this sweep publishes carry it
        // onto the workers, and their progress events carry it back to the aggregator.
        sweep(state, mode)
            .instrument(tracing::info_span!(
                "scheduler_sweep",
                "sentry.op" = "cron",
                mode = mode.as_str(),
            ))
            .await;
    } else {
        tracing::debug!(?mode, "skipping sweep; not scheduler leader");
    }
}

/// Whether `provider` is still serving out a cooldown earned by consecutive failed runs.
///
/// Only the **sweep** consults this. An operator pressing "Scan now" is asking a question the
/// backoff exists to stop *us* asking, and the answer they want is the current one — so
/// [`trigger_scan`] goes straight to [`plan_run`] and a manual run also ends the streak the
/// moment it succeeds.
///
/// [`trigger_scan`]: super::trigger_scan
pub(super) async fn cooling_down(
    state: &AppState,
    provider: &Provider,
    mode: ScanMode,
) -> anyhow::Result<bool> {
    let streak = tankovault_db::repo::scans::failure_streak(&state.pool, provider.id, mode).await?;
    let Some(remaining) = state
        .backoff(mode)
        .remaining(streak, time::OffsetDateTime::now_utc())
    else {
        return Ok(false);
    };

    // At `debug`, not `warn`: once a provider is down this fires every tick, and the failure it
    // reports is already in the triage feed. The counter below is what carries it to a panel.
    tracing::debug!(
        provider = %provider.slug,
        ?mode,
        failures = streak.failures,
        remaining_secs = remaining.as_secs(),
        "skipping the sweep; provider is backing off after consecutive failed runs"
    );
    planned(&provider.slug, mode, "cooling_down");
    Ok(true)
}

pub(super) async fn sweep(state: &AppState, mode: ScanMode) {
    let started = std::time::Instant::now();
    let providers = match tankovault_db::repo::providers::list(&state.pool).await {
        Ok(p) => p,
        Err(e) => {
            tracing::warn!(error = %e, "scheduler: failed to list providers");
            return;
        }
    };
    for provider in providers
        .iter()
        .filter(|p| p.state == tankovault_domain::ProviderState::Active)
    {
        match cooling_down(state, provider, mode).await {
            Ok(false) => {}
            Ok(true) => continue,
            Err(e) => {
                // The streak is an optimisation on politeness, not a precondition for scanning:
                // a provider must not stop being scanned because one read failed.
                tracing::warn!(
                    provider = %provider.slug,
                    error = %e,
                    next = "planning the run anyway; the backoff resumes on the next sweep",
                    "scheduler: could not read the provider's failure streak"
                );
            }
        }
        if let Err(e) = plan_run(state, provider, mode).await {
            tracing::warn!(provider = %provider.slug, error = %e, "scheduler: plan failed");
            planned(&provider.slug, mode, "error");
        }
    }
    metrics::histogram!("scheduler_sweep_duration_seconds", "scan" => mode.as_str())
        .record(started.elapsed().as_secs_f64());
}

fn interval_or_never(secs: u64) -> Option<tokio::time::Interval> {
    (secs > 0).then(|| tokio::time::interval(Duration::from_secs(secs)))
}

pub(super) async fn tick(maybe: &mut Option<tokio::time::Interval>) {
    match maybe {
        Some(i) => {
            i.tick().await;
        }
        // Never resolves, so `select!` ignores a disabled cadence.
        None => std::future::pending::<()>().await,
    }
}
