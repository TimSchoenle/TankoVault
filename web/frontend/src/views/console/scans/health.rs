//! What the current filter matched, as figures: run outcomes, task success rate, open failures
//! and throughput, plus the per-provider breakdown behind them.
//!
//! This is the half of the panel the filter used to have no effect on at all. It is deliberately
//! computed from the *same* provider and window the row list uses, so narrowing to one provider
//! reports that provider's success rate rather than the deployment's.

use super::{duration_label, filters::ProviderOptions, percent, ScanFilter};
use crate::i18n::use_i18n;
use crate::models::{ProviderScanHealthView, ScanSummary};
use crate::util::{rel_time, thousands};
use crate::views::console::{use_console_nav, ConsoleQuery};
use dioxus::prelude::*;

/// A failure rate at or above this is called out rather than merely printed. Not a threshold the
/// system acts on — an operator's eye needs somewhere to land on a table of twenty providers.
const CONCERNING_FAILURE_RATE: f64 = 0.1;

/// Tasks settled per minute of run time, or `None` when nothing has run long enough to divide by.
fn throughput(summary: &ScanSummary) -> Option<f64> {
    const SECONDS_PER_MINUTE: f64 = 60.0;
    let minutes = summary.busy_seconds / SECONDS_PER_MINUTE;
    if minutes < f64::EPSILON {
        return None;
    }
    let settled = summary.tasks_done + summary.tasks_failed;
    #[expect(
        clippy::cast_precision_loss,
        reason = "a task count large enough to lose precision in an f64 is not reachable"
    )]
    Some(settled as f64 / minutes)
}

/// Settled tasks that succeeded, in `0.0..=1.0`, or `None` when nothing has settled.
fn success_ratio(done: i64, failed: i64) -> Option<f64> {
    let settled = done + failed;
    if settled <= 0 {
        return None;
    }
    #[expect(
        clippy::cast_precision_loss,
        reason = "a task count large enough to lose precision in an f64 is not reachable"
    )]
    Some(done as f64 / settled as f64)
}

/// The window's figures, and the per-provider table behind them.
#[component]
pub(super) fn HealthStrip(summary: ScanSummary, filter: ScanFilter) -> Element {
    let i18n = use_i18n();
    let slugs: Vec<String> = summary
        .providers
        .iter()
        .map(|provider| provider.slug.clone())
        .collect();

    let success = success_ratio(summary.tasks_done, summary.tasks_failed);
    let rate_label = success.map_or_else(
        || i18n.t("console.scan.health.noTasks"),
        |ratio| format!("{}%", percent(ratio)),
    );
    let rate_tone = match success {
        Some(ratio) if ratio < 1.0 - CONCERNING_FAILURE_RATE => "val acc",
        Some(_) => "val jade",
        None => "val",
    };
    let throughput_label = throughput(&summary).map_or_else(
        || i18n.t("time.unknown"),
        |rate| {
            i18n.args(
                "console.scan.detail.perMinute",
                &[("count", &format!("{rate:.1}"))],
            )
        },
    );

    rsx! {
        ProviderOptions { slugs }
        div { class: "ik-stat-row", style: "margin-top:14px;",
            div { class: "ik-stat",
                div { class: "lbl", {i18n.t("console.scan.health.runs")} }
                div { class: "val", "{thousands(summary.runs_total)}" }
                div { class: "ik-kpi-sub",
                    {
                        i18n.args(
                            "console.scan.health.runBreakdown",
                            &[
                                ("active", &thousands(summary.runs_queued + summary.runs_running)),
                                ("failed", &thousands(summary.runs_failed)),
                            ],
                        )
                    }
                }
            }
            div { class: "ik-stat",
                div { class: "lbl", {i18n.t("console.scan.health.successRate")} }
                div { class: "{rate_tone}", "{rate_label}" }
                div { class: "ik-kpi-sub",
                    {
                        i18n.args(
                            "console.scan.health.taskBreakdown",
                            &[
                                ("done", &thousands(summary.tasks_done)),
                                ("failed", &thousands(summary.tasks_failed)),
                            ],
                        )
                    }
                }
            }
            div { class: "ik-stat",
                div { class: "lbl", {i18n.t("console.scan.health.openFailures")} }
                div {
                    class: if summary.failures_open > 0 { "val acc" } else { "val jade" },
                    "{thousands(summary.failures_open)}"
                }
                div { class: "ik-kpi-sub", {i18n.t("console.scan.health.openFailuresSub")} }
            }
            div { class: "ik-stat",
                div { class: "lbl", {i18n.t("console.scan.health.throughput")} }
                div { class: "val", "{throughput_label}" }
                div { class: "ik-kpi-sub",
                    {
                        i18n.args(
                            "console.scan.health.busy",
                            &[("duration", &duration_label(i18n, summary.busy_seconds))],
                        )
                    }
                }
            }
        }
        ProviderHealthTable { providers: summary.providers, filter }
    }
}

/// Per-provider scan health, worst first. A row selects that provider into the filter, which is
/// what an operator wants next after spotting one — every figure on the panel then narrows to it.
#[component]
fn ProviderHealthTable(providers: Vec<ProviderScanHealthView>, filter: ScanFilter) -> Element {
    let i18n = use_i18n();
    let nav = use_console_nav();
    if providers.is_empty() {
        return rsx! {
            p { class: "ik-muted", style: "font-size:13px;margin:12px 0 0;",
                {i18n.t("console.scan.health.noProviders")}
            }
        };
    }
    rsx! {
        div { style: "margin-top:16px;",
            div { class: "ik-subhead", {i18n.t("console.scan.health.byProvider")} }
            div { class: "ik-tablewrap scroll",
                table { class: "ik-table ik-table-compact",
                    thead {
                        tr {
                            th { {i18n.t("console.scan.health.col.provider")} }
                            th { {i18n.t("console.scan.health.col.runs")} }
                            th { {i18n.t("console.scan.health.col.tasks")} }
                            th { {i18n.t("console.scan.health.col.failureRate")} }
                            th { {i18n.t("console.scan.health.col.open")} }
                            th { {i18n.t("console.scan.health.col.lastRun")} }
                        }
                    }
                    tbody {
                        for provider in providers {
                            ProviderHealthRow {
                                key: "{provider.slug}",
                                provider: provider.clone(),
                                selected: filter.provider.as_deref() == Some(provider.slug.as_str()),
                                on_pick: move |slug: String| {
                                    let query = nav.query();
                                    let already = query.provider.as_deref() == Some(slug.as_str());
                                    nav.filter(ConsoleQuery {
                                        provider: (!already).then_some(slug),
                                        ..query
                                    });
                                },
                            }
                        }
                    }
                }
            }
        }
    }
}

#[component]
fn ProviderHealthRow(
    provider: ProviderScanHealthView,
    selected: bool,
    on_pick: EventHandler<String>,
) -> Element {
    let i18n = use_i18n();
    let failure =
        success_ratio(provider.tasks_done, provider.tasks_failed).map(|ratio| 1.0 - ratio);
    let failure_pct = failure.map_or(0, percent);
    let tone = if failure.is_some_and(|ratio| ratio >= CONCERNING_FAILURE_RATE) {
        "color:var(--vermilion);"
    } else {
        ""
    };
    let slug = provider.slug.clone();
    rsx! {
        tr {
            // `ik-row-pick`, not the list pane's `ik-cons-row`: that one is `display: block`,
            // which collapses a `<tr>` and unhooks its cells from the column headings.
            class: if selected { "ik-row-pick selected" } else { "ik-row-pick" },
            tabindex: "0",
            role: "button",
            "aria-selected": if selected { "true" } else { "false" },
            onclick: move |_| on_pick.call(slug.clone()),
            td {
                div { class: "ik-mono", style: "font-size:12.5px;", "{provider.slug}" }
                div { class: "ik-muted", style: "font-size:11px;", "{provider.name}" }
            }
            td { class: "ik-mono", style: "font-size:12px;",
                "{thousands(provider.runs)}"
                if provider.runs_active > 0 {
                    span { class: "ik-muted", " ·{provider.runs_active}▶" }
                }
            }
            td { class: "ik-mono", style: "font-size:12px;",
                "{thousands(provider.tasks_done)}"
                if provider.tasks_failed > 0 {
                    span { style: "color:var(--vermilion);", " ·{thousands(provider.tasks_failed)}✗" }
                }
            }
            td {
                div { class: "ik-flex", style: "gap:8px;",
                    span { class: "ik-mono", style: "font-size:12px;min-width:4ch;{tone}",
                        if failure.is_some() { "{failure_pct}%" } else { "—" }
                    }
                    div { class: "ik-progress thin", style: "flex:1;min-width:48px;",
                        span { style: "width:{failure_pct}%;background:var(--vermilion);" }
                    }
                }
            }
            td { class: "ik-mono", style: "font-size:12px;{tone}",
                "{thousands(provider.failures_open)}"
            }
            td { class: "ik-muted ik-mono", style: "font-size:12px;",
                "{rel_time(i18n, provider.last_run_at.as_deref())}"
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{success_ratio, throughput};
    use crate::models::ScanSummary;

    fn a_summary(done: i64, failed: i64, busy: f64) -> ScanSummary {
        ScanSummary {
            runs_total: 1,
            runs_queued: 0,
            runs_running: 0,
            runs_completed: 1,
            runs_failed: 0,
            runs_cancelled: 0,
            tasks_total: done + failed,
            tasks_done: done,
            tasks_failed: failed,
            failures_open: failed,
            busy_seconds: busy,
            first_run_at: None,
            last_run_at: None,
            providers: Vec::new(),
        }
    }

    /// An idle deployment has no rate to report, and reporting one would be a division by zero
    /// rendered as `NaN%` — or worse, as `0%`, which reads as "everything is failing".
    #[test]
    fn an_idle_window_reports_no_rate_rather_than_zero() {
        assert_eq!(success_ratio(0, 0), None);
        assert_eq!(throughput(&a_summary(0, 0, 0.0)), None);
    }

    #[test]
    fn the_rates_divide_by_what_actually_settled() {
        assert_eq!(success_ratio(90, 10), Some(0.9));
        // 120 tasks over two minutes of run time.
        assert_eq!(throughput(&a_summary(100, 20, 120.0)), Some(60.0));
    }
}
