//! Series detail (DESIGN_SPEC §7.3) — a blurred-cover **hero** (cover + type/status/year +
//! title + stat row + watchlist actions) over a `1fr 340px` **body grid**: synopsis +
//! chapter list on the left; a **Read on** source list, a **Tracking** card, and a
//! **Readers also follow** slot in the right sidebar.
//!
//! Fields the current API does not expose — rating, author, alt-titles, tags, per-source
//! `is_primary`, per-chapter read-state and read-% — are **omitted gracefully** (never
//! fabricated, per the links-&-metadata invariant); they light up once the enrichment
//! endpoints (§9.2) land. Related series need `/v1/series/:id/related` (§9.3), so that slot
//! is an honest placeholder.

use crate::api;
use crate::components::{Cover, ErrorBox};
use crate::icons::{Ic, Icon};
use crate::models::{
    ContentType, SeriesStatus, SourceDto, WatchStatus, WatchlistItem, WatchlistUpsert,
};
use crate::state::use_session;
use crate::Route;
use dioxus::prelude::*;

#[component]
pub fn Series(id: String) -> Element {
    let session = use_session();
    let nav = use_navigator();
    let selected = use_signal(|| Option::<String>::None);
    let mut reload_wl = use_signal(|| 0u32);

    let detail_id = id.clone();
    let detail = use_resource(move || {
        let id = detail_id.clone();
        async move { api::series_detail(&id).await }
    });

    let chapters_id = id.clone();
    let chapters = use_resource(move || {
        let id = chapters_id.clone();
        let src = selected.read().clone();
        let token = session.token_value();
        async move { api::series_chapters(&id, src.as_deref(), token.as_deref()).await }
    });

    let wl_id = id.clone();
    let watchlist = use_resource(move || {
        let _ = (reload_wl.read(), wl_id.clone());
        async move {
            match session.token_value() {
                Some(t) => api::watchlist(&t).await,
                None => Ok(Vec::new()),
            }
        }
    });

    let sync_status = use_resource(move || async move {
        match session.token_value() {
            Some(t) => api::anilist_status(&t).await.ok(),
            None => None,
        }
    });

    let hero = match &*detail.read_unchecked() {
        None => rsx! {
            div { class: "ik-hero",
                div { class: "ik-skeleton ik-skel-cover" }
                div {
                    div { class: "ik-skeleton", style: "height:34px;width:60%;margin-bottom:12px;" }
                    div { class: "ik-skeleton", style: "height:14px;width:90%;margin-bottom:6px;" }
                    div { class: "ik-skeleton", style: "height:14px;width:80%;" }
                }
            }
        },
        Some(Err(e)) => {
            let msg = e.clone();
            rsx! {
                ErrorBox { message: msg, on_retry: move |()| reload_wl += 1 }
            }
        }
        Some(Ok(d)) => {
            let d = d.clone();
            let wl_entry = current_entry(&watchlist, &id);
            let chapters_total: i64 = d.sources.iter().map(|s| i64::from(s.chapter_count)).sum();
            let source_count = d.sources.len();
            let bg = d.cover_url.clone().unwrap_or_default();
            rsx! {
                div { class: "ik-hero-wrap",
                    if !bg.is_empty() {
                        div { class: "ik-hero-bg", style: "background-image:url('{bg}');" }
                    }
                    button {
                        class: "ik-btn",
                        style: "margin-bottom:16px;",
                        onclick: move |_| { nav.go_back(); },
                        Ic { icon: Icon::Back, size: 16 }
                        "Back"
                    }
                    div { class: "ik-hero",
                        div { Cover { url: d.cover_url.clone(), title: d.title.clone() } }
                        div {
                            div { class: "ik-flex", style: "margin-bottom:10px;",
                                span { class: "ik-pill", style: "color:{type_color(d.content_type)};border-color:color-mix(in srgb,{type_color(d.content_type)} 55%,transparent);", "{d.content_type.label()}" }
                                span { class: "ik-flex", style: "gap:6px;",
                                    span { class: "ik-status-dot", style: "background:{status_color(d.status)};" }
                                    span { class: "ik-muted", style: "font-size:13px;", "{d.status.label()}" }
                                }
                                if let Some(y) = d.release_year {
                                    span { class: "ik-mono ik-muted", "· {y}" }
                                }
                            }
                            h1 { style: "font-family:var(--font-display);font-size:38px;font-weight:800;letter-spacing:-.02em;line-height:1.05;margin:0 0 6px;", "{d.title}" }
                            // Alternative titles (§9.2) — shown only when present.
                            if !d.alt_titles.is_empty() {
                                div { class: "ik-muted", style: "font-size:14px;margin:0 0 8px;", "{d.alt_titles.join(\" · \")}" }
                            }
                            // Genre/tag chips (§9.2) — omitted gracefully when none.
                            if !d.tags.is_empty() {
                                div { class: "ik-chips", style: "margin:0 0 10px;",
                                    for tag in d.tags.clone() {
                                        span { key: "{tag.id}", class: "ik-chip", "{tag.name}" }
                                    }
                                }
                            }
                            div { class: "ik-stat-inline",
                                div { class: "item", Ic { icon: Icon::Layers, size: 16 } "{chapters_total} ch" }
                                div { class: "item", Ic { icon: Icon::Layers, size: 16 } "{source_count} sources" }
                            }
                            WatchControls {
                                series_id: id.clone(),
                                entry: wl_entry,
                                authed: session.is_authenticated(),
                                reload: reload_wl,
                            }
                        }
                    }
                }
            }
        }
    };

    // Sidebar sources + chapter list depend on loaded detail.
    let sources: Vec<SourceDto> = match &*detail.read_unchecked() {
        Some(Ok(d)) => d.sources.clone(),
        _ => Vec::new(),
    };
    let description: Option<String> = match &*detail.read_unchecked() {
        Some(Ok(d)) => d.description.clone(),
        _ => None,
    };
    let wl_entry_side = current_entry(&watchlist, &id);

    let chapter_body = match &*chapters.read_unchecked() {
        None => rsx! {
            div { class: "ik-chapter-list",
                for _ in 0..6 {
                    div { class: "ik-chapter", div { class: "ik-skeleton", style: "height:14px;width:40%;" } }
                }
            }
        },
        Some(Err(e)) => {
            let msg = e.clone();
            rsx! { div { class: "ik-error", "Could not load chapters: {msg}" } }
        }
        Some(Ok(list)) if list.is_empty() => rsx! {
            div { class: "ik-empty", "No chapters indexed for this source yet." }
        },
        Some(Ok(list)) => {
            let list = list.clone();
            rsx! {
                div { class: "ik-chapter-list",
                    for (i , c) in list.into_iter().enumerate() {
                        ChapterRow { key: "{i}", chapter: c }
                    }
                }
            }
        }
    };

    rsx! {
        {hero}
        div { class: "ik-body-grid", style: "margin-top:24px;",
            // Left column: synopsis + chapters.
            div {
                if let Some(desc) = description {
                    h3 { class: "ik-dayhead", "Synopsis" }
                    p { class: "ik-muted", style: "margin:0 0 22px;max-width:70ch;line-height:1.7;", "{desc}" }
                }
                h3 { class: "ik-dayhead", "Chapters" }
                {chapter_body}
            }
            // Right sidebar: read-on + tracking + related.
            div {
                div { class: "ik-sidebar-card",
                    h4 { "Read on" }
                    if sources.is_empty() {
                        div { class: "ik-muted", style: "font-size:13px;", "No sources linked yet." }
                    } else {
                        for s in sources {
                            SourceCard { key: "{s.id}", source: s, selected }
                        }
                    }
                }
                div { class: "ik-sidebar-card",
                    h4 { "Tracking" }
                    WatchControls {
                        series_id: id.clone(),
                        entry: wl_entry_side,
                        authed: session.is_authenticated(),
                        reload: reload_wl,
                    }
                    div { class: "ik-flex", style: "margin-top:12px;font-size:13px;justify-content:space-between;",
                        span { "AniList" }
                        match &*sync_status.read_unchecked() {
                            Some(Some(s)) if s.linked => rsx! {
                                span { class: "ik-flex", style: "gap:4px;color:var(--jade,#3DA88F);",
                                    Ic { icon: Icon::CloudDone, size: 15 }
                                    "Synced"
                                }
                            },
                            Some(_) => rsx! {
                                Link { to: Route::Account {}, class: "ik-flex", style: "gap:4px;color:inherit;text-decoration:none;",
                                    Ic { icon: Icon::CloudOff, size: 15 }
                                    span { class: "ik-muted", "Not connected" }
                                }
                            },
                            None => rsx! { span { class: "ik-muted", "…" } },
                        }
                    }
                }
                div { class: "ik-sidebar-card",
                    h4 { "Readers also follow" }
                    // TODO(api) §9.3: needs GET /v1/series/:id/related.
                    div { class: "ik-muted", style: "font-size:13px;", "Recommendations arrive with the related-series endpoint." }
                }
            }
        }
    }
}

/// Find this series' watchlist entry (if any) from the loaded watchlist resource.
fn current_entry(
    watchlist: &Resource<Result<Vec<WatchlistItem>, String>>,
    series_id: &str,
) -> Option<WatchlistItem> {
    match &*watchlist.read_unchecked() {
        Some(Ok(list)) => list.iter().find(|i| i.series_id == series_id).cloned(),
        _ => None,
    }
}

fn type_color(t: ContentType) -> &'static str {
    match t {
        ContentType::Manga => "#6FA8DC",
        ContentType::Manhwa => "var(--acc2)",
        ContentType::Manhua => "#3DA88F",
        ContentType::Webtoon => "#CBA43C",
        ContentType::Unknown => "var(--muted)",
    }
}

fn status_color(s: SeriesStatus) -> &'static str {
    match s {
        SeriesStatus::Ongoing => "#3DA88F",
        SeriesStatus::Completed => "#6FA8DC",
        SeriesStatus::Hiatus => "#CBA43C",
        SeriesStatus::Cancelled | SeriesStatus::Unknown => "var(--muted)",
    }
}

/// A "Read on" source card in the sidebar. Selecting it loads that source's chapters in the
/// left column; the trailing link opens the resolved source page in a new tab.
#[component]
fn SourceCard(source: SourceDto, selected: Signal<Option<String>>) -> Element {
    let mut selected = selected;
    let is_selected = selected.read().as_deref() == Some(source.id.as_str());
    let sid = source.id.clone();
    let initial = source
        .provider_name
        .chars()
        .next()
        .unwrap_or('?')
        .to_uppercase()
        .to_string();
    let border = if is_selected {
        "border-color:var(--acc);"
    } else {
        ""
    };
    rsx! {
        div { class: "ik-source-card",
            button {
                class: "ik-flex",
                style: "flex:1;background:none;border:none;cursor:pointer;color:inherit;text-align:left;{border}",
                onclick: move |_| selected.set(Some(sid.clone())),
                div { class: "ik-source-tile", "{initial}" }
                div { class: "grow",
                    div { class: "ik-flex", style: "gap:6px;",
                        span { style: "font-weight:600;font-size:13px;", "{source.provider_name}" }
                        if source.is_primary {
                            span { class: "ik-pill jade", style: "font-size:10px;", "Primary" }
                        }
                    }
                    div { class: "ik-mono ik-muted", style: "font-size:11px;", "{source.chapter_count} ch" }
                }
            }
            a { class: "ik-btn-icon ik-btn", href: "{source.url}", target: "_blank", rel: "noopener", title: "Open source",
                Ic { icon: Icon::OpenInNew, size: 16 }
            }
        }
    }
}

#[component]
fn WatchControls(
    series_id: String,
    entry: Option<WatchlistItem>,
    authed: bool,
    reload: Signal<u32>,
) -> Element {
    let session = use_session();

    if !authed {
        return rsx! {
            span { class: "ik-muted", "Sign in to track this series." }
        };
    }

    let in_list = entry.is_some();
    let notify = entry.as_ref().map(|e| e.notify).unwrap_or(true);
    let status = entry
        .as_ref()
        .map(|e| e.status)
        .unwrap_or(WatchStatus::Reading);

    let add = {
        let sid = series_id.clone();
        let mut reload = reload;
        move |_| {
            let sid = sid.clone();
            spawn(async move {
                if let Some(t) = session.token_value() {
                    let body = WatchlistUpsert {
                        status: WatchStatus::Reading,
                        notify: true,
                    };
                    if api::set_watchlist(&t, &sid, &body).await.is_ok() {
                        reload += 1;
                    }
                }
            });
        }
    };

    let remove = {
        let sid = series_id.clone();
        let mut reload = reload;
        move |_| {
            let sid = sid.clone();
            spawn(async move {
                if let Some(t) = session.token_value() {
                    if api::remove_watchlist(&t, &sid).await.is_ok() {
                        reload += 1;
                    }
                }
            });
        }
    };

    let toggle_notify = {
        let sid = series_id.clone();
        let mut reload = reload;
        move |_| {
            let sid = sid.clone();
            spawn(async move {
                if let Some(t) = session.token_value() {
                    let body = WatchlistUpsert {
                        status,
                        notify: !notify,
                    };
                    if api::set_watchlist(&t, &sid, &body).await.is_ok() {
                        reload += 1;
                    }
                }
            });
        }
    };

    rsx! {
        div { class: "ik-flex", style: "flex-wrap:wrap;",
            if in_list {
                button { class: "ik-btn", onclick: remove,
                    Ic { icon: Icon::Bookmark, size: 16 }
                    "In watchlist"
                }
                button {
                    class: if notify { "ik-btn primary" } else { "ik-btn" },
                    onclick: toggle_notify,
                    Ic { icon: Icon::Notify, size: 16 }
                    if notify { "Notify on" } else { "Notify off" }
                }
            } else {
                button { class: "ik-btn primary", onclick: add,
                    Ic { icon: Icon::Bookmark, size: 16 }
                    "Add to watchlist"
                }
            }
        }
    }
}

#[component]
fn ChapterRow(chapter: crate::models::ChapterDto) -> Element {
    let num = trim_num(chapter.number);
    let label = chapter
        .title
        .clone()
        .filter(|t| !t.is_empty())
        .unwrap_or_else(|| format!("Chapter {num}"));
    let date = chapter
        .published_at
        .as_deref()
        .and_then(|p| p.get(0..10))
        .unwrap_or("")
        .to_owned();
    let url = chapter.url.clone();
    // Auth-scoped read-state (§9.2): `Some(true)` dims the row + shows a check; anonymous
    // callers get `None` and the row renders unmarked.
    let is_read = chapter.read.unwrap_or(false);
    let row_style = if is_read { "opacity:.55;" } else { "" };
    rsx! {
        div { class: "ik-chapter", style: "{row_style}",
            span { class: "num", "#{num}" }
            span { "{label}" }
            if is_read {
                span { class: "ik-flex ik-muted", style: "gap:4px;font-size:11px;",
                    Ic { icon: Icon::Check, size: 13 }
                    "Read"
                }
            }
            span { class: "date", "{date}" }
            a {
                class: "ik-btn",
                style: "margin-left:12px;padding:4px 10px;",
                href: "{url}",
                target: "_blank",
                rel: "noopener",
                if is_read { "Re-read" } else { "Read" }
            }
        }
    }
}

fn trim_num(n: f64) -> String {
    if n.fract() == 0.0 {
        format!("{}", n as i64)
    } else {
        format!("{n}")
    }
}
