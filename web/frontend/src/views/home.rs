//! Home dashboard (`DESIGN_SPEC` §7.1) — the signed-in reader's landing screen. Greeting +
//! lifetime stat tiles, a continue-reading rail, a day-grouped "New in your watchlist" feed,
//! and a "Because you read" recommendations shelf.

use crate::api;
use crate::components::{
    async_list, async_view, Cover, CoverCard, SignInGate, SkeletonBlock, SkeletonRows,
};
use crate::hooks::{use_reload, Reload};
use crate::i18n::use_i18n;
use crate::icons::{Ic, Icon};
use crate::models::*;
use crate::state::use_session;
use crate::util::{chapter_number, greeting_key, iso_date};
use crate::Route;
use dioxus::prelude::*;
use progenitor_client::ResponseValue;

#[component]
pub(crate) fn Home() -> Element {
    let session = use_session();
    let i18n = use_i18n();
    let api = api::use_api();
    let reload = use_reload();

    // Each resource builds its client from the *live* session token, so the boot-time silent
    // refresh — which lands a moment after first paint on a reload — automatically refetches
    // everything instead of leaving the screen stuck on its signed-out result.
    let feed = use_resource(move || {
        reload.track();
        let client = api.client();
        let authed = session.is_authenticated();
        async move {
            if !authed {
                return Ok(Vec::new());
            }
            client
                .feed()
                .send()
                .await
                .map(ResponseValue::into_inner)
                .map_err(|e| api::friendly_error(i18n, e))
        }
    });

    let stats = use_resource(move || {
        reload.track();
        let client = api.client();
        let authed = session.is_authenticated();
        async move {
            if !authed {
                return Ok(None);
            }
            client
                .stats()
                .send()
                .await
                .map(|r| Some(r.into_inner()))
                .map_err(|e| api::friendly_error(i18n, e))
        }
    });

    let continuing = use_resource(move || {
        reload.track();
        let client = api.client();
        let authed = session.is_authenticated();
        async move {
            if !authed {
                return Ok(Vec::new());
            }
            client
                .continue_reading()
                .send()
                .await
                .map(ResponseValue::into_inner)
                .map_err(|e| api::friendly_error(i18n, e))
        }
    });

    let recommendations = use_resource(move || {
        let client = api.client();
        let authed = session.is_authenticated();
        async move {
            if !authed {
                return Ok(Vec::new());
            }
            client
                .recommendations()
                .send()
                .await
                .map(ResponseValue::into_inner)
                .map_err(|e| api::friendly_error(i18n, e))
        }
    });

    if !session.is_authenticated() {
        return rsx! {
            h1 { class: "ik-page-title", {i18n.t("nav.home")} }
            SignInGate {}
        };
    }

    let name = session
        .username()
        .unwrap_or_else(|| i18n.t("common.readerFallback"));

    // Tile values render as an em dash until their call resolves, rather than as a
    // provisional zero the reader would read as a real figure.
    let new_chapters = match &*feed.read_unchecked() {
        Some(Ok(items)) => items.len().to_string(),
        _ => "—".to_owned(),
    };
    let (reading, chapters_read) = match &*stats.read_unchecked() {
        Some(Ok(Some(stats))) => (stats.reading.to_string(), stats.chapters_read.to_string()),
        _ => ("—".to_owned(), "—".to_owned()),
    };

    rsx! {
        div { class: "ik-home-head",
            div {
                div { class: "ik-kicker", {i18n.t(greeting_key())} }
                h1 { class: "ik-page-title", style: "margin:6px 0 0;",
                    {i18n.args("home.welcome", &[("name", &name)])}
                }
            }
            div { class: "ik-stat-row",
                StatTile { icon: Icon::Bolt, label: i18n.t("home.stat.newChapters"), value: new_chapters, tone: "acc" }
                StatTile { icon: Icon::MenuBook, label: i18n.t("home.stat.reading"), value: reading, tone: "" }
                StatTile { icon: Icon::Check, label: i18n.t("home.stat.chaptersRead"), value: chapters_read, tone: "jade" }
            }
        }

        div { class: "ik-section-head",
            Ic { icon: Icon::PlayCircle, size: 20 }
            h2 { {i18n.t("home.continue.title")} }
        }
        {
            async_list(
                &continuing,
                reload,
                || rsx! { SkeletonBlock { height: 96 } },
                &i18n.t("home.continue.empty"),
                |items| rsx! {
                    div { class: "ik-grid",
                        for item in items.iter().cloned() {
                            ContinueCard { key: "{item.series_id}", item }
                        }
                    }
                },
            )
        }

        div { class: "ik-section-head",
            Ic { icon: Icon::Bolt, size: 20 }
            h2 { {i18n.t("home.feed.title")} }
            Link { to: Route::Notifications {}, class: "more", {i18n.t("common.seeAll")} }
        }
        {
            async_list(
                &feed,
                reload,
                || rsx! { SkeletonRows { count: 3 } },
                &i18n.t("home.feed.empty"),
                |items| rsx! {
                    for (day , entries) in group_by_day(items) {
                        div { class: "ik-daygroup", key: "{day}",
                            div { class: "ik-dayhead", "{day}" }
                            for entry in entries {
                                FeedRow { key: "{entry.series_id}-{entry.chapter_number}", entry, reload }
                            }
                        }
                    }
                },
            )
        }

        // Recommendations are a bonus shelf: when there is nothing to suggest the whole
        // section disappears rather than showing an empty state for something unasked for.
        {
            async_view(
                &recommendations,
                reload,
                || rsx! { SkeletonBlock { height: 96 } },
                |items| {
                    if items.is_empty() {
                        return rsx! {};
                    }
                    rsx! {
                        div { class: "ik-section-head",
                            Ic { icon: Icon::AutoAwesome, size: 20 }
                            h2 { {i18n.t("home.recommendations.title")} }
                        }
                        div { class: "ik-grid",
                            for series in items.iter().cloned() {
                                CoverCard { key: "{series.id}", series }
                            }
                        }
                    }
                },
            )
        }
    }
}

/// One lifetime-stat tile in the header row.
#[component]
fn StatTile(icon: Icon, label: String, value: String, tone: &'static str) -> Element {
    rsx! {
        div { class: "ik-stat",
            div { class: "lbl",
                Ic { icon, size: 13 }
                "{label}"
            }
            div { class: "val {tone}", "{value}" }
        }
    }
}

/// A continue-reading card: cover plus the next unread chapter, linking to the series so the
/// reader can resume.
#[component]
fn ContinueCard(item: ContinueItem) -> Element {
    let i18n = use_i18n();
    let next = item.next_number.map(chapter_number);
    let last = chapter_number(item.last_read_number);
    rsx! {
        Link { to: Route::Series { id: item.series_id.to_string() }, class: "ik-card",
            Cover { url: item.cover_url.clone(), title: item.series_title.clone() }
            div { class: "ik-card-body",
                div { class: "ik-card-title", "{item.series_title}" }
                div { class: "ik-card-meta",
                    match next {
                        Some(n) => rsx! {
                            span { {i18n.args("home.continue.next", &[("number", &n)])} }
                        },
                        None => rsx! {
                            span { {i18n.args("home.continue.read", &[("number", &last)])} }
                        },
                    }
                    span { class: "ik-rail-spacer" }
                    if item.unread > 0 {
                        span { class: "ik-pill acc", style: "font-size:10px;",
                            {i18n.args("home.continue.new", &[("count", &item.unread.to_string())])}
                        }
                    }
                }
            }
        }
    }
}

/// One newly-discovered chapter, with an open link and a mark-read action.
#[component]
fn FeedRow(entry: FeedEntry, reload: Reload) -> Element {
    let api = api::use_api();
    let i18n = use_i18n();
    let series_id = entry.series_id;
    let number = entry.chapter_number;
    let label = entry
        .chapter_title
        .clone()
        .filter(|title| !title.is_empty())
        .unwrap_or_else(|| {
            i18n.args(
                "series.chapterNumbered",
                &[("number", &chapter_number(number))],
            )
        });

    let mark_read = move |_| {
        let client = api.client();
        spawn(async move {
            let body = ProgressUpdate {
                last_read_whole_number: number,
            };
            if client
                .put_progress()
                .series_id(series_id)
                .body(body)
                .send()
                .await
                .is_ok()
            {
                reload.bump();
            }
        });
    };

    rsx! {
        div { class: "ik-row unread",
            span { class: "ik-mono", style: "color:var(--acc);min-width:56px;", "#{chapter_number(number)}" }
            div { class: "grow",
                div { style: "font-weight:600;", "{entry.series_title}" }
                div { class: "ik-muted", style: "font-size:13px;", "{label} · {entry.provider_slug}" }
            }
            a { class: "ik-btn", href: "{entry.url}", target: "_blank", rel: "noopener", {i18n.t("common.open")} }
            button { class: "ik-btn primary", onclick: mark_read, {i18n.t("common.markRead")} }
        }
    }
}

/// Group feed entries by the date component of `discovered_at`, preserving the server's
/// newest-first ordering.
///
/// Relies on that ordering: entries for one day are contiguous, so a single pass suffices and
/// no sort is needed.
fn group_by_day(items: &[FeedEntry]) -> Vec<(String, Vec<FeedEntry>)> {
    let mut groups: Vec<(String, Vec<FeedEntry>)> = Vec::new();
    for entry in items {
        let day = iso_date(Some(&entry.discovered_at)).to_owned();
        match groups.last_mut() {
            Some((current, entries)) if *current == day => entries.push(entry.clone()),
            _ => groups.push((day, vec![entry.clone()])),
        }
    }
    groups
}
