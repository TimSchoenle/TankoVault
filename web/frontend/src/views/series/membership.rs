//! The Tracking card's membership footer, its untracked state, and the operator's merge note.
//!
//! Membership is the last block on the card rather than the first because it is the destructive
//! one: everything above it edits an entry that exists, and this is the control that stops it
//! existing. It confirms inline, and the confirmation says what survives removal — read progress
//! and the tracker mapping do, which is what makes re-adding cheap and is not guessable from the
//! word "remove".

use crate::api;
use crate::components::InlineConfirm;
use crate::hooks::{use_busy, Reload};
use crate::i18n::use_i18n;
use crate::icons::{Ic, Icon};
use crate::models::*;
use crate::state::capabilities::use_capabilities;
use crate::util::rel_time;
use crate::views::{ConsoleEntity, ConsoleQuery};
use crate::wire::types::{Feature, Permission};
use crate::Route;
use dioxus::prelude::*;
use inkstone_ui::{Button, Size, Tone};
use progenitor_client::ResponseValue;

/// The card's last row: when the entry was made, and the control that ends it.
#[component]
pub(super) fn MembershipFooter(entry: WatchlistItem, reload_wl: Reload) -> Element {
    let api = api::use_api();
    let i18n = use_i18n();
    let busy = use_busy();
    let mut confirming = use_signal(|| false);
    let mut error = use_signal(|| Option::<String>::None);
    let series_id = entry.series_id;

    let remove = move |()| {
        if !busy.claim() {
            return;
        }
        error.set(None);
        let client = api.client();
        spawn(async move {
            match client.delete_watchlist().series_id(series_id).send().await {
                Ok(_) => {
                    confirming.set(false);
                    reload_wl.bump();
                }
                Err(e) => error.set(Some(api::friendly_error(i18n, e))),
            }
            busy.release();
        });
    };

    rsx! {
        div { style: "border-top:1px solid var(--border);margin-top:16px;padding-top:12px;",
            if *confirming.read() {
                InlineConfirm {
                    title: i18n.t("series.track.remove"),
                    body: i18n.args(
                        "series.track.removeBody",
                        &[("read", &entry.read_count.to_string())],
                    ),
                    cta: i18n.t("common.remove"),
                    busy: busy.is_busy(),
                    on_cancel: move |()| confirming.set(false),
                    on_confirm: remove,
                }
            } else {
                div { class: "ik-flex", style: "gap:8px;",
                    Button {
                        size: Size::Sm,
                        tone: Tone::Bare,
                        style: "padding:0;",
                        on_click: move |_| confirming.set(true),
                        Ic { icon: Icon::Delete, size: 14 }
                        {i18n.t("series.track.remove")}
                    }
                    span { class: "ik-mono", style: "margin-left:auto;font-size:11px;color:var(--faint);",
                        {
                            i18n.args(
                                "series.track.entryAge",
                                &[("when", &rel_time(i18n, Some(entry.added_at.as_str())))],
                            )
                        }
                    }
                }
            }
            if let Some(message) = error.read().clone() {
                crate::components::ErrorLine { message }
            }
        }
    }
}

/// What the card says while the series is not tracked.
///
/// Progress and history stay on the card above this: they are read state, not membership, and a
/// reader who has read forty chapters without shelving the title should not see that vanish
/// because they never pressed *Add*.
#[component]
pub(super) fn NotTrackedNote(series_id: SeriesId, reload_wl: Reload) -> Element {
    let api = api::use_api();
    let i18n = use_i18n();
    let busy = use_busy();

    let add = move |_| {
        if !busy.claim() {
            return;
        }
        let client = api.client();
        spawn(async move {
            if client
                .put_watchlist()
                .series_id(series_id)
                .body(WatchlistUpsert {
                    status: Some(WatchStatus::Reading),
                    notify: Some(true),
                })
                .send()
                .await
                .is_ok()
            {
                reload_wl.bump();
            }
            busy.release();
        });
    };

    rsx! {
        div { class: "ik-track-sec",
            div { class: "ik-sec-lbl", style: "margin-bottom:6px;", {i18n.t("series.track.notTracked")} }
            p { class: "ik-muted", style: "font-size:12.5px;line-height:1.55;margin:0 0 10px;",
                {i18n.t("series.track.notTrackedBody")}
            }
            Button {
                size: Size::Sm,
                disabled: busy.is_busy(),
                on_click: add,
                Ic { icon: Icon::Bookmark, size: 14 }
                {i18n.t("series.addToWatchlist")}
            }
        }
    }
}

/// The merge note: this series absorbed another one, shown only to an operator who may audit it.
///
/// The reader's side of a merge is deliberately silent — a merged catalogue is the point — so
/// this is gated on `merge.audit` rather than on being signed in, and it exists because the
/// operator who notices a wrong merge is usually the one reading the series page, not the one
/// scrolling the journal.
#[component]
pub(super) fn MergedRecordNote(series_id: SeriesId) -> Element {
    let api = api::use_api();
    let i18n = use_i18n();
    let caps = use_capabilities();
    let permitted = caps.can(Permission::MergeAudit) && caps.has_feature(Feature::AdminAudit);

    let merges = use_resource(move || {
        let client = api.client();
        async move {
            if !permitted {
                return Vec::new();
            }
            client
                .list_merge_decisions()
                .series_id(series_id.0)
                .outcome("merged")
                .limit(1_u32)
                .send()
                .await
                .map(ResponseValue::into_inner)
                .unwrap_or_default()
        }
    });

    let rows = merges.read_unchecked().clone().unwrap_or_default();
    let Some(decision) = rows.into_iter().find(|d| d.reverted_at.is_none()) else {
        return rsx! {};
    };
    // Both titles are stored as they read at merge time, so the absorbed one still has a name
    // even though its id no longer resolves.
    let absorbed = if decision.absorbed_id == Some(decision.left_id) {
        decision.left_title.clone()
    } else {
        decision.right_title.clone()
    };
    let when = rel_time(i18n, Some(decision.decided_at.as_str()));

    rsx! {
        div { class: "ik-note", style: "margin-top:14px;",
            div { class: "ik-flex", style: "gap:8px;align-items:flex-start;",
                span { style: "min-width:0;font-size:12.5px;line-height:1.55;",
                    div { class: "ik-sec-lbl", style: "margin-bottom:4px;",
                        {i18n.t("series.track.mergedRecord")}
                    }
                    {i18n.args("series.track.absorbed", &[("title", &absorbed), ("when", &when)])}
                }
                Link {
                    to: Route::ConsoleSection {
                        entity: ConsoleEntity::Merges,
                        query: ConsoleQuery::fresh().with_selection(Some(decision.id.to_string())),
                    },
                    class: "ik-icon-link",
                    style: "margin-left:auto;flex:none;color:var(--acc3);",
                    {i18n.t("series.track.whyMerged")}
                }
            }
        }
    }
}
