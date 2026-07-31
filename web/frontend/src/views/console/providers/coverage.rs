//! The Coverage tab: catalogue footprint and freshness for one provider.

use crate::components::{EmptyBox, Kpi};
use crate::i18n::use_i18n;
use crate::models::ProviderStat;
use crate::util::{rel_time, thousands};
use dioxus::prelude::*;

/// What this provider actually carries.
#[component]
pub(super) fn CoverageTab(stat: Option<ProviderStat>) -> Element {
    let i18n = use_i18n();
    let Some(stat) = stat else {
        return rsx! {
            EmptyBox { message: i18n.t("console.providers.noStats") }
        };
    };
    rsx! {
        div { class: "ik-kpis",
            Kpi { label: i18n.t("console.stats.col.series"), value: thousands(stat.series_count), large: true }
            Kpi { label: i18n.t("console.stats.col.sources"), value: thousands(stat.source_count), large: true }
            Kpi { label: i18n.t("console.stats.col.chapters"), value: thousands(stat.chapter_count), large: true }
            Kpi { label: i18n.t("console.providers.blocked"), value: thousands(stat.blocked_sources), large: true }
            Kpi { label: i18n.t("console.stats.col.new24h"), value: thousands(stat.chapters_24h), large: true }
            Kpi { label: i18n.t("console.stats.col.new7d"), value: thousands(stat.chapters_7d), large: true }
        }
        div { class: "ik-meta-line", style: "margin-top:14px;",
            span {
                {
                    i18n.args(
                        "console.providers.lastScan",
                        &[("when", &rel_time(use_i18n(), stat.last_scanned_at.as_deref()))],
                    )
                }
            }
            span {
                {
                    i18n.args(
                        "console.providers.lastFullScan",
                        &[("when", &rel_time(use_i18n(), stat.last_full_scan_at.as_deref()))],
                    )
                }
            }
            span {
                {
                    i18n.args(
                        "console.providers.lastChapter",
                        &[("when", &rel_time(use_i18n(), stat.last_chapter_at.as_deref()))],
                    )
                }
            }
        }
    }
}
