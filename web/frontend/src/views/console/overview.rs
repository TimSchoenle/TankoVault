//! System overview — the at-a-glance health of the whole system as a grid of KPI tiles.

use crate::api;
use crate::components::{async_block, Kpi};
use crate::i18n::use_i18n;
use crate::models::*;
use crate::util::thousands;
use crate::views::console::RefreshTick;
use dioxus::prelude::*;
use progenitor_client::ResponseValue;

/// System-wide KPI header — the at-a-glance health of the whole system.
#[component]
pub(super) fn SystemOverview(tick: RefreshTick) -> Element {
    let api = api::use_api();
    let i18n = use_i18n();
    let res = use_resource(move || {
        tick.track();
        let client = api.client();
        async move {
            client
                .system_stats()
                .send()
                .await
                .map(ResponseValue::into_inner)
                .map_err(|e| api::friendly_error(i18n, e))
        }
    });

    rsx! {
        section { style: "margin-bottom:18px;",
            {
                async_block(
                    &res,
                    tick.reload(),
                    104,
                    |s| {
                        let s = s.clone();
                        rsx! {
                            KpiGrid { stats: Signal::new(s) }
                        }
                    },
                )
            }
        }
    }
}

/// The grid of KPI tiles.
#[component]
pub(super) fn KpiGrid(stats: Signal<SystemStats>) -> Element {
    let i18n = use_i18n();
    let s = stats.read();
    let runs_accent = (if s.runs_active > 0 { "good" } else { "" }).to_owned();
    let fail_accent = (if s.tasks_failed_24h > 0 { "warn" } else { "" }).to_owned();
    let merge_accent = (if s.pending_merges > 0 { "warn" } else { "" }).to_owned();
    rsx! {
        div { class: "ik-kpis",
            Kpi {
                label: i18n.t("console.kpi.providers"),
                value: thousands(s.providers_total),
                sub: i18n.args(
                    "console.kpi.providersSub",
                    &[
                        ("active", &thousands(s.providers_active)),
                        ("unhealthy", &thousands(s.providers_unhealthy)),
                        ("disabled", &thousands(s.providers_disabled)),
                    ],
                ),
            }
            Kpi {
                label: i18n.t("console.kpi.series"),
                value: thousands(s.series_total),
                sub: i18n.args("console.kpi.seriesSub", &[("count", &thousands(s.sources_total))]),
            }
            Kpi {
                label: i18n.t("console.kpi.chapters"),
                value: thousands(s.chapters_total),
                sub: i18n.args("console.kpi.chaptersSub", &[("count", &thousands(s.chapters_7d))]),
            }
            Kpi {
                label: i18n.t("console.kpi.new24h"),
                value: thousands(s.chapters_24h),
                sub: i18n.args("console.kpi.new24hSub", &[("count", &thousands(s.chapters_1h))]),
            }
            Kpi {
                label: i18n.t("console.kpi.activeScans"),
                value: thousands(s.runs_active),
                sub: i18n.args("console.kpi.activeScansSub", &[("count", &thousands(s.runs_running))]),
                accent: runs_accent,
            }
            Kpi {
                label: i18n.t("console.kpi.queueDepth"),
                value: thousands(s.tasks_queued),
                sub: i18n.args("console.kpi.queueDepthSub", &[("count", &thousands(s.tasks_running))]),
            }
            Kpi {
                label: i18n.t("console.kpi.failures24h"),
                value: thousands(s.tasks_failed_24h),
                sub: i18n.t("console.kpi.failures24hSub"),
                accent: fail_accent,
            }
            Kpi {
                label: i18n.t("console.kpi.mergeQueue"),
                value: thousands(s.pending_merges),
                sub: i18n.t("console.kpi.mergeQueueSub"),
                accent: merge_accent,
            }
            Kpi {
                label: i18n.t("console.kpi.users"),
                value: thousands(s.users_total),
                sub: i18n.t("console.kpi.usersSub"),
            }
        }
    }
}
