//! Series detail — the chapter list leads, tracking rides the sidebar.
//!
//! A hero band (cover, identity, stat row and one split primary action) over a `1fr 340px`
//! body grid: synopsis and the chapter table on the left, the Tracking card and the related
//! slot on the right.
//!
//! # The one thing this screen does differently
//!
//! `GET /v1/series/:id/chapters` answers for a single source. This view fetches **every**
//! source's list concurrently and merges them ([`model`]), so a chapter is one row that knows
//! which sources carry it and where each would open. That is what lets the per-source panel
//! collapse into a single open control per row.
//!
//! Fields the API does not expose are omitted rather than fabricated: there is no rating, and
//! related series stay an honest placeholder pending `/v1/series/:id/related`.

mod chapters;
mod model;
mod tracking;

use crate::api;
use crate::components::{async_view, Cover, ErrorBox, SkeletonBlock};
use crate::hooks::{use_busy, use_reload, Reload};
use crate::i18n::use_i18n;
use crate::icons::{Ic, Icon};
use crate::models::*;
use crate::state::use_session;
use crate::util::chapter_number;
use chapters::{ChapterSection, OpenControl};
use dioxus::prelude::*;
use model::{
    merge_chapters, next_unread, rank_sources, source_ceiling, ChapterKey, MergedChapter,
    RankedSource,
};
use progenitor_client::ResponseValue;

/// Every source's chapter list, in the order the API returned the sources.
type SourceChapters = Vec<(SourceDto, Vec<ChapterDto>)>;

/// Where a per-series source pin is remembered.
///
/// A pin is a per-device reading preference, stored like the appearance knobs are. TODO(api):
/// a `preferred_source_id` on the watchlist entry would make it follow the reader across
/// devices; until then this is honest about being local.
fn pin_key(id: SeriesId) -> String {
    format!("tv-src-{id}")
}

/// The route gives us a plain `String` (see `crate::Route::Series`); parse it once here so
/// the rest of the view works with the real, compiler-checked `SeriesId`.
#[component]
pub(crate) fn Series(id: String) -> Element {
    let i18n = use_i18n();
    let Ok(id) = id.parse::<SeriesId>() else {
        return rsx! {
            div { class: "ik-empty", {i18n.t("series.badLink")} }
        };
    };

    let session = use_session();
    let api = api::use_api();
    let reload_detail = use_reload();
    let reload_wl = use_reload();
    let reload_chapters = use_reload();
    let mut pinned = use_signal(|| Option::<SeriesSourceId>::None);

    // The pin is read once on mount and written back whenever it changes.
    use_future(move || async move {
        let script = format!("return localStorage.getItem('{}');", pin_key(id));
        if let Ok(value) = document::eval(&script).await {
            if let Some(stored) = value.as_str() {
                if let Ok(source) = stored.trim_matches('"').parse::<SeriesSourceId>() {
                    pinned.set(Some(source));
                }
            }
        }
    });
    use_effect(move || {
        if let Some(source) = *pinned.read() {
            let _ = document::eval(&format!(
                "localStorage.setItem('{key}','{source}');",
                key = pin_key(id),
            ));
        }
    });

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

    // One request per source, issued together. Reading `detail` here subscribes this resource
    // to it, so the fan-out starts the moment the series lands and re-runs after a progress
    // write bumps `reload_chapters`.
    let per_source = use_resource(move || {
        reload_chapters.track();
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

    let watchlist = use_resource(move || {
        reload_wl.track();
        let client = api.client();
        let authed = session.is_authenticated();
        async move {
            if !authed {
                return Ok(Vec::new());
            }
            client
                .watchlist()
                .send()
                .await
                .map(ResponseValue::into_inner)
                .map_err(|e| api::friendly_error(i18n, e))
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
    let ranked_sources = rank_sources(&loaded.sources, pin);
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

    let entry = current_entry(&watchlist, id);
    let total_chapters = merged.iter().filter(|c| !c.is_part()).count();

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
        div { class: "ik-body-grid", style: "margin-top:8px;",
            div { style: "min-width:0;",
                if let Some(description) = loaded.description.clone() {
                    p { style: "font-size:14px;line-height:1.7;color:var(--text-2);margin:0 0 22px;max-width:75ch;",
                        "{description}"
                    }
                }
                if loaded.sources.is_empty() {
                    div { class: "ik-empty", {i18n.t("series.noSources")} }
                } else {
                    {
                        async_view(
                            &per_source,
                            reload_chapters,
                            || rsx! { SkeletonBlock { height: 320 } },
                            |_| {
                                if merged.is_empty() {
                                    return rsx! {
                                        div { class: "ik-empty", {i18n.t("series.noChapters")} }
                                    };
                                }
                                rsx! {
                                    ChapterSection {
                                        series_id: id,
                                        chapters: merged.clone(),
                                        sources: sources.clone(),
                                        pinned,
                                        reload: reload_chapters,
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
                    reload_chapters,
                }
                div { class: "ik-sidebar-card",
                    div { class: "ik-sec-lbl", style: "margin-bottom:10px;", {i18n.t("series.alsoFollow")} }
                    // TODO(api) §9.3: needs GET /v1/series/:id/related.
                    div { class: "ik-muted", style: "font-size:12.5px;", {i18n.t("series.alsoFollowSoon")} }
                }
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
    pinned: Signal<Option<SeriesSourceId>>,
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
    let source_count = i64::try_from(detail.sources.len()).unwrap_or(0);
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
            button {
                class: "ik-btn",
                style: "margin-bottom:16px;font-size:12.5px;padding:8px 12px;",
                onclick: move |_| {
                    nav.go_back();
                },
                Ic { icon: Icon::Back, size: 14 }
                {i18n.t("common.back")}
            }
            div { class: "ik-hero",
                div { Cover { url: detail.cover_url.clone(), title: detail.title.clone() } }
                div { style: "min-width:0;",
                    div { class: "ik-flex", style: "margin-bottom:8px;flex-wrap:wrap;",
                        span {
                            class: "ik-pill",
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
                    div { class: "ik-stat-inline",
                        div { class: "item",
                            {
                                i18n.args(
                                    "series.chapterCount",
                                    &[("count", &whole_chapters.to_string())],
                                )
                            }
                        }
                        div { class: "item",
                            span { style: "display:flex;color:var(--jade-bright);",
                                Ic { icon: Icon::Layers, size: 15 }
                            }
                            {i18n.plural("series.sources", source_count, &[])}
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
        button {
            class: "ik-btn",
            style: "padding:12px 14px;font-size:13.5px;",
            disabled: busy.is_busy(),
            onclick: toggle_membership,
            Ic { icon: Icon::Bookmark, size: 15 }
            if in_list {
                {i18n.t("series.inWatchlist")}
            } else {
                {i18n.t("series.addToWatchlist")}
            }
        }
        if in_list {
            button {
                class: "ik-btn",
                style: if notify { "width:44px;height:44px;padding:0;justify-content:center;color:var(--acc);" } else { "width:44px;height:44px;padding:0;justify-content:center;" },
                disabled: busy.is_busy(),
                "aria-pressed": if notify { "true" } else { "false" },
                title: if notify { i18n.t("watchlist.notifyOn") } else { i18n.t("watchlist.notifyOff") },
                onclick: toggle_notify,
                Ic { icon: Icon::Notify, size: 17 }
            }
        }
    }
}

/// Find this series' watchlist entry (if any) from the loaded watchlist resource.
fn current_entry(
    watchlist: &Resource<Result<Vec<WatchlistItem>, String>>,
    series_id: SeriesId,
) -> Option<WatchlistItem> {
    match &*watchlist.read_unchecked() {
        Some(Ok(list)) => list.iter().find(|i| i.series_id == series_id).cloned(),
        _ => None,
    }
}
