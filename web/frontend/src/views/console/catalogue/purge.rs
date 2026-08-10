//! The catalogue danger zone: drop every chapter, or empty the catalogue outright.
//!
//! Both run as a **loop** of calls rather than one request. A full catalogue cascades into a
//! dozen tables and would outlast any request timeout, so the endpoint deletes for as long as it
//! safely can and reports what is left; this panel drives it until nothing is, showing the
//! running total as it goes. An operator who closes the tab half way leaves a smaller catalogue,
//! not a rolled-back no-op: the purge is resumable, and pressing it again continues.
//!
//! The loop is also the one console interaction that can rate-limit *itself*, so it waits out a
//! `429` and carries on instead of reporting it. It used to make one call per 500 series against
//! a budget of thirty a minute, which meant it could not finish on any catalogue big enough to
//! need it; the endpoint's own deadline fixed the arithmetic, and this is the belt to that
//! braces — a console busy elsewhere can still spend the budget out from under it.

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

/// How long to wait when the server refuses with `429` but names no interval.
const WAIT_FALLBACK_MS: u32 = 5_000;

/// How many rate-limit waits one purge may sit through before giving up.
///
/// A budget the purge cannot get back — because something else is spending it, or because the
/// limiter is misconfigured — would otherwise keep this loop alive indefinitely behind a progress
/// line that never moves. Twenty waits is minutes of patience, which is the right amount for an
/// operation that legitimately takes minutes.
const MAX_WAITS: u32 = 20;

/// Progress through a running purge.
#[derive(Clone, Copy, Default, PartialEq)]
struct Progress {
    removed: i64,
    remaining: i64,
    running: bool,
    /// Whether the loop is currently sitting out a rate limit rather than deleting.
    ///
    /// On screen because the two look identical otherwise: a counter that stops moving for five
    /// seconds reads as a wedged purge, and the operator's next move is to reload the page and
    /// start it again.
    waiting: bool,
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
            let mut waits = 0_u32;
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
                    // A rate limit is not a failure of the purge, it is the server asking for a
                    // pause: the route draws on the tight write budget and a console doing
                    // anything else at the same time can spend it. Waiting the stated interval
                    // and carrying on is the only answer that finishes the job — reporting it
                    // strands the operator mid-purge with no way to tell how far it got.
                    Err(e) if api::retry_after_ms(&e).is_some() && waits < MAX_WAITS => {
                        let wait = api::retry_after_ms(&e).unwrap_or(WAIT_FALLBACK_MS);
                        waits += 1;
                        progress.with_mut(|p| p.waiting = true);
                        crate::platform::sleep_ms(wait).await;
                        progress.with_mut(|p| p.waiting = false);
                        continue;
                    }
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
                    waiting: false,
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
            progress.with_mut(|p| {
                p.running = false;
                p.waiting = false;
            });
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
                    if live.waiting {
                        span { style: "margin-left:8px;color:var(--star-ink);",
                            {i18n.t("console.catalogue.purgeWaiting")}
                        }
                    }
                }
            }
            StepUpGuard { gate, intro: Some(i18n.t("console.stepUp.intro")) }
            OutcomeLine { outcome: outcome.read().clone() }
        }
    }
}
