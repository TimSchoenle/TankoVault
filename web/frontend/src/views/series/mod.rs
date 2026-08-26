//! Series detail — the chapter list leads, tracking rides the sidebar.
//!
//! # The one thing this screen does differently
//!
//! `GET /v1/series/:id/chapters` answers for a single source. This view fetches every source's
//! list concurrently and merges them ([`model`]), so each chapter row knows which sources carry
//! it and where each would open.
//!
//! Fields the API does not expose are omitted rather than fabricated (no rating). The sidebar's
//! [`similar`] rail is content similarity from the recommendation model, not a relation graph —
//! a direct sequel is still something this screen cannot name.

mod chapters;
mod model;
mod pin;
mod similar;
mod tracking;

use crate::api;
use crate::components::{async_view, Cover, EmptyBox, ErrorBox, SkeletonBlock};
use crate::hooks::{use_busy, use_outcome, use_reload, Reload};
use crate::i18n::use_i18n;
use crate::icons::{Ic, Icon};
use crate::models::*;
use crate::state::source_order::use_source_order;
use crate::state::use_session;
use crate::util::{chapter_number, rel_time};
use crate::Route;
use chapters::{ChapterSection, OpenControl};
use dioxus::prelude::*;
use inkstone_ui::{Button, IconButton, Pill, Size, Tone};
use model::{
    merge_chapters, next_unread, rank_sources, source_ceiling, ChapterKey, MergedChapter,
    RankedSource,
};
use progenitor_client::ResponseValue;
/// Every source's chapter list, in the order the API returned the sources.
type SourceChapters = Vec<(SourceDto, Vec<ChapterDto>)>;

/// The Series route.
///
/// **Keyed, not called directly.** Following a link from one series to another — which is what
/// the `More like this` rail is — keeps the router on the same route variant, so Dioxus reuses
/// this scope and only swaps the prop. Every `use_resource` below captured the id it was first
/// built with and reacts to signals, not to props, so the URL moved while the screen went on
/// showing the series it was mounted with. A changed key remounts the subtree instead, which is
/// also what resets the chapter list's paging and the source menus.
#[component]
pub(crate) fn Series(id: String) -> Element {
    rsx! {
        SeriesPage { key: "{id}", id }
    }
}

/// The route gives us a plain `String` (see `crate::Route::Series`); parse it once here so
/// the rest of the view works with the real, compiler-checked `SeriesId`.
#[component]
fn SeriesPage(id: String) -> Element {
    let i18n = use_i18n();
    let Ok(id) = id.parse::<SeriesId>() else {
        return rsx! {
            EmptyBox { message: i18n.t("series.badLink") }
        };
    };

    let session = use_session();
    let api = api::use_api();
    let source_order = use_source_order();
    let reload_detail = use_reload();
    let reload_wl = use_reload();
    // One signal for read state, shared by the chapter list's toggles and the sidebar's
    // stepper — they move the same server state, so must invalidate the same fetches. Two
    // separate `Reload`s here left the sidebar showing a stale frontier until a full reload.
    let reload_progress = use_reload();
    // The pin now rides the watchlist entry, so it is seeded from that fetch rather than read
    // synchronously — see the effect below the entry resource.
    let pinned = use_signal(|| Option::<SeriesSourceId>::None);
    let pin_outcome = use_outcome();

    let detail = use_resource(move || {
        reload_detail.track();
        let client = api.client();
        async move {
            client
                .detail()
                .id(id)
                .send()
                .await
                .map(ResponseValue::into_inner)
                .map_err(|e| api::friendly_error(i18n, e))
        }
    });

    // The one screen whose tab name the route cannot spell — it is in the payload, not the URL.
    let page_title = use_context::<crate::title::PageTitle>();
    use_effect(move || {
        if let Some(Ok(loaded)) = &*detail.read() {
            let route = Route::Series {
                id: loaded.id.to_string(),
            };
            page_title.set(route, loaded.title.clone());
        }
    });

    // One request per source, issued together. Reading `detail` here subscribes this resource
    // to it, so the fan-out starts once the series lands and re-runs on `reload_progress`.
    let per_source = use_resource(move || {
        reload_progress.track();
        let sources: Vec<SourceDto> = match &*detail.read() {
            Some(Ok(loaded)) => loaded.sources.clone(),
            _ => Vec::new(),
        };
        let client = api.client();
        async move {
            let fetches = sources.into_iter().map(|source| {
                let client = client.clone();
                async move {
                    let list = client
                        .chapters()
                        .id(id)
                        .source(source.id.to_string())
                        .send()
                        .await
                        .map(ResponseValue::into_inner)
                        .map_err(|e| api::friendly_error(i18n, e))?;
                    Ok::<_, String>((source, list))
                }
            });
            futures_util::future::join_all(fetches)
                .await
                .into_iter()
                .collect::<Result<SourceChapters, String>>()
        }
    });

    // One entry, not the whole watchlist — fetching everything and scanning for this series
    // broke once the list started paginating: past the first page the entry isn't in the
    // response, and the page would falsely offer "Add to watchlist" for a tracked title.
    let watchlist = use_resource(move || {
        reload_wl.track();
        // Marking a chapter read now tracks the series server-side, so a progress write is one
        // of the things that can create this entry. Without this second subscription the
        // sidebar kept offering "Add to watchlist" for a series already on it.
        reload_progress.track();
        let client = api.client();
        let authed = session.is_authenticated();
        async move {
            if !authed {
                return Ok(None);
            }
            client
                .get_watchlist_entry()
                .series_id(id)
                .send()
                .await
                .map(|r| r.into_inner().entry)
                .map_err(|e| api::friendly_error(i18n, e))
        }
    });

    // The entry is the pin's home, so every refetch of it re-seeds the signal — including the
    // one a successful pin write triggers, which is what reconciles the optimistic move.
    let mut pin_signal = pinned;
    use_effect(move || {
        if let Some(Ok(entry)) = &*watchlist.read() {
            pin_signal.set(
                entry
                    .as_ref()
                    .and_then(|e| e.pinned_source_id)
                    .map(SeriesSourceId::from),
            );
        }
    });

    let loaded = match &*detail.read() {
        None => {
            return rsx! {
                div { class: "ik-hero",
                    div { class: "ik-skeleton ik-skel-cover" }
                    div {
                        div { class: "ik-skeleton", style: "height:38px;width:60%;margin-bottom:12px;" }
                        div { class: "ik-skeleton", style: "height:14px;width:90%;margin-bottom:6px;" }
                        div { class: "ik-skeleton", style: "height:14px;width:80%;" }
                    }
                }
                SkeletonBlock { height: 320 }
            }
        }
        Some(Err(message)) => {
            let message = message.clone();
            return rsx! {
                ErrorBox { message, on_retry: move |()| reload_detail.bump() }
            };
        }
        Some(Ok(loaded)) => loaded.clone(),
    };

    // Rank and merge in the render, not in the fetch: changing the pin re-orders the same data
    // rather than re-issuing every request.
    let pin = *pinned.read();
    let fetched = match &*per_source.read() {
        Some(Ok(rows)) => rows.clone(),
        _ => Vec::new(),
    };
    let ranked_sources = rank_sources(&loaded.sources, pin, &source_order.slugs());
    let ordered: SourceChapters = ranked_sources
        .iter()
        .filter_map(|source| {
            fetched
                .iter()
                .find(|(fetched, _)| fetched.id == source.id)
                .cloned()
        })
        .collect();
    let merged = merge_chapters(&ordered);
    let sources: Vec<RankedSource> = ranked_sources
        .iter()
        .map(|source| RankedSource {
            source: source.clone(),
            ceiling: ordered
                .iter()
                .find(|(fetched, _)| fetched.id == source.id)
                .and_then(|(_, list)| source_ceiling(list)),
        })
        .collect();

    let entry = current_entry(&watchlist);
    let total_chapters = merged.iter().filter(|c| !c.is_part()).count();
    // Shadowed so every component below keeps forwarding one `pinned` prop; what changed is
    // that moving it now writes to the server instead of to this device's storage.
    let pinned = pin::Pinned::new(
        pinned,
        id,
        api,
        i18n,
        pin_outcome,
        reload_wl,
        entry.is_some(),
    );

    rsx! {
        Hero {
            detail: loaded.clone(),
            chapters: merged.clone(),
            sources: sources.clone(),
            pinned,
            entry: entry.clone(),
            authed: session.is_authenticated(),
            reload_wl,
        }
        // A refused pin is reported here rather than inside the source menu: the write closes
        // the menu, so a message rendered there would be dismissed by the very click that
        // caused it.
        crate::components::OutcomeLine { outcome: pin_outcome.read().clone() }
        div { class: "ik-body-grid", style: "margin-top:8px;",
            div { style: "min-width:0;",
                if let Some(description) = loaded.description.clone() {
                    p { style: "font-size:14px;line-height:1.7;color:var(--text-2);margin:0 0 22px;max-width:75ch;",
                        "{description}"
                    }
                }
                if loaded.sources.is_empty() {
                    EmptyBox { message: i18n.t("series.noSources") }
                } else {
                    {
                        async_view(
                            &per_source,
                            reload_progress,
                            || rsx! { SkeletonBlock { height: 320 } },
                            |_| {
                                if merged.is_empty() {
                                    return rsx! {
                                        EmptyBox { message: i18n.t("series.noChapters") }
                                    };
                                }
                                rsx! {
                                    ChapterSection {
                                        series_id: id,
                                        chapters: merged.clone(),
                                        sources: sources.clone(),
                                        pinned,
                                        reload: reload_progress,
                                    }
                                }
                            },
                        )
                    }
                }
            }
            div { style: "min-width:0;",
                tracking::TrackingCard {
                    series_id: id,
                    anilist_id: loaded.anilist_id.clone(),
                    entry,
                    authed: session.is_authenticated(),
                    total_chapters: i64::try_from(total_chapters).unwrap_or(i64::MAX),
                    reload_wl,
                    reload_progress,
                }
                similar::SimilarRail { series_id: id }
            }
        }
    }
}

/// Cover, identity, stat row and the page's primary action.
#[component]
fn Hero(
    detail: SeriesDetail,
    chapters: Vec<MergedChapter>,
    sources: Vec<RankedSource>,
    pinned: pin::Pinned,
    entry: Option<WatchlistItem>,
    authed: bool,
    reload_wl: Reload,
) -> Element {
    let i18n = use_i18n();
    let nav = use_navigator();
    // The hero's own menu slot, so opening it does not close a chapter row's menu.
    let open_menu = use_signal(|| Option::<ChapterKey>::None);

    let backdrop = detail.cover_url.clone().unwrap_or_default();
    let whole_chapters = chapters.iter().filter(|c| !c.is_part()).count();
    let whole_chapters_i64 = i64::try_from(whole_chapters).unwrap_or(i64::MAX);
    // Counted separately rather than folded into the total: a part release is a chapter a source
    // shipped ahead of the compiled whole one, so counting them together would make a series
    // look longer than it is — which is exactly why the chapter list collapses them.
    let part_releases =
        i64::try_from(chapters.iter().filter(|c| c.is_part()).count()).unwrap_or(i64::MAX);
    // The merge orders newest-first, so the head is the newest number and the newest date.
    let latest_number = chapters.first().map(|c| c.number);
    let latest_release = chapters
        .first()
        .and_then(|c| c.resolved().published_at.clone())
        .map(|at| rel_time(i18n, Some(at.as_str())));
    let byline = {
        let authors = detail
            .authors
            .iter()
            .map(|a| a.name.clone())
            .collect::<Vec<_>>();
        let mut parts = Vec::new();
        if !authors.is_empty() {
            parts.push(i18n.args("series.by", &[("authors", &authors.join(", "))]));
        }
        parts.extend(detail.alt_titles.iter().cloned());
        parts.join(" · ")
    };

    // The primary action: the next unread chapter when there is one, otherwise the newest —
    // "continue" and "open" are different promises and must not share wording.
    let (target, label) = match next_unread(&chapters) {
        Some(chapter) => (Some(chapter.clone()), "series.continueOn"),
        None => (chapters.first().cloned(), "series.openOn"),
    };

    rsx! {
        div { class: "ik-hero-wrap",
            // The blurred cover and the fade to the page ground live in their own clipping
            // layer, so the band itself can let a popover overflow it.
            div { class: "ik-hero-clip",
                if !backdrop.is_empty() {
                    div { class: "ik-hero-bg", style: "background-image:url('{backdrop}');" }
                }
            }
            Button {
                style: "margin-bottom:16px;font-size:12.5px;padding:8px 12px;",
                on_click: move |_| {
                    nav.go_back();
                },
                Ic { icon: Icon::Back, size: 14 }
                {i18n.t("common.back")}
            }
            div { class: "ik-hero",
                div { Cover { url: detail.cover_url.clone(), title: detail.title.clone() } }
                div { style: "min-width:0;",
                    div { class: "ik-flex", style: "margin-bottom:8px;flex-wrap:wrap;",
                        Pill {
                            style: "color:{detail.content_type.color()};border-color:color-mix(in srgb,{detail.content_type.color()} 50%,transparent);background:color-mix(in srgb,{detail.content_type.color()} 12%,transparent);",
                            {i18n.t(detail.content_type.label_key())}
                        }
                        span { class: "ik-flex ik-mono", style: "gap:6px;font-size:12px;color:{detail.status.color()};",
                            span {
                                class: "ik-status-dot",
                                style: "width:7px;height:7px;background:{detail.status.color()};",
                            }
                            {i18n.t(detail.status.label_key())}
                        }
                        if let Some(year) = detail.release_year {
                            span { class: "ik-mono", style: "font-size:12px;color:var(--faint);", "{year}" }
                        }
                    }
                    h1 { class: "ik-hero-title", "{detail.title}" }
                    if !byline.is_empty() {
                        div { class: "ik-hero-byline", "{byline}" }
                    }
                    if !detail.tags.is_empty() {
                        div { class: "ik-flex", style: "flex-wrap:wrap;gap:8px;margin:14px 0;",
                            for tag in detail.tags.iter() {
                                span { key: "{tag.id}", class: "ik-tagchip", "{tag.name}" }
                            }
                        }
                    }
                    // The source count used to sit here beside the chapter total. It is an
                    // operational fact about how many providers this deployment happens to
                    // carry the title on, not a property of the work, and next to a length it
                    // read as one. What replaces it is what a reader actually asks of a series
                    // page before committing: how much there is, how far it goes, and whether
                    // it is still moving.
                    div { class: "ik-stat-inline",
                        div { class: "item",
                            span { style: "display:flex;color:var(--jade-bright);",
                                Ic { icon: Icon::Layers, size: 15 }
                            }
                            {i18n.plural("series.chapterTally", whole_chapters_i64, &[])}
                        }
                        if let Some(latest) = latest_number {
                            div { class: "item",
                                {i18n.args("series.upTo", &[("number", &chapter_number(latest))])}
                            }
                        }
                        if part_releases > 0 {
                            div { class: "item",
                                {i18n.plural("series.partTally", part_releases, &[])}
                            }
                        }
                        if let Some(updated) = latest_release.clone() {
                            div { class: "item",
                                {i18n.args("series.lastRelease", &[("when", &updated)])}
                            }
                        }
                    }
                    div { class: "ik-flex", style: "gap:9px;flex-wrap:wrap;",
                        if let Some(chapter) = target {
                            OpenControl {
                                label: i18n.args(
                                    label,
                                    &[
                                        ("number", &chapter_number(chapter.number)),
                                        ("source", &chapter.resolved().provider_name),
                                    ],
                                ),
                                chapter,
                                sources,
                                pinned,
                                open_menu,
                                filled: true,
                                compact: false,
                            }
                        }
                        WatchControls {
                            series_id: detail.id,
                            entry,
                            authed,
                            reload: reload_wl,
                        }
                    }
                }
            }
        }
    }
}

/// Watchlist membership and the per-title notification bell.
#[component]
fn WatchControls(
    series_id: SeriesId,
    entry: Option<WatchlistItem>,
    authed: bool,
    reload: Reload,
) -> Element {
    let api = api::use_api();
    let i18n = use_i18n();
    let busy = use_busy();

    if !authed {
        return rsx! {
            span { class: "ik-muted", style: "font-size:13px;", {i18n.t("series.signInToTrack")} }
        };
    }

    let in_list = entry.is_some();
    let notify = entry.as_ref().is_none_or(|e| e.notify);
    let status = entry.as_ref().map_or(WatchStatus::Reading, |e| e.status);

    let toggle_membership = move |_| {
        if !busy.claim() {
            return;
        }
        let client = api.client();
        spawn(async move {
            let ok = if in_list {
                client
                    .delete_watchlist()
                    .series_id(series_id)
                    .send()
                    .await
                    .is_ok()
            } else {
                client
                    .put_watchlist()
                    .series_id(series_id)
                    .body(WatchlistUpsert {
                        status: Some(WatchStatus::Reading),
                        notify: Some(true),
                    })
                    .send()
                    .await
                    .is_ok()
            };
            if ok {
                reload.bump();
            }
            busy.release();
        });
    };

    let toggle_notify = move |_| {
        if !busy.claim() {
            return;
        }
        let client = api.client();
        spawn(async move {
            if client
                .put_watchlist()
                .series_id(series_id)
                .body(WatchlistUpsert {
                    status: Some(status),
                    notify: Some(!notify),
                })
                .send()
                .await
                .is_ok()
            {
                reload.bump();
            }
            busy.release();
        });
    };

    rsx! {
        Button {
            style: "padding:12px 14px;font-size:13.5px;",
            disabled: busy.is_busy(),
            on_click: toggle_membership,
            Ic { icon: Icon::Bookmark, size: 15 }
            if in_list {
            {i18n.t("series.inWatchlist")}
            } else {
            {i18n.t("series.addToWatchlist")}
            }
        }
        if in_list {
            IconButton {
                tone: Tone::Neutral,
                size: Size::Md,
                disabled: busy.is_busy(),
                pressed: notify,
                label: if notify { i18n.t("watchlist.notifyOn") } else { i18n.t("watchlist.notifyOff") },
                on_click: toggle_notify,
                icon: rsx! {
                    Ic { icon: Icon::Notify, size: 17 }
                },
            }
        }
    }
}

/// This series' watchlist entry, once the lookup has landed.
///
/// A failed or in-flight lookup is `None` ("not tracked as far as this page knows"); adding an
/// already-tracked title is an upsert, so the worst case is a no-op, not a wrong write.
fn current_entry(
    watchlist: &Resource<Result<Option<WatchlistItem>, String>>,
) -> Option<WatchlistItem> {
    match &*watchlist.read_unchecked() {
        Some(Ok(entry)) => entry.clone(),
        _ => None,
    }
}
