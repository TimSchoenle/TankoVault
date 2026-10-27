//! Planning a scan run: which providers it covers, and publishing its tasks to the bus.

use tankovault_contracts::{ScanTaskMessage, TaskKind};
use tankovault_domain::{Provider, RunState, ScanMode, ScanRunId};

use super::AppState;

/// Expand a run into its initial task(s) and dispatch them, unless the provider already has one
/// of this mode in flight — in which case that run's id is returned and nothing is queued.
///
/// ## Why planning is coalesced rather than queued
///
/// Nothing about a scan run bounds how long it takes: a fast scan is one `latest_feed` task that
/// re-ingests every series the feed names, at whatever rate that provider's crawl budget allows.
/// The sweep, by contrast, ticks on a fixed interval. A provider whose fast scan outlasts
/// `fast_interval_secs` therefore gained a second queued run every tick, indefinitely — and those
/// runs are *identical*, since each one re-reads the same feed.
///
/// The worker's per-provider lanes bound the damage but do not stop it: the backlog is served one
/// task at a time, so the provider spends every slot it is given re-reading a feed it has already
/// read, and its lane never empties. The fast tier is drained before the full tier is looked at
/// at all, so one such provider also stops every full scan in the deployment from being served.
///
/// Coalescing removes the cause. A provider has at most one in-flight run per mode, so a scan
/// that is slow simply keeps running, and the tick that would have duplicated it does nothing.
pub(super) async fn plan_run(
    state: &AppState,
    provider: &Provider,
    mode: ScanMode,
) -> anyhow::Result<ScanRunId> {
    if let Some(run_id) = tankovault_db::repo::scans::in_flight_run(
        &state.pool,
        provider.id,
        mode,
        state.run_stale_after,
    )
    .await?
    {
        tracing::info!(
            %run_id,
            provider = %provider.slug,
            ?mode,
            "skipped planning a scan run; the provider already has one in flight"
        );
        planned(&provider.slug, mode, "coalesced");
        return Ok(run_id);
    }

    let run_id =
        tankovault_db::repo::scans::create_run(&state.pool, Some(provider.id), mode).await?;

    match dispatch_run(state, provider, mode, run_id).await {
        Ok(result) => {
            tracing::info!(%run_id, provider = %provider.slug, ?mode, result, "planned scan run");
            planned(&provider.slug, mode, result);
            Ok(run_id)
        }
        Err(e) => {
            // The run row exists and would read as in flight forever, which now means it would
            // suppress this provider's every later run until it went stale. Failing it here is
            // what keeps one broker hiccup from costing a provider its whole schedule.
            if let Err(fail_err) =
                tankovault_db::repo::scans::finish_run(&state.pool, run_id, RunState::Failed).await
            {
                tracing::warn!(
                    %run_id,
                    provider = %provider.slug,
                    error = %fail_err,
                    next = "the run stays in flight and suppresses this provider's scans until it \
                            passes scheduler.run_stale_after_secs",
                    "could not fail the run whose dispatch failed"
                );
            }
            Err(e)
        }
    }
}

/// Persist and publish `run_id`'s initial task, naming which outcome it was for the log and the
/// counter.
async fn dispatch_run(
    state: &AppState,
    provider: &Provider,
    mode: ScanMode,
    run_id: ScanRunId,
) -> anyhow::Result<&'static str> {
    tankovault_db::repo::scans::start_run(&state.pool, run_id).await?;

    let (kind_str, kind, target) = match mode {
        ScanMode::Full => (
            "catalog_page",
            TaskKind::CatalogPage,
            serde_json::json!({ "page": 1 }),
        ),
        ScanMode::Fast => ("latest_feed", TaskKind::LatestFeed, serde_json::json!({})),
    };

    let Some(task_id) =
        tankovault_db::repo::scans::create_task(&state.pool, run_id, kind_str, &target).await?
    else {
        // A task with this exact target already exists for the run (idempotent replan);
        // the run is already dispatched, so there is nothing further to publish.
        return Ok("duplicate");
    };
    tankovault_db::repo::scans::add_total_tasks(&state.pool, run_id, 1).await?;

    state
        .bus
        .publish_task(&ScanTaskMessage {
            task_id,
            run_id,
            provider_id: provider.id,
            provider_slug: provider.slug.clone(),
            mode,
            kind,
            target,
            traceparent: None,
        })
        .await?;
    Ok("planned")
}

/// Count one planning decision.
///
/// The upstream half of the scan pipeline: `scan_tasks_served_total` on the worker says work
/// is being consumed, and only this says work is being *created*. A scheduler that is alive
/// but planning nothing — a lost leader, a flag left off — is otherwise indistinguishable
/// from an idle deployment.
///
/// `result` is one of `planned`, `duplicate`, `coalesced`, `cooling_down` or `error`. A provider
/// scanning more slowly than it is swept shows up as a steady stream of `coalesced` and no
/// `planned`, which is the signal to look at its crawl budget or its feed size rather than at the
/// scheduler. A steady stream of `cooling_down` is the other shape worth a panel: that provider
/// is not being scanned at all, and the reason is its own failure streak.
pub(super) fn planned(provider_slug: &str, mode: ScanMode, result: &'static str) {
    metrics::counter!(
        "scan_runs_planned_total",
        "provider" => provider_slug.to_owned(),
        "scan" => mode.as_str(),
        "result" => result,
    )
    .increment(1);
}
