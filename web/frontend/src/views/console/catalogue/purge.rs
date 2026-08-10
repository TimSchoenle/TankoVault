//! The catalogue danger zone: drop every chapter, or empty the catalogue outright.
//!
//! Both run as a **loop** of batched calls rather than one request. The server can only take a
//! slice per call — a full catalogue cascades into a dozen tables and would outlast any request
//! timeout — so this panel drives the endpoint until it reports nothing left, showing the
//! running total as it goes. An operator who closes the tab half way leaves a smaller catalogue,
//! not a rolled-back no-op: the purge is resumable, and pressing it again continues.

use crate::api;
use crate::components::{use_step_up_gate, OutcomeLine, Section, StepUpGuard, TypeToConfirm};
use crate::hooks::{use_busy, use_outcome, Reload};
use crate::i18n::use_i18n;
use crate::util::thousands;
use crate::wire::types::{CatalogueSummary, PurgeRequest, PurgeScope};
use dioxus::prelude::*;
use progenitor_client::ResponseValue;

/// Safety stop on the batch loop.
///
/// A server that kept answering `done: false` without shrinking `remaining` would otherwise spin
/// this loop forever against a live deployment. At the server's batch size this covers a
/// catalogue far larger than any this software is aimed at; hitting it means something is wrong,
/// and stopping with the count on screen is the honest outcome.
const MAX_BATCHES: u32 = 5_000;

/// Progress through a running purge.
#[derive(Clone, Copy, Default, PartialEq)]
struct Progress {
    removed: i64,
    remaining: i64,
    running: bool,
}

/// The two purges, each stating its blast radius from the live totals.
#[component]
pub(super) fn PurgePanel(totals: Option<CatalogueSummary>, reload: Reload) -> Element {
    let api = api::use_api();
    let i18n = use_i18n();
    let gate = use_step_up_gate();
    let busy = use_busy();
    let mut outcome = use_outcome();
    let mut progress = use_signal(Progress::default);

    // `—` rather than `0` while the summary is in flight: a purge panel that claims the
    // catalogue holds nothing is the one wrong thing it could say here.
    let count = |value: Option<i64>| value.map_or_else(|| "—".to_owned(), thousands);
    let series_total = count(totals.as_ref().map(|t| t.series_total));
    let chapters_total = count(totals.as_ref().map(|t| t.chapters_total));
    let watchlist_total = count(totals.as_ref().map(|t| t.watchlist_entries));
    let progress_total = count(totals.as_ref().map(|t| t.progress_rows));

    let run = use_callback(move |scope: PurgeScope| {
        if !busy.claim() {
            return;
        }
        outcome.set(None);
        progress.set(Progress {
            running: true,
            ..Progress::default()
        });
        // Elevated, and built once for the whole loop: the grant outlives the batches, so a
        // purge that took a hundred calls asks for the second factor once rather than per batch.
        let client = gate.client(api);
        spawn(async move {
            let confirm = match scope {
                PurgeScope::Chapters => "chapters",
                PurgeScope::Everything => "everything",
            };
            let mut removed = 0_i64;
            let mut batches = 0_u32;
            loop {
                let call = client
                    .purge_catalogue()
                    .body(PurgeRequest {
                        scope,
                        confirm: confirm.to_owned(),
                    })
                    .send()
                    .await
                    .map(ResponseValue::into_inner);
                let batch = match call {
                    Ok(batch) => batch,
                    Err(e) => {
                        // Mid-loop as much as on the first call: a grant that lapses between
                        // batches leaves a half-emptied catalogue, and the operator needs the
                        // prompt to finish it rather than "you don't have permission".
                        if !gate.refused(api::Refusal::of(&e)) {
                            outcome.set(Some(Err(api::guarded_error(i18n, e))));
                        }
                        break;
                    }
                };
                removed += match scope {
                    PurgeScope::Chapters => batch.removed.chapters,
                    PurgeScope::Everything => batch.removed.series,
                };
                progress.set(Progress {
                    removed,
                    remaining: batch.remaining,
                    running: !batch.done,
                });
                batches += 1;
                if batch.done {
                    outcome.set(Some(Ok(i18n.args(
                        "console.catalogue.purgeDone",
                        &[("count", &thousands(removed))],
                    ))));
                    break;
                }
                if batches >= MAX_BATCHES {
                    outcome.set(Some(Err(i18n.t("console.catalogue.purgeStalled"))));
                    break;
                }
            }
            progress.with_mut(|p| p.running = false);
            busy.release();
            reload.bump();
        });
    });

    let live = *progress.read();
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
                    busy: busy.is_busy(),
                    on_confirm: move |()| gate.attempt(move || run.call(PurgeScope::Chapters)),
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
                    busy: busy.is_busy(),
                    on_confirm: move |()| gate.attempt(move || run.call(PurgeScope::Everything)),
                }
            }
            if live.running || live.removed > 0 {
                p { class: "ik-mono", style: "font-size:12px;margin:10px 0 0;color:var(--muted);",
                    {
                        i18n.args(
                            "console.catalogue.purgeProgress",
                            &[
                                ("done", &thousands(live.removed)),
                                ("left", &thousands(live.remaining)),
                            ],
                        )
                    }
                }
            }
            StepUpGuard { gate, intro: Some(i18n.t("console.stepUp.intro")) }
            OutcomeLine { outcome: outcome.read().clone() }
        }
    }
}
