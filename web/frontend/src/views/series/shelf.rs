//! The Tracking card's shelf row: which shelf this series is on, and the picker that moves it.
//!
//! The picker carries the reader's whole-library count beside each shelf because the choice is
//! comparative — how much is already on hold is what makes *paused* the right answer — and that
//! count comes from the summary endpoint, which carries no filter state, so it cannot disagree
//! with the watchlist's own tabs.

use crate::api;
use crate::components::{async_view, SkeletonRows};
use crate::hooks::{use_busy, use_reload, Reload};
use crate::i18n::use_i18n;
use crate::icons::{Ic, Icon};
use crate::models::*;
use crate::util::{rel_time, thousands};
use dioxus::prelude::*;
use inkstone_ui::{Button, Size};
use progenitor_client::ResponseValue;

/// The shelf row, and the picker it opens into.
#[component]
pub(super) fn ShelfSection(entry: WatchlistItem, reload_wl: Reload) -> Element {
    let api = api::use_api();
    let i18n = use_i18n();
    let mut open = use_signal(|| false);
    let reload_counts = use_reload();
    let expanded = *open.read();

    let summary = use_resource(move || {
        reload_counts.track();
        reload_wl.track();
        let client = api.client();
        async move {
            client
                .watchlist_summary()
                .send()
                .await
                .map(ResponseValue::into_inner)
                .map_err(|e| api::friendly_error(i18n, e))
        }
    });

    rsx! {
        div { class: "ik-track-sec",
            div { class: "ik-flex", style: "align-items:baseline;gap:8px;margin-bottom:8px;",
                span { class: "ik-sec-lbl", {i18n.t("series.track.shelf")} }
                span { class: "ik-mono ik-muted", style: "margin-left:auto;font-size:11.5px;",
                    {
                        i18n.args(
                            "series.track.trackedFor",
                            &[("when", &rel_time(i18n, Some(entry.added_at.as_str())))],
                        )
                    }
                }
            }
            Button {
                size: Size::Md,
                style: "width:100%;justify-content:flex-start;",
                expanded,
                on_click: move |_| {
                    let next = !*open.peek();
                    open.set(next);
                },
                {i18n.t(entry.status.label_key())}
                span { class: "ik-mono", style: "margin-left:auto;font-size:11px;color:var(--faint);",
                    if expanded {
                        {i18n.t("common.close")}
                    } else {
                        {i18n.t("series.track.change")}
                    }
                }
            }
            if expanded {
                div { class: "ik-listbox", style: "margin-top:8px;",
                    {
                        async_view(
                            &summary,
                            reload_counts,
                            || rsx! {
                                div { style: "padding:10px 12px;",
                                    SkeletonRows { count: 5, height: 20 }
                                }
                            },
                            move |view| rsx! {
                                for status in <WatchStatus as WatchStatusExt>::all().iter().copied() {
                                    ShelfChoice {
                                        key: "{status.token()}",
                                        entry: entry.clone(),
                                        status,
                                        count: shelf_count(&view.counts, status),
                                        reload_wl,
                                        on_done: move |()| open.set(false),
                                    }
                                }
                            },
                        )
                    }
                    div { class: "ik-listfoot", {i18n.t("series.track.shelfNote")} }
                }
            }
        }
    }
}

/// One shelf in the picker: the name, this reader's count on it, and the write that moves here.
#[component]
fn ShelfChoice(
    entry: WatchlistItem,
    status: WatchStatus,
    count: i64,
    reload_wl: Reload,
    on_done: EventHandler<()>,
) -> Element {
    let api = api::use_api();
    let i18n = use_i18n();
    let busy = use_busy();
    let current = entry.status == status;
    let series_id = entry.series_id;
    let notify = entry.notify;

    let choose = move |_| {
        if current {
            on_done.call(());
            return;
        }
        if !busy.claim() {
            return;
        }
        let client = api.client();
        spawn(async move {
            let body = WatchlistUpsert {
                status: Some(status),
                notify: Some(notify),
            };
            if client
                .put_watchlist()
                .series_id(series_id)
                .body(body)
                .send()
                .await
                .is_ok()
            {
                reload_wl.bump();
                on_done.call(());
            }
            busy.release();
        });
    };

    rsx! {
        button {
            class: if current { "ik-pickrow on" } else { "ik-pickrow" },
            disabled: busy.is_busy(),
            "aria-pressed": if current { "true" } else { "false" },
            onclick: choose,
            span { class: "nm", {i18n.t(status.label_key())} }
            if current {
                span { style: "margin-left:auto;display:flex;color:var(--acc);",
                    Ic { icon: Icon::Check, size: 14 }
                }
            } else {
                span { class: "cnt", "{thousands(count)}" }
            }
        }
    }
}

/// This shelf's bucket out of the summary's counts.
const fn shelf_count(counts: &WatchlistCounts, status: WatchStatus) -> i64 {
    match status {
        WatchStatus::Reading => counts.reading,
        WatchStatus::Planned => counts.planned,
        WatchStatus::Paused => counts.paused,
        WatchStatus::Completed => counts.completed,
        WatchStatus::Dropped => counts.dropped,
    }
}
