//! The multi-select bulk bar.
//!
//! This is what replaced drag-and-drop for the case drag never handled: "these forty titles are
//! dead, drop them all". Every action here is **one** request (`POST`/`DELETE
//! /v1/me/watchlist/bulk`, `POST /v1/me/progress/bulk-read`) rather than one per id — forty
//! `PUT`s each followed by a refetch of six hundred rows is what the old board did, and it is
//! why moving a handful of titles took the better part of a minute.

use super::row::RowCtx;
use super::{Board, BULK_LIMIT};
use crate::hooks::{use_busy, Reload};
use crate::i18n::use_i18n;
use crate::icons::{Ic, Icon};
use crate::models::*;
use dioxus::prelude::*;
use std::collections::HashSet;

/// Floats bottom-centre, and exists only while something is selected.
#[component]
pub(super) fn BulkBar(
    mut selected: Signal<HashSet<SeriesId>>,
    board: Signal<Board>,
    reload: Reload,
) -> Element {
    let i18n = use_i18n();
    let ctx = use_context::<RowCtx>();
    let busy = use_busy();
    // First click arms the remove button, second acts. Local to the bar, so it resets whenever
    // the selection empties and the bar unmounts.
    let mut arm_remove = use_signal(|| false);
    let ids: Vec<SeriesId> = selected.read().iter().copied().collect();
    if ids.is_empty() {
        return rsx! {};
    }

    // A bulk call either lands or it does not; there is no meaningful half-applied local state
    // to paint, and every one of these changes which rows belong on the page. So unlike the
    // single-row actions, these refetch — and the selection is cleared, because holding a
    // selection of rows that may no longer be there is how a second click acts on the wrong set.
    let settle = move |result: Result<BulkResult, progenitor_client::Error<ProblemDetails>>| {
        let mut ctx = ctx;
        let mut selected = selected;
        match result {
            Ok(outcome) => {
                if !outcome.skipped.is_empty() {
                    // Partial success is reported, not swallowed: "38 of 40" tells the reader
                    // their client was stale, which is actionable. Claiming 40 is not.
                    ctx.outcome.set(Some(Ok(i18n.args(
                        "watchlist.bulkPartial",
                        &[
                            ("applied", &outcome.applied.len().to_string()),
                            ("total", &(outcome.applied.len() + outcome.skipped.len()).to_string()),
                        ],
                    ))));
                }
                selected.write().clear();
                reload.bump();
            }
            Err(e) => ctx.failed(e),
        }
        busy.release();
    };

    let update = move |status: Option<WatchStatus>, notify: Option<bool>| {
        if !busy.claim() {
            return;
        }
        let client = ctx.api.client();
        let series_ids = selected.peek().iter().copied().take(BULK_LIMIT).collect();
        spawn(async move {
            let result = client
                .bulk_update_watchlist()
                .body(WatchlistBulkUpdate {
                    series_ids,
                    status: status.map(Into::into),
                    notify,
                })
                .send()
                .await
                .map(progenitor_client::ResponseValue::into_inner);
            settle(result);
        });
    };

    let mark_read = move |_| {
        if !busy.claim() {
            return;
        }
        let client = ctx.api.client();
        let series_ids = selected.peek().iter().copied().take(BULK_LIMIT).collect();
        spawn(async move {
            let result = client
                .bulk_mark_read()
                .body(WatchlistBulkIds { series_ids })
                .send()
                .await
                .map(progenitor_client::ResponseValue::into_inner);
            settle(result);
        });
    };

    let remove = move |()| {
        if !busy.claim() {
            return;
        }
        let client = ctx.api.client();
        let series_ids = selected.peek().iter().copied().take(BULK_LIMIT).collect();
        spawn(async move {
            let result = client
                .bulk_remove_watchlist()
                .body(WatchlistBulkIds { series_ids })
                .send()
                .await
                .map(progenitor_client::ResponseValue::into_inner);
            settle(result);
        });
    };

    // Muting a mixed selection has to mean something definite. "If any of them still notify,
    // mute everything" is the rule with no surprising outcome: pressing it twice never toggles
    // half the selection back on.
    let any_notifying = {
        let board = board.read();
        board
            .items
            .iter()
            .any(|i| selected.read().contains(&i.series_id) && i.notify)
    };
    let mute_icon = if any_notifying {
        Icon::NotifyOff
    } else {
        Icon::Notify
    };

    rsx! {
        div { class: "ik-wl-bulk", role: "toolbar", "aria-label": i18n.t("watchlist.bulkLabel"),
            span { class: "ik-mono ik-wl-bulk-count",
                {i18n.args("watchlist.nSelected", &[("count", &ids.len().to_string())])}
            }
            label { class: "ik-wl-ctl",
                select {
                    class: "ik-select",
                    disabled: busy.is_busy(),
                    // A `<select>` whose value is always the prompt: it is an action menu, not a
                    // field, and leaving it showing the last status picked would misreport the
                    // selection's actual state.
                    value: "",
                    onchange: move |event| {
                        if let Some(status) = WatchStatus::all()
                            .iter()
                            .copied()
                            .find(|s| s.token() == event.value())
                        {
                            update(Some(status), None);
                        }
                    },
                    option { value: "", {i18n.t("watchlist.moveToPrompt")} }
                    for status in WatchStatus::all().iter().copied() {
                        option { value: "{status.token()}", {i18n.t(status.label_key())} }
                    }
                }
            }
            button {
                class: "ik-btn",
                r#type: "button",
                disabled: busy.is_busy(),
                onclick: mark_read,
                {i18n.t("watchlist.markAllRead")}
            }
            button {
                class: "ik-btn",
                r#type: "button",
                disabled: busy.is_busy(),
                onclick: move |_| update(None, Some(!any_notifying)),
                Ic { icon: mute_icon, size: 15 }
                if any_notifying {
                    {i18n.t("watchlist.mute")}
                } else {
                    {i18n.t("watchlist.unmute")}
                }
            }
            // Untracking forty titles is not undoable from this screen, so it asks once — the
            // two-tier rule for a reversible-in-principle destructive action, in the shape a
            // 44px bar has room for: the first click arms the button and states the
            // consequence, the second acts. The rest of the bar does not ask, because a wrong
            // status or a wrong mute is one click to put back.
            button {
                class: if *arm_remove.read() { "ik-btn primary" } else { "ik-btn" },
                r#type: "button",
                disabled: busy.is_busy(),
                onclick: move |_| {
                    if *arm_remove.peek() {
                        arm_remove.set(false);
                        remove(());
                    } else {
                        arm_remove.set(true);
                    }
                },
                if *arm_remove.read() {
                    {i18n.args("watchlist.confirmRemove", &[("count", &ids.len().to_string())])}
                } else {
                    {i18n.t("common.remove")}
                }
            }
            button {
                class: "ik-wl-bulk-close",
                r#type: "button",
                "aria-label": i18n.t("watchlist.clearSelection"),
                onclick: move |_| selected.write().clear(),
                Ic { icon: Icon::Close, size: 16 }
            }
        }
    }
}
