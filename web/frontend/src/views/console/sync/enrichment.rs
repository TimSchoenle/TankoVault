//! What the catalogue-wide metadata enrichment sweep did, and is doing.
//!
//! The sync service does two unrelated things with `AniList`, and until this panel only one of
//! them was visible anywhere: reconciling a *user's* list (the account rows above) and walking
//! the *catalogue* asking for cover art, descriptions, tags and release years for series no
//! account is involved in. The second reported itself into the container's log and nowhere else,
//! so "is enrichment working?" had no answer an operator could reach — and its two ways of doing
//! nothing (a sweep that ran and resolved nothing, versus one that never ran because no provider
//! offers public metadata) leave the catalogue looking identical.

use crate::api;
use crate::components::{async_view, ErrorLine, Kpi, SkeletonBlock};
use crate::hooks::use_reload;
use crate::i18n::use_i18n;
use crate::util::{rel_time, thousands};
use crate::wire::types::EnrichmentSweepView;
use dioxus::prelude::*;
use progenitor_client::ResponseValue;

/// How often the panel re-reads itself while a sweep is in flight.
///
/// The sweep writes its counters once per database page, so a faster tick would mostly re-fetch
/// numbers that had not moved.
const SWEEP_POLL_MS: u32 = 5_000;

#[component]
pub(super) fn EnrichmentPanel() -> Element {
    let api = api::use_api();
    let i18n = use_i18n();
    let reload = use_reload();

    let sweep = use_resource(move || {
        reload.track();
        let client = api.client();
        async move {
            client
                .enrichment_status()
                .send()
                .await
                .map(ResponseValue::into_inner)
                .map_err(|e| api::friendly_error(i18n, e))
        }
    });

    // Only while one is running; an idle sweep's figures cannot change between reads.
    let running = matches!(&*sweep.read_unchecked(), Some(Ok(view)) if view.running);
    use_future(move || async move {
        loop {
            crate::platform::sleep_ms(SWEEP_POLL_MS).await;
            if running {
                reload.bump();
            }
        }
    });

    rsx! {
        {
            async_view(
                &sweep,
                reload,
                || rsx! { SkeletonBlock { height: 160 } },
                |view| rsx! { SweepBody { view: view.clone() } },
            )
        }
    }
}

/// The figures themselves, once loaded.
#[component]
fn SweepBody(view: EnrichmentSweepView) -> Element {
    let i18n = use_i18n();

    // The one number that says whether the sweep is making progress at all. Everything else is a
    // snapshot of the last run; this is the backlog it is working through, and a figure that
    // does not fall between two visits is the signal that nothing is running.
    let backlog_accent = if view.never_checked > 0 { "warn" } else { "" }.to_owned();
    let unresolved_accent = if view.unresolved > view.enriched {
        "warn"
    } else {
        ""
    }
    .to_owned();

    let when = if view.running {
        i18n.args(
            "console.sync.enrich.startedAt",
            &[("when", &rel_time(i18n, view.started_at.as_deref()))],
        )
    } else {
        i18n.args(
            "console.sync.enrich.finishedAt",
            &[("when", &rel_time(i18n, view.finished_at.as_deref()))],
        )
    };

    rsx! {
        div { class: "ik-flex", style: "gap:8px;align-items:center;margin-bottom:12px;flex-wrap:wrap;",
            span {
                class: if view.running { "ik-pill run" } else { "ik-pill" },
                if view.running {
                    {i18n.t("console.sync.enrich.running")}
                } else {
                    {i18n.t("console.sync.enrich.idle")}
                }
            }
            span { class: "ik-mono ik-muted", style: "font-size:11.5px;", "{when}" }
        }

        // Said, not inferred from three zeroes. A deployment with no public-metadata provider
        // registered records exactly that sentence here, which is the difference between "the
        // sweep found nothing" and "the sweep cannot run".
        if let Some(error) = view.error.clone() {
            ErrorLine { message: i18n.args("console.sync.enrich.lastError", &[("message", &error)]) }
        }

        div { class: "ik-kpis",
            Kpi {
                label: i18n.t("console.sync.enrich.kpi.scanned"),
                value: thousands(i64::from(view.scanned)),
                sub: i18n.t("console.sync.enrich.kpi.scannedSub"),
            }
            Kpi {
                label: i18n.t("console.sync.enrich.kpi.enriched"),
                value: thousands(i64::from(view.enriched)),
                sub: i18n.t("console.sync.enrich.kpi.enrichedSub"),
            }
            Kpi {
                label: i18n.t("console.sync.enrich.kpi.unresolved"),
                value: thousands(i64::from(view.unresolved)),
                sub: i18n.t("console.sync.enrich.kpi.unresolvedSub"),
                accent: unresolved_accent,
            }
            Kpi {
                label: i18n.t("console.sync.enrich.kpi.backlog"),
                value: thousands(view.never_checked),
                sub: i18n.args(
                    "console.sync.enrich.kpi.backlogSub",
                    &[("total", &thousands(view.series_total))],
                ),
                accent: backlog_accent,
            }
            Kpi {
                label: i18n.t("console.sync.enrich.kpi.lastDay"),
                value: thousands(view.checked_last_day),
                sub: i18n.t("console.sync.enrich.kpi.lastDaySub"),
            }
        }
    }
}
