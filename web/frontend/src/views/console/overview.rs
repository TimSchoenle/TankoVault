//! System overview — the at-a-glance health of the whole system as a grid of KPI tiles.

use crate::api;
use crate::models::*;
use crate::state::use_session;
use crate::util::thousands;
use crate::views::console::RefreshTick;
use dioxus::prelude::*;
use progenitor_client::ResponseValue;

/// System-wide KPI header — the at-a-glance health of the whole system.
#[component]
pub(super) fn SystemOverview(tick: RefreshTick) -> Element {
    let api = api::use_api();
    let session = use_session();
    let res = use_resource(move || {
        tick.track();
        let client = api.client();
        async move {
            if session.is_authenticated() {
                Some(
                    client
                        .system_stats()
                        .send()
                        .await
                        .map(ResponseValue::into_inner)
                        .map_err(api::friendly_error),
                )
            } else {
                None
            }
        }
    });

    let body = match &*res.read_unchecked() {
        None | Some(None) => rsx! { div { class: "ik-skeleton", style: "height:104px;" } },
        Some(Some(Err(e))) => {
            rsx! {
                p { class: "ik-muted", style: "font-size:13px;", "Stats unavailable: {e}" }
            }
        }
        Some(Some(Ok(s))) => {
            let s = s.clone();
            rsx! {
                KpiGrid { stats: Signal::new(s) }
            }
        }
    };

    rsx! {
        section { style: "margin-bottom:18px;", {body} }
    }
}

/// The grid of KPI tiles.
#[component]
pub(super) fn KpiGrid(stats: Signal<SystemStats>) -> Element {
    let s = stats.read();
    let runs_accent = (if s.runs_active > 0 { "good" } else { "" }).to_owned();
    let fail_accent = (if s.tasks_failed_24h > 0 { "warn" } else { "" }).to_owned();
    let merge_accent = (if s.pending_merges > 0 { "warn" } else { "" }).to_owned();
    rsx! {
        div { class: "ik-kpis",
            Kpi {
                label: "Providers",
                value: thousands(s.providers_total),
                sub: format!("{} active · {} unhealthy · {} off", s.providers_active, s.providers_unhealthy, s.providers_disabled),
                accent: "",
            }
            Kpi {
                label: "Series",
                value: thousands(s.series_total),
                sub: format!("{} source links", thousands(s.sources_total)),
                accent: "",
            }
            Kpi {
                label: "Chapters",
                value: thousands(s.chapters_total),
                sub: format!("+{} in 7d", thousands(s.chapters_7d)),
                accent: "",
            }
            Kpi {
                label: "New · 24h",
                value: thousands(s.chapters_24h),
                sub: format!("{} in the last hour", thousands(s.chapters_1h)),
                accent: "",
            }
            Kpi {
                label: "Active scans",
                value: thousands(s.runs_active),
                sub: format!("{} running now", thousands(s.runs_running)),
                accent: runs_accent,
            }
            Kpi {
                label: "Queue depth",
                value: thousands(s.tasks_queued),
                sub: format!("{} in flight", thousands(s.tasks_running)),
                accent: "",
            }
            Kpi {
                label: "Failures · 24h",
                value: thousands(s.tasks_failed_24h),
                sub: "tasks failed".to_owned(),
                accent: fail_accent,
            }
            Kpi {
                label: "Merge queue",
                value: thousands(s.pending_merges),
                sub: "pending review".to_owned(),
                accent: merge_accent,
            }
            Kpi {
                label: "Users",
                value: thousands(s.users_total),
                sub: "registered".to_owned(),
                accent: "",
            }
        }
    }
}

/// A single KPI tile: label, big value, and a supporting sub-line. `accent` is `""`,
/// `"good"`, or `"warn"`.
#[component]
pub(super) fn Kpi(label: String, value: String, sub: String, accent: String) -> Element {
    rsx! {
        div { class: "ik-kpi",
            div { class: "ik-kpi-label", "{label}" }
            div { class: "ik-kpi-value {accent}", "{value}" }
            if !sub.is_empty() {
                div { class: "ik-kpi-sub", "{sub}" }
            }
        }
    }
}
