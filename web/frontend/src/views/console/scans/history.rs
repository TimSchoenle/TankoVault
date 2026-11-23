//! Owns the run history table and the drawer one row opens.

use super::{duration_label, elapsed_seconds, percent, run_state_ring, scope_label};
use crate::api;
use crate::components::{CompactPager, Window};
use crate::hooks::use_reload;
use crate::i18n::use_i18n;
use crate::models::{RunStateExt as _, ScanRun, ScanRunExt as _};
use crate::util::{rel_time, thousands};
use crate::views::console::run_state_pill;
use crate::views::console::{use_console_nav, ConsoleQuery};
use crate::wire::types::ScanRunId;
use dioxus::prelude::*;
use inkstone_ui::{Align, Button, Gap, Hint, Justify, Row, Size, Table, TableColumn};
/// Recent runs under the current filter, with the drawer for the selected one and a pager.
///
/// Paged, not capped: a capped panel shows whichever page the server's default hands it and
/// prints `30 of 743` beside it, with nothing that can reach the other 713.
#[component]
pub(super) fn RunHistory(
    runs: Vec<ScanRun>,
    total: i64,
    narrowed: bool,
    page: u32,
    page_size: i64,
) -> Element {
    let i18n = use_i18n();
    let nav = use_console_nav();
    let window = Window {
        offset: i64::from(page).saturating_mul(page_size),
        page_len: i64::try_from(runs.len()).unwrap_or(page_size),
        total,
    };
    rsx! {
        RunDrawer {}
        div { style: "margin-top:16px;",
            Row { gap: Gap::None, align: Align::Baseline, justify: Justify::Between,
                div { class: "ik-subhead", {i18n.t("console.scans.recent")} }
                // Says what the list is a window on, so a page of 30 out of 400 does not read as
                // the whole history, and, under a filter, states that it is one.
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
                Hint { above: Gap::Sm,
                    {
                        i18n.t(
                            if narrowed { "console.scan.history.noneMatch" } else { "console.scans.noRuns" },
                        )
                    }
                }
            } else {
                Table {
                    caption: i18n.t("console.scans.recent"),
                    scroll: true,
                    columns: vec![
                        TableColumn::new(i18n.t("console.scans.col.state")),
                        TableColumn::new(i18n.t("console.scans.col.mode")),
                        TableColumn::new(i18n.t("console.scans.col.scope")),
                        TableColumn::new(i18n.t("console.scans.col.progress")),
                        TableColumn::new(i18n.t("console.scans.col.duration")),
                        TableColumn::new(i18n.t("console.scans.col.started")),
                        TableColumn::new(i18n.t("console.scans.col.finished")),
                    ],
                    for run in runs {
                        RunHistoryRow { key: "{run.id}", run }
                    }
                }
            }
            if window.has_prev() || window.has_next() {
                CompactPager {
                    page: i64::from(page),
                    window,
                    on_page: move |next: i64| {
                        // Selection is dropped with the page: the drawer is open on a run this
                        // page no longer holds.
                        nav.select(ConsoleQuery {
                            sel: None,
                            ..nav.query().with_page(u32::try_from(next).unwrap_or(0))
                        });
                    },
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
                // Toggle, so clicking the open run closes the drawer instead of doing nothing.
                let next = if selected { None } else { Some(run_id.to_string()) };
                nav.select(nav.query().with_selection(next));
            },
            td {
                div { class: "ik-flex", style: "gap:7px;align-items:center;",
                    span { class: run_state_ring(run.state) }
                    span { class: run_state_pill(run.state), {i18n.t(run.state.label_key())} }
                }
            }
            td { class: "ik-mono", "{run.mode:?}" }
            td { class: "ik-mono ik-muted", "{scope_label(i18n, &run)}" }
            td {
                Row { gap: Gap::Sm, align: Align::Stretch,
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
/// Fetched, not picked out of the list, because a run linked to from elsewhere may be
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
            Row { gap: Gap::Md, justify: Justify::Between,
                Row { gap: Gap::Sm, wrap: true,
                    span { class: run_state_ring(run.state) }
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
            Row { gap: Gap::Lg, align: Align::Stretch, wrap: true, class: "ik-mt-md",
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
