//! The run history table and the drawer one row opens.

use super::{duration_label, elapsed_seconds, percent, scope_label};
use crate::api;
use crate::hooks::use_reload;
use crate::i18n::use_i18n;
use crate::models::{RunStateExt as _, ScanRun, ScanRunExt as _};
use crate::util::{rel_time, thousands};
use crate::views::console::run_state_pill;
use crate::views::console::use_console_nav;
use crate::wire::types::ScanRunId;
use dioxus::prelude::*;
use inkstone_ui::{Button, Size};
/// Recent runs under the current filter, with the drawer for the selected one.
#[component]
pub(super) fn RunHistory(runs: Vec<ScanRun>, total: i64, narrowed: bool) -> Element {
    let i18n = use_i18n();
    rsx! {
        RunDrawer {}
        div { style: "margin-top:16px;",
            div { class: "ik-flex", style: "justify-content:space-between;align-items:baseline;",
                div { class: "ik-subhead", {i18n.t("console.scans.recent")} }
                // Says what the list is a window on, so a page of 30 out of 400 does not read as
                // the whole history — and, under a filter, states that it is one.
                span { class: "ik-muted ik-mono", style: "font-size:11.5px;",
                    {
                        i18n.args(
                            if narrowed {
                                "console.scan.history.countFiltered"
                            } else {
                                "console.scan.history.count"
                            },
                            &[
                                ("shown", &thousands(i64::try_from(runs.len()).unwrap_or(i64::MAX))),
                                ("total", &thousands(total)),
                            ],
                        )
                    }
                }
            }
            if runs.is_empty() {
                p { class: "ik-muted", style: "font-size:13px;margin:6px 0 0;",
                    {
                        i18n.t(
                            if narrowed { "console.scan.history.noneMatch" } else { "console.scans.noRuns" },
                        )
                    }
                }
            } else {
                div { class: "ik-tablewrap",
                    table { class: "ik-table ik-table-compact",
                        thead {
                            tr {
                                th { {i18n.t("console.scans.col.state")} }
                                th { {i18n.t("console.scans.col.mode")} }
                                th { {i18n.t("console.scans.col.scope")} }
                                th { {i18n.t("console.scans.col.progress")} }
                                th { {i18n.t("console.scans.col.duration")} }
                                th { {i18n.t("console.scans.col.started")} }
                                th { {i18n.t("console.scans.col.finished")} }
                            }
                        }
                        tbody {
                            for run in runs {
                                RunHistoryRow { key: "{run.id}", run }
                            }
                        }
                    }
                }
            }
        }
    }
}

#[component]
fn RunHistoryRow(run: ScanRun) -> Element {
    let i18n = use_i18n();
    let nav = use_console_nav();
    let run_id = run.id;
    let selected = nav.query().sel.as_deref() == Some(run_id.to_string().as_str());
    let pct = percent(run.progress());
    let failed_pct = if run.total_tasks > 0 {
        percent(f64::from(run.failed_tasks) / f64::from(run.total_tasks))
    } else {
        0
    };
    let done_pct = pct.saturating_sub(failed_pct);
    let duration =
        elapsed_seconds(&run).map_or_else(|| i18n.t("time.unknown"), |s| duration_label(i18n, s));
    rsx! {
        tr {
            // `ik-row-pick`, not the list pane's `ik-cons-row`: that one is `display: block`,
            // which collapses a `<tr>` and unhooks its cells from the column headings.
            class: if selected { "ik-row-pick selected" } else { "ik-row-pick" },
            tabindex: "0",
            role: "button",
            "aria-selected": if selected { "true" } else { "false" },
            onclick: move |_| {
                // Toggle, so clicking the open run closes the drawer rather than doing nothing.
                let next = if selected { None } else { Some(run_id.to_string()) };
                nav.select(nav.query().with_selection(next));
            },
            td { span { class: run_state_pill(run.state), {i18n.t(run.state.label_key())} } }
            td { class: "ik-mono", "{run.mode:?}" }
            td { class: "ik-mono ik-muted", "{scope_label(i18n, &run)}" }
            td {
                div { class: "ik-flex", style: "gap:8px;",
                    span { class: "ik-mono", style: "font-size:12px;min-width:82px;",
                        "{run.done_tasks}/{run.total_tasks}"
                        if run.failed_tasks > 0 {
                            span { style: "color:var(--vermilion);", " ·{run.failed_tasks}✗" }
                        }
                    }
                    div { class: "ik-progress split", style: "flex:1;min-width:60px;",
                        span { style: "width:{done_pct}%;" }
                        span { class: "fail", style: "width:{failed_pct}%;" }
                    }
                }
            }
            td { class: "ik-muted ik-mono", style: "font-size:12px;", "{duration}" }
            td { class: "ik-muted ik-mono", style: "font-size:12px;",
                "{rel_time(i18n, run.started_at.as_deref())}"
            }
            td { class: "ik-muted ik-mono", style: "font-size:12px;",
                "{rel_time(i18n, run.finished_at.as_deref())}"
            }
        }
    }
}

/// The selected run, fetched by id.
///
/// Fetched rather than picked out of the list, because a run linked to from elsewhere may be
/// older than the window the list holds.
#[component]
fn RunDrawer() -> Element {
    let api = api::use_api();
    let i18n = use_i18n();
    let nav = use_console_nav();
    let reload = use_reload();
    let selected = nav.query().sel;

    let detail = use_resource(use_reactive!(|selected| {
        reload.track();
        let client = api.client();
        async move {
            let Some(id) = selected
                .as_deref()
                .and_then(|id| id.parse::<ScanRunId>().ok())
            else {
                return Ok(None);
            };
            client
                .get_scan()
                .run_id(id)
                .send()
                .await
                .map(|response| Some(response.into_inner()))
                .map_err(|e| api::friendly_error(i18n, e))
        }
    }));

    let Some(Ok(Some(run))) = detail.read().clone() else {
        return rsx! {};
    };

    let elapsed = elapsed_seconds(&run);
    let elapsed_label = elapsed.map_or_else(|| i18n.t("time.unknown"), |s| duration_label(i18n, s));
    let settled = f64::from(run.done_tasks + run.failed_tasks);
    let throughput = elapsed.filter(|seconds| *seconds > 0.0).map_or_else(
        || i18n.t("time.unknown"),
        |seconds| {
            let rate = settled / (seconds / 60.0);
            i18n.args(
                "console.scan.detail.perMinute",
                &[("count", &format!("{rate:.1}"))],
            )
        },
    );

    rsx! {
        div { class: "ik-tile", style: "margin-top:12px;",
            div { class: "ik-flex", style: "justify-content:space-between;align-items:center;gap:10px;",
                div { class: "ik-flex", style: "gap:8px;align-items:center;flex-wrap:wrap;",
                    span { class: run_state_pill(run.state), {i18n.t(run.state.label_key())} }
                    span { class: "ik-mono", style: "font-size:12px;", "{run.mode:?}" }
                    span { class: "ik-mono ik-muted", style: "font-size:12px;",
                        "{scope_label(i18n, &run)}"
                    }
                    span { class: "ik-mono ik-muted", style: "font-size:11.5px;", "{run.id}" }
                }
                Button {
                    size: Size::Xs,
                    on_click: move |_| nav.select(nav.query().with_selection(None)),
                    {i18n.t("common.close")}
                }
            }
            div { class: "ik-flex", style: "gap:16px;flex-wrap:wrap;margin-top:10px;",
                Fact {
                    label: i18n.t("console.scan.detail.tasks"),
                    value: format!("{}/{}", run.done_tasks, run.total_tasks),
                }
                Fact {
                    label: i18n.t("console.scan.detail.failed"),
                    value: run.failed_tasks.to_string(),
                }
                Fact { label: i18n.t("console.scan.detail.elapsed"), value: elapsed_label }
                Fact { label: i18n.t("console.scan.detail.throughput"), value: throughput }
                Fact {
                    label: i18n.t("console.scans.col.started"),
                    value: rel_time(i18n, run.started_at.as_deref()),
                }
                Fact {
                    label: i18n.t("console.scans.col.finished"),
                    value: rel_time(i18n, run.finished_at.as_deref()),
                }
            }
        }
    }
}

/// One labelled figure in the drawer.
#[component]
fn Fact(label: String, value: String) -> Element {
    rsx! {
        div {
            span { class: "ik-muted", style: "font-size:11px;display:block;", "{label}" }
            span { class: "ik-mono", style: "font-size:13px;", "{value}" }
        }
    }
}
