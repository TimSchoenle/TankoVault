//! Per-provider statistics: catalogue footprint, content freshness and last-scan health.

use crate::api;
use crate::i18n::use_i18n;
use crate::models::*;
use crate::state::use_session;
use crate::util::{rel_time, thousands};
use crate::views::console::providers::HealthPill;
use crate::views::console::RefreshTick;
use dioxus::prelude::*;
use crate::components::{SkeletonBlock, EmptyBox};
use progenitor_client::ResponseValue;

/// Per-provider statistics table (read-only, auto-refreshing): catalogue footprint,
/// content freshness, and last-scan health for every provider at a glance.
#[component]
pub(super) fn ProviderStatsTable(tick: RefreshTick) -> Element {
    let api = api::use_api();
    let i18n = use_i18n();
    let session = use_session();
    let res = {
        use_resource(move || {
            tick.track();
            let client = api.client();
            async move {
                if session.is_authenticated() {
                    Some(
                        client
                            .provider_stats()
                            .send()
                            .await
                            .map(ResponseValue::into_inner)
                            .map_err(|e| api::friendly_error(i18n, e)),
                    )
                } else {
                    None
                }
            }
        })
    };

    let body = match &*res.read_unchecked() {
        None | Some(None) => rsx! { SkeletonBlock { height: 120 } },
        Some(Some(Err(e))) => {
            rsx! {
                p { class: "ik-muted", style: "font-size:13px;",
                    {i18n.args("console.stats.unavailable", &[("message", e)])}
                }
            }
        }
        Some(Some(Ok(list))) if list.is_empty() => rsx! {
            EmptyBox { message: i18n.t("console.stats.empty") }
        },
        Some(Some(Ok(list))) => {
            let rows = list.clone();
            rsx! {
                div { class: "ik-tablewrap",
                    table { class: "ik-table ik-table-compact",
                        thead {
                            tr {
                                th { {i18n.t("console.stats.col.provider")} }
                                th { {i18n.t("console.stats.col.adapter")} }
                                th { style: "text-align:right;", {i18n.t("console.stats.col.series")} }
                                th { style: "text-align:right;", {i18n.t("console.stats.col.sources")} }
                                th { style: "text-align:right;", {i18n.t("console.stats.col.chapters")} }
                                th { style: "text-align:right;", {i18n.t("console.stats.col.day")} }
                                th { style: "text-align:right;", {i18n.t("console.stats.col.week")} }
                                th { {i18n.t("console.stats.col.newest")} }
                                th { {i18n.t("console.stats.col.lastScan")} }
                                th { {i18n.t("console.stats.col.lastRun")} }
                            }
                        }
                        tbody {
                            for p in rows {
                                ProviderStatRow { key: "{p.provider_id}", stat: p }
                            }
                        }
                    }
                }
            }
        }
    };

    rsx! {
        section { style: "margin-bottom:18px;",
            h3 { {i18n.t("console.stats.title")} }
            {body}
        }
    }
}

#[component]
pub(super) fn ProviderStatRow(stat: ProviderStat) -> Element {
    let i18n = use_i18n();
    let s = stat;
    let blocked = if s.blocked_sources > 0 {
        i18n.args(
            "console.stats.blockedSources",
            &[("count", &thousands(s.blocked_sources))],
        )
    } else {
        String::new()
    };
    let last_run = match (&s.last_run_state, s.last_run_at.as_deref()) {
        (Some(state), at) => format!("{state} · {}", rel_time(i18n, at)),
        (None, _) => i18n.t("time.unknown"),
    };
    rsx! {
        tr {
            td {
                div { style: "font-weight:600;", "{s.name}" }
                div { class: "ik-flex", style: "gap:6px;margin-top:2px;",
                    HealthPill { state: s.state.clone() }
                    span { class: "ik-mono ik-muted", style: "font-size:11px;", "{s.slug}" }
                }
            }
            td { class: "ik-mono ik-muted", style: "font-size:12px;", "{s.adapter}" }
            td { class: "ik-mono", style: "text-align:right;", "{thousands(s.series_count)}" }
            td { class: "ik-mono", style: "text-align:right;",
                "{thousands(s.source_count)}"
                if !blocked.is_empty() {
                    span { class: "ik-muted", style: "font-size:11px;", "{blocked}" }
                }
            }
            td { class: "ik-mono", style: "text-align:right;", "{thousands(s.chapter_count)}" }
            td { class: "ik-mono", style: "text-align:right;",
                if s.chapters_24h > 0 {
                    span { style: "color:var(--jade);", "+{thousands(s.chapters_24h)}" }
                } else {
                    span { class: "ik-muted", "0" }
                }
            }
            td { class: "ik-mono ik-muted", style: "text-align:right;", "{thousands(s.chapters_7d)}" }
            td { class: "ik-muted ik-mono", style: "font-size:12px;",
                "{rel_time(i18n, s.last_chapter_at.as_deref())}"
            }
            td { class: "ik-muted ik-mono", style: "font-size:12px;",
                "{rel_time(i18n, s.last_scanned_at.as_deref())}"
            }
            td { class: "ik-muted ik-mono", style: "font-size:12px;", "{last_run}" }
        }
    }
}
