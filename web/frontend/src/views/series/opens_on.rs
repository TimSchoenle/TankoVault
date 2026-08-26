//! The Tracking card's *Opens on* row: which source this series resolves to, and the picker that
//! pins another one.
//!
//! This is the series-level counterpart to the per-chapter menu in [`super::chapters`]. The two
//! answer different questions and must not be folded together: the chapter menu is "open *this
//! chapter* somewhere", and is offered even to a reader who tracks nothing, while this one writes
//! the pin that decides where every button on the screen points from now on — which is a column
//! on the watchlist entry, so an untracked series has nowhere to keep it.

use super::model::RankedSource;
use super::pin::Pinned;
use crate::i18n::use_i18n;
use crate::icons::{Ic, Icon};
use crate::models::*;
use crate::util::{monogram, rel_time};
use crate::Route;
use dioxus::prelude::*;
use inkstone_ui::{Button, Size, Tone};

/// How many following sources the fallback line names before it stops.
const FALLBACK_SHOWN: usize = 2;

/// The current source, the fallback chain behind it, and the picker that changes both.
#[component]
pub(super) fn OpensOnSection(
    /// Every source in resolution order — the pin, then the reader's order, then the API's.
    sources: Vec<RankedSource>,
    pinned: Pinned,
    /// This series' watchlist entry, for the per-provider health the catalogue call omits.
    entry: Option<WatchlistItem>,
) -> Element {
    let i18n = use_i18n();
    let mut open = use_signal(|| false);
    let expanded = *open.read();

    let Some(leader) = sources.first().cloned() else {
        return rsx! {};
    };
    let states = entry.map(|e| e.sources).unwrap_or_default();
    let is_pinned = pinned.current() == Some(leader.source.id);
    let fallback: Vec<String> = sources
        .iter()
        .skip(1)
        .take(FALLBACK_SHOWN)
        .map(|ranked| monogram(&ranked.source.provider_name))
        .collect();

    rsx! {
        div { class: "ik-track-sec",
            div { class: "ik-sec-lbl", style: "margin-bottom:8px;", {i18n.t("series.track.opensOn")} }
            div { class: "ik-listbox",
                div { class: "ik-listrow",
                    span { class: "ik-mono-tile lg pref", {monogram(&leader.source.provider_name)} }
                    div { style: "min-width:0;",
                        div { style: "font-weight:600;font-size:13px;", "{leader.source.provider_name}" }
                        div { class: "ik-mono", style: "font-size:10.5px;color:var(--muted);margin-top:1px;",
                            if is_pinned {
                                {i18n.t("series.track.pinnedHere")}
                            } else {
                                {i18n.t("series.track.followingOrder")}
                            }
                        }
                    }
                    Button {
                        size: Size::Xs,
                        style: "margin-left:auto;",
                        expanded,
                        on_click: move |_| {
                            let next = !*open.peek();
                            open.set(next);
                        },
                        if expanded {
                            {i18n.t("common.close")}
                        } else {
                            {i18n.t("series.track.change")}
                        }
                    }
                }
                if expanded {
                    for ranked in sources.iter().cloned() {
                        SourceChoice {
                            key: "{ranked.source.id}",
                            ranked: ranked.clone(),
                            state: provider_state(&states, &ranked.source.provider_slug),
                            pinned,
                            on_done: move |()| open.set(false),
                        }
                    }
                }
                div { class: "ik-listfoot",
                    if expanded {
                        if pinned.is_available() && pinned.current().is_some() {
                            Button {
                                size: Size::Xs,
                                tone: Tone::Bare,
                                style: "padding:0;",
                                on_click: move |_| {
                                    pinned.clear();
                                    open.set(false);
                                },
                                {i18n.t("series.unpinSource")}
                            }
                        } else if !pinned.is_available() {
                            span { {i18n.t("series.pinNeedsTracking")} }
                        } else {
                            span { {i18n.t("series.pinHint")} }
                        }
                        Link {
                            to: Route::Account {},
                            class: "ik-icon-link",
                            style: "margin-left:auto;color:var(--muted);",
                            {i18n.t("series.track.sourceOrder")}
                            Ic { icon: Icon::OpenInNew, size: 11 }
                        }
                    } else if !fallback.is_empty() {
                        span { class: "ik-mono",
                            {i18n.args("series.track.fallback", &[("sources", &fallback.join(" → "))])}
                        }
                    }
                }
            }
        }
    }
}

/// One source in the picker: what it carries, how fresh it is, and whether it is pinned.
///
/// Every source is offered, including a degraded one: which source a reader *wants* is their
/// call, and a picker that silently omits the one they came here to choose is worse than one
/// that names the problem beside it.
#[component]
fn SourceChoice(
    ranked: RankedSource,
    /// This source's provider health, when the watchlist entry carried one.
    state: Option<ProviderState>,
    pinned: Pinned,
    on_done: EventHandler<()>,
) -> Element {
    let i18n = use_i18n();
    let source_id = ranked.source.id;
    let is_pinned = pinned.current() == Some(source_id);
    let degraded = state.is_some_and(|state| state != ProviderState::Active);

    let mut facts = Vec::new();
    if is_pinned {
        facts.push(i18n.t("series.pinned"));
    }
    facts.push(i18n.args(
        "series.chapterCount",
        &[("count", &ranked.source.chapter_count.to_string())],
    ));
    if let Some(when) = ranked.freshest.as_deref() {
        facts.push(rel_time(i18n, Some(when)));
    }
    let sub = facts.join(" · ");
    let health = state.map(|state| i18n.t(&format!("console.providerState.{state}")));

    rsx! {
        button {
            class: if is_pinned { "ik-pickrow on" } else { "ik-pickrow" },
            disabled: !pinned.is_available(),
            "aria-pressed": if is_pinned { "true" } else { "false" },
            title: if pinned.is_available() { i18n.t("series.pinSource") } else { i18n.t("series.pinNeedsTracking") },
            onclick: move |_| {
                pinned.set(source_id);
                on_done.call(());
            },
            span { class: "ik-mono-tile md", {monogram(&ranked.source.provider_name)} }
            span { style: "min-width:0;",
                span { class: "nm", style: "display:block;", "{ranked.source.provider_name}" }
                span { class: "why", style: "display:block;", "{sub}" }
                if let Some(health) = health.filter(|_| degraded) {
                    span { class: "why warn", style: "display:block;", "{health}" }
                }
            }
            if is_pinned {
                span { style: "margin-left:auto;display:flex;color:var(--acc);",
                    Ic { icon: Icon::Check, size: 14 }
                }
            }
        }
    }
}

/// The health the watchlist entry recorded for the provider behind a source, if it named one.
fn provider_state(sources: &[WatchlistSource], slug: &str) -> Option<ProviderState> {
    sources
        .iter()
        .find(|source| source.code == slug)
        .map(|source| source.state)
}
