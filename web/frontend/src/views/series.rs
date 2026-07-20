//! Series detail (§17.2.2) — hero (cover + title + meta + description), a Sources tab strip
//! ("Read on: A · B · C") that opens the resolved link in a new tab, a chapter list with
//! mono numbers + published dates, and watchlist / notify toggles.

use crate::api;
use crate::components::{Cover, ErrorBox};
use crate::models::{WatchStatus, WatchlistItem, WatchlistUpsert};
use crate::state::use_session;
use dioxus::prelude::*;

#[component]
pub fn Series(id: String) -> Element {
    let session = use_session();
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
        async move { api::series_chapters(&id, src.as_deref()).await }
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

    let hero = match &*detail.read_unchecked() {
        None => rsx! {
            div { class: "ik-hero",
                div { class: "ik-skeleton ik-skel-cover" }
                div {
                    div { class: "ik-skeleton", style: "height:28px;width:60%;margin-bottom:12px;" }
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
            rsx! {
                div { class: "ik-hero",
                    div { Cover { url: d.cover_url.clone(), title: d.title.clone() } }
                    div {
                        h1 { class: "ik-page-title", style: "margin-top:0;", "{d.title}" }
                        div { class: "ik-flex", style: "margin-bottom:8px;",
                            span { class: "ik-pill", "{d.content_type.label()}" }
                            span { class: "ik-pill", "{d.status.label()}" }
                            if let Some(y) = d.release_year {
                                span { class: "ik-pill ik-mono", "{y}" }
                            }
                        }
                        WatchControls {
                            series_id: id.clone(),
                            entry: wl_entry,
                            authed: session.is_authenticated(),
                            reload: reload_wl,
                        }
                        if let Some(desc) = d.description.clone() {
                            p { class: "ik-muted", style: "margin-top:14px;max-width:64ch;", "{desc}" }
                        }
                        div { class: "ik-sources",
                            span { class: "ik-muted", "Read on:" }
                            for s in d.sources.clone() {
                                SourceChip { source: s, selected }
                            }
                        }
                    }
                }
            }
        }
    };

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
        h3 { class: "ik-dayhead", style: "margin-top:22px;", "Chapters" }
        {chapter_body}
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

#[component]
fn SourceChip(source: crate::models::SourceDto, selected: Signal<Option<String>>) -> Element {
    let mut selected = selected;
    let is_selected = selected.read().as_deref() == Some(source.id.as_str());
    let class = if is_selected {
        "ik-chip active"
    } else {
        "ik-chip"
    };
    let sid = source.id.clone();
    rsx! {
        span { class: "ik-flex",
            button {
                class: "{class}",
                r#type: "button",
                onclick: move |_| selected.set(Some(sid.clone())),
                "{source.provider_name} · {source.chapter_count}"
            }
            a { class: "ik-pill", href: "{source.url}", target: "_blank", rel: "noopener", "↗" }
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
        div { class: "ik-flex",
            if in_list {
                button { class: "ik-btn", onclick: remove, "In watchlist ✓" }
                button {
                    class: if notify { "ik-btn primary" } else { "ik-btn" },
                    onclick: toggle_notify,
                    if notify { "🔔 Notify on" } else { "Notify off" }
                }
            } else {
                button { class: "ik-btn primary", onclick: add, "Add to watchlist" }
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
    rsx! {
        div { class: "ik-chapter",
            span { class: "num", "#{num}" }
            span { "{label}" }
            span { class: "date", "{date}" }
            a {
                class: "ik-btn",
                style: "margin-left:12px;padding:4px 10px;",
                href: "{url}",
                target: "_blank",
                rel: "noopener",
                "Read"
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
