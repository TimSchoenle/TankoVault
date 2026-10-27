//! The scheduler's housekeeping: dispatch reconciliation, rollup and browse verification, watchlist
//! import resolution, unlock and unread sweeps, and history pruning.

use std::time::Duration;

use tankovault_domain::Feature;
use tracing::Instrument as _;

use super::AppState;
use crate::{leader, reconcile};

/// Repairs dispatch that has drifted from the task table, on the leader only.
///
/// Deliberately **not** behind [`Feature::ScanningScheduler`]. That flag stops new scans being
/// planned; it is the switch an operator reaches for during an incident, which is exactly when
/// the runs already in flight most need to be able to finish. A repair creates no new work — it
/// republishes messages for rows that already exist and closes runs that are already over.
pub(super) async fn maybe_reconcile(state: &AppState, leadership: &leader::Leadership) {
    if !leadership.is_leader() {
        tracing::debug!("skipping reconciliation; not scheduler leader");
        return;
    }
    let started = std::time::Instant::now();
    // A trace root: this is a timer tick, not a request, so nothing is on the stack to be a
    // parent. `sentry-tracing` turns a root span into one transaction, which is what puts the
    // scan tasks this republishes under the sweep that republished them.
    if let Err(e) = reconcile::pass(state)
        .instrument(tracing::info_span!(
            "dispatch_reconcile",
            "sentry.op" = "cron"
        ))
        .await
    {
        tracing::warn!(error = %e, "scan dispatch reconciliation failed");
    }
    metrics::histogram!("scan_reconcile_duration_seconds").record(started.elapsed().as_secs_f64());
}

/// Stored unread rows recomputed per unlock pass; the rest wait for the next tick.
const UNLOCK_SWEEP_BATCH: i64 = 1_000;

/// Sources whose stored chapter counts are re-counted per verification pass.
const ROLLUP_VERIFY_BATCH: i64 = 2_000;

/// Re-counts the next batch of sources' chapters against `chapter_rollup`, on the leader only.
///
/// `cursor` walks the sources in id order across passes and wraps at the end. Drift is a writer
/// that bypassed the triggers; it is logged at `WARN`, counted, and rebuilt in the same pass.
pub(super) async fn maybe_verify_chapter_rollup(
    state: &AppState,
    leadership: &leader::Leadership,
    cursor: &mut Option<uuid::Uuid>,
) {
    if !leadership.is_leader() {
        return;
    }
    let check = match tankovault_db::repo::catalog::rollup::verify_batch(
        &state.pool,
        *cursor,
        ROLLUP_VERIFY_BATCH,
    )
    .instrument(tracing::info_span!(
        "chapter_rollup_verify",
        "sentry.op" = "cron"
    ))
    .await
    {
        Ok(check) => check,
        Err(e) => {
            tracing::warn!(error = %e, "chapter rollup verification failed");
            return;
        }
    };
    *cursor = check.resume_after;
    for (field, count) in [("total", check.total), ("recent", check.recent)] {
        metrics::counter!(tankovault_service::metrics::names::CHAPTER_ROLLUP_DRIFT, "field" => field)
            .increment(count);
    }
    if check.drifted() > 0 {
        tracing::warn!(
            checked = check.checked,
            total = check.total,
            recent = check.recent,
            "stored chapter counts had drifted from the chapters table; rebuilt"
        );
    }
}

/// Series whose browse projection is recomputed per verification pass.
const BROWSE_VERIFY_BATCH: i64 = 5_000;

/// Recomputes the next batch of series' browse keys against `series_browse`, on the leader only.
///
/// `cursor` walks the series in id order across passes and wraps at the end. Drift is a writer
/// the projection's triggers do not see; it is logged at `WARN`, counted, and repaired in the same
/// pass.
pub(super) async fn maybe_verify_series_browse(
    state: &AppState,
    leadership: &leader::Leadership,
    cursor: &mut Option<uuid::Uuid>,
) {
    if !leadership.is_leader() {
        return;
    }
    let check = match tankovault_db::repo::catalog::verify_projection(
        &state.pool,
        *cursor,
        BROWSE_VERIFY_BATCH,
    )
    .instrument(tracing::info_span!(
        "series_browse_verify",
        "sentry.op" = "cron"
    ))
    .await
    {
        Ok(check) => check,
        Err(e) => {
            tracing::warn!(error = %e, "series browse verification failed");
            return;
        }
    };
    *cursor = check.resume_after;
    for (field, count) in [("missing", check.missing), ("stale", check.stale)] {
        metrics::counter!(tankovault_service::metrics::names::SERIES_BROWSE_DRIFT, "field" => field)
            .increment(count);
    }
    if check.drifted() > 0 {
        tracing::warn!(
            checked = check.checked,
            missing = check.missing,
            stale = check.stale,
            "stored browse keys had drifted from the catalogue; repaired"
        );
    }
}

/// Pending watchlist-import entries retried per pass; the rest wait for the next tick.
const WATCHLIST_IMPORT_RESOLVE_BATCH: i64 = 500;

/// Attaches imported watchlist entries whose series a scan has since brought in, on the leader
/// only.
///
/// Skipped while [`Feature::TrackingWatchlist`] is off, so switching the watchlist off does not
/// leave it being written to behind the reader's back. Not behind the scanning scheduler: it plans
/// no scan, and entries should still attach from scans an operator triggers by hand.
pub(super) async fn maybe_resolve_watchlist_imports(
    state: &AppState,
    leadership: &leader::Leadership,
) {
    if !leadership.is_leader() || !state.features.is_enabled(Feature::TrackingWatchlist) {
        return;
    }
    let adult_feature_on = state.features.is_enabled(Feature::CatalogueAdultContent);
    match tankovault_db::repo::tracking::watchlist_pending_sweep(
        &state.pool,
        WATCHLIST_IMPORT_RESOLVE_BATCH,
        adult_feature_on,
    )
    .instrument(tracing::info_span!(
        "watchlist_import_resolve",
        "sentry.op" = "cron"
    ))
    .await
    {
        Ok(outcome) if outcome.attached > 0 => tracing::info!(
            examined = outcome.examined,
            attached = outcome.attached,
            "attached imported watchlist entries whose series are now in the catalogue"
        ),
        Ok(_) => {}
        Err(e) => tracing::warn!(error = %e, "watchlist import resolution pass failed"),
    }
}

/// Stored unread rows re-verified per reconciliation pass.
const UNREAD_RECONCILE_BATCH: i64 = 500;

/// Recomputes stored unread figures whose early-access unlock time has passed, on the leader only.
///
/// Not behind [`Feature::ScanningScheduler`]: it plans no scan, and a reader's counts must stay
/// right while scanning is switched off.
pub(super) async fn maybe_sweep_unlocks(state: &AppState, leadership: &leader::Leadership) {
    if !leadership.is_leader() {
        return;
    }
    match tankovault_db::repo::tracking::unread::sweep_unlocks(&state.pool, UNLOCK_SWEEP_BATCH)
        .instrument(tracing::info_span!(
            "unread_unlock_sweep",
            "sentry.op" = "cron"
        ))
        .await
    {
        Ok(0) => {}
        Ok(due) => {
            metrics::counter!(tankovault_service::metrics::names::UNREAD_UNLOCKS_SWEPT)
                .increment(due);
            tracing::debug!(due, "recomputed unread figures past their unlock time");
        }
        Err(e) => tracing::warn!(error = %e, "unread unlock sweep failed"),
    }
}

/// Re-verifies a batch of stored unread figures against the live computation, on the leader only.
///
/// Any drift it finds is a writer the database triggers do not cover; it is logged at `WARN` with
/// the per-column counts and repaired in the same pass.
pub(super) async fn maybe_reconcile_unread(state: &AppState, leadership: &leader::Leadership) {
    if !leadership.is_leader() {
        return;
    }
    let drift = match tankovault_db::repo::tracking::unread::reconcile_batch(
        &state.pool,
        UNREAD_RECONCILE_BATCH,
    )
    .instrument(tracing::info_span!(
        "unread_reconcile",
        "sentry.op" = "cron"
    ))
    .await
    {
        Ok(drift) => drift,
        Err(e) => {
            tracing::warn!(error = %e, "unread reconciliation failed");
            return;
        }
    };
    for (field, count) in [
        ("missing", drift.missing),
        ("unread_count", drift.unread_count),
        ("next_unread", drift.next_unread),
        ("totals", drift.totals),
        ("latest", drift.latest),
        ("next_unlock", drift.next_unlock),
    ] {
        metrics::counter!(tankovault_service::metrics::names::UNREAD_DRIFT, "field" => field)
            .increment(count);
    }
    if drift.drifted() > 0 {
        tracing::warn!(
            checked = drift.checked,
            missing = drift.missing,
            unread_count = drift.unread_count,
            next_unread = drift.next_unread,
            totals = drift.totals,
            latest = drift.latest,
            next_unlock = drift.next_unlock,
            "stored unread figures had drifted from the live computation; repaired"
        );
    }
}

/// Scan runs selected per pruning pass.
const PRUNE_RUN_BATCH: i64 = 200;

/// Task rows deleted per pruning pass: small enough that one `DELETE` holds its locks briefly.
const PRUNE_TASK_BATCH: i64 = 5_000;

/// How long one pruning tick may keep working before yielding to the scheduler's other sweeps.
///
/// A first pass over weeks of history is far larger than this; it catches up over later ticks.
const PRUNE_TICK_BUDGET: Duration = Duration::from_secs(20);

/// Deletes settled scan history past retention, on the leader only.
///
/// Not behind [`Feature::ScanningScheduler`], like [`maybe_reconcile`]: it plans no work, and an
/// operator stopping scans is the moment the triage tables most need to stay small.
pub(super) async fn maybe_prune_scan_history(
    state: &AppState,
    leadership: &leader::Leadership,
    retention_days: u32,
) {
    if !leadership.is_leader() {
        tracing::debug!("skipping scan-history pruning; not scheduler leader");
        return;
    }
    let started = std::time::Instant::now();
    let mut total = tankovault_db::repo::scans::HistoryPrune::default();
    let span = tracing::info_span!("scan_history_prune", "sentry.op" = "cron");
    while started.elapsed() < PRUNE_TICK_BUDGET {
        match tankovault_db::repo::scans::prune_scan_history(
            &state.pool,
            retention_days,
            PRUNE_RUN_BATCH,
            PRUNE_TASK_BATCH,
        )
        .instrument(span.clone())
        .await
        {
            Ok(pass) if pass.is_empty() => break,
            Ok(pass) => {
                total.tasks += pass.tasks;
                total.runs += pass.runs;
            }
            Err(e) => {
                tracing::warn!(error = %e, "scan-history pruning failed");
                break;
            }
        }
    }
    metrics::counter!(tankovault_service::metrics::names::SCAN_HISTORY_PRUNED, "kind" => "tasks")
        .increment(total.tasks);
    metrics::counter!(tankovault_service::metrics::names::SCAN_HISTORY_PRUNED, "kind" => "runs")
        .increment(total.runs);
    if !total.is_empty() {
        tracing::info!(
            tasks = total.tasks,
            runs = total.runs,
            retention_days,
            "pruned scan history"
        );
    }
    prune_deferred_merge_decisions(state, &span, started).await;
}

/// How long a `deferred` merge-journal row is kept. A deferral only says a run's merge ceiling
/// was spent; the pair is re-judged on the next run, so an old one records nothing still true.
const DEFERRED_DECISION_RETENTION_DAYS: u32 = 14;

/// `deferred` journal rows deleted per statement.
const PRUNE_DECISION_BATCH: i64 = 5_000;

/// Deletes `deferred` merge-journal rows past retention, inside what is left of the tick budget.
///
/// Runs on the scan-history pruning tick rather than its own: it is the same kind of
/// housekeeping, on the leader, and a first pass over a backlog catches up over later ticks.
async fn prune_deferred_merge_decisions(
    state: &AppState,
    span: &tracing::Span,
    started: std::time::Instant,
) {
    let mut deleted = 0_u64;
    while started.elapsed() < PRUNE_TICK_BUDGET {
        match tankovault_db::repo::matching::purge_deferred_decisions(
            &state.pool,
            DEFERRED_DECISION_RETENTION_DAYS,
            PRUNE_DECISION_BATCH,
        )
        .instrument(span.clone())
        .await
        {
            Ok(0) => break,
            Ok(n) => deleted += n,
            Err(e) => {
                tracing::warn!(error = %e, "merge-journal pruning failed");
                break;
            }
        }
    }
    if deleted > 0 {
        tracing::info!(
            deleted,
            retention_days = DEFERRED_DECISION_RETENTION_DAYS,
            "pruned deferred merge decisions"
        );
    }
}
