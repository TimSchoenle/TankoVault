//! The catalogue danger zone: drop every chapter, or empty the catalogue outright.
//!
//! Both start a detached run on the server and then read its progress back. The panel neither
//! drives the batches nor has to stay open: closing the tab leaves the purge running, and
//! reopening it shows where it got to. Stopping one is the cancel button, not navigation.

use crate::api;
use crate::components::{use_step_up_gate, OutcomeLine, Section, StepUpGuard, TypeToConfirm};
use crate::hooks::{use_busy, use_outcome, use_reload, Reload};
use crate::i18n::{use_i18n, Translator};
use crate::util::thousands;
use crate::wire::types::{
    CataloguePurgeStatus, CataloguePurgeStop, CatalogueSummary, PurgeRequest, PurgeScope,
};
use dioxus::prelude::*;
use inkstone_ui::{Button, Tone};
use progenitor_client::ResponseValue;

/// How often the panel re-reads a running purge.
const POLL_MS: u32 = 2_000;

/// The two purges, each stating its blast radius from the live totals, and the state of the
/// current or last run.
#[component]
pub(super) fn PurgePanel(totals: Option<CatalogueSummary>, reload: Reload) -> Element {
    let api = api::use_api();
    let i18n = use_i18n();
    let gate = use_step_up_gate();
    let busy = use_busy();
    let mut outcome = use_outcome();
    let tick = use_reload();

    let status = use_resource(move || {
        tick.track();
        let client = api.client();
        async move {
            client
                .catalogue_purge_status()
                .send()
                .await
                .map(ResponseValue::into_inner)
                .map_err(|e| api::friendly_error(i18n, e))
        }
    });

    // Polls only while a run is live. The flag is read inside the loop because this future runs
    // once; a copy taken at mount would be `false` for a run started from this panel. The first
    // idle read after a live one refreshes the totals above, which the run has just emptied.
    use_future(move || async move {
        let mut was_running = false;
        loop {
            crate::platform::sleep_ms(POLL_MS).await;
            let running = matches!(&*status.read_unchecked(), Some(Ok(view)) if view.running);
            if running {
                tick.bump();
            } else if was_running {
                reload.bump();
            }
            was_running = running;
        }
    });

    // `—` rather than `0` while the summary is in flight: a purge panel that claims the
    // catalogue holds nothing is the one wrong thing it could say here.
    let count = |value: Option<i64>| value.map_or_else(|| "—".to_owned(), thousands);
    let series_total = count(totals.as_ref().map(|t| t.series_total));
    let chapters_total = count(totals.as_ref().map(|t| t.chapters_total));
    let watchlist_total = count(totals.as_ref().map(|t| t.watchlist_entries));
    let progress_total = count(totals.as_ref().map(|t| t.progress_rows));

    let start = use_callback(move |scope: PurgeScope| {
        if !busy.claim() {
            return;
        }
        outcome.set(None);
        let client = gate.client(api);
        spawn(async move {
            let confirm = match scope {
                PurgeScope::Chapters => "chapters",
                PurgeScope::Everything => "everything",
            };
            let call = client
                .purge_catalogue()
                .body(PurgeRequest {
                    scope,
                    confirm: confirm.to_owned(),
                })
                .send()
                .await
                .map(ResponseValue::into_inner);
            match call {
                // A run already held the claim. Not an error: the line below is tracking it.
                Ok(start) if !start.started => {
                    outcome.set(Some(Ok(i18n.t("console.catalogue.purgeBusy"))));
                }
                Ok(_) => {}
                Err(e) => {
                    if !gate.refused(api::Refusal::of(&e)) {
                        outcome.set(Some(Err(api::guarded_error(i18n, e))));
                    }
                }
            }
            busy.release();
            tick.bump();
        });
    });

    let cancel = use_callback(move |()| {
        if !busy.claim() {
            return;
        }
        let client = gate.client(api);
        spawn(async move {
            if let Err(e) = client.cancel_catalogue_purge().send().await {
                if !gate.refused(api::Refusal::of(&e)) {
                    outcome.set(Some(Err(api::guarded_error(i18n, e))));
                }
            }
            busy.release();
            tick.bump();
        });
    });

    let view = match &*status.read() {
        Some(Ok(view)) => Some(view.clone()),
        _ => None,
    };
    let running = view.as_ref().is_some_and(|v| v.running);
    let cancelling = view.as_ref().is_some_and(|v| v.cancel_requested);
    let line = view.as_ref().and_then(|v| status_line(i18n, v));
    let locked = busy.is_busy() || running;

    rsx! {
        Section { label: i18n.t("console.catalogue.danger"),
            div { class: "ik-danger",
                TypeToConfirm {
                    title: i18n.t("console.catalogue.purgeChapters"),
                    body: i18n.args(
                        "console.catalogue.purgeChaptersWhy",
                        &[("chapters", &chapters_total)],
                    ),
                    expect: "chapters".to_owned(),
                    cta: i18n.t("console.catalogue.purgeChaptersCta"),
                    busy: locked,
                    on_confirm: move |()| gate.attempt(move || start.call(PurgeScope::Chapters)),
                }
                TypeToConfirm {
                    title: i18n.t("console.catalogue.purgeAll"),
                    body: i18n.args(
                        "console.catalogue.purgeAllWhy",
                        &[
                            ("series", &series_total),
                            ("chapters", &chapters_total),
                            ("watchers", &watchlist_total),
                            ("progress", &progress_total),
                        ],
                    ),
                    expect: "everything".to_owned(),
                    cta: i18n.t("console.catalogue.purgeAllCta"),
                    busy: locked,
                    on_confirm: move |()| gate.attempt(move || start.call(PurgeScope::Everything)),
                }
            }
            if let Some(line) = line {
                div {
                    class: "ik-flex",
                    style: "gap:10px;align-items:center;flex-wrap:wrap;margin:10px 0 0;",
                    p { class: "ik-mono", style: "font-size:12px;margin:0;color:var(--muted);",
                        "{line}"
                    }
                    if running && !cancelling {
                        Button {
                            tone: Tone::Danger,
                            busy: busy.is_busy(),
                            on_click: move |_| gate.attempt(move || cancel.call(())),
                            {i18n.t("console.catalogue.purgeCancel")}
                        }
                    }
                }
            }
            StepUpGuard { gate, intro: Some(i18n.t("console.stepUp.intro")) }
            OutcomeLine { outcome: outcome.read().clone() }
        }
    }
}

/// One line saying what the purge is doing, or how the last run ended. `None` before any run.
fn status_line(i18n: Translator, view: &CataloguePurgeStatus) -> Option<String> {
    let scope = view.scope?;
    let removed = thousands(match scope {
        PurgeScope::Chapters => view.removed.chapters,
        PurgeScope::Everything => view.removed.series,
    });
    let left = view.remaining.map_or_else(|| "—".to_owned(), thousands);
    let counts = [("done", removed.as_str()), ("left", left.as_str())];

    if view.running {
        let key = if view.cancel_requested {
            "console.catalogue.purgeCancelling"
        } else {
            "console.catalogue.purgeProgress"
        };
        return Some(i18n.args(key, &counts));
    }
    Some(match view.stopped? {
        CataloguePurgeStop::Done => i18n.args("console.catalogue.purgeDone", &counts),
        CataloguePurgeStop::Cancelled => i18n.args("console.catalogue.purgeCancelled", &counts),
        CataloguePurgeStop::Interrupted => i18n.args("console.catalogue.purgeInterrupted", &counts),
        CataloguePurgeStop::Stalled => i18n.args("console.catalogue.purgeStalled", &counts),
        CataloguePurgeStop::Failed => i18n.args(
            "console.catalogue.purgeFailed",
            &[("message", view.error.as_deref().unwrap_or("-"))],
        ),
    })
}
