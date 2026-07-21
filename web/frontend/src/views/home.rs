//! Home dashboard (DESIGN_SPEC §7.1) — the signed-in reader's landing screen. Greeting +
//! lifetime stat tiles (`GET /v1/me/stats`), a continue-reading rail (`GET /v1/me/continue`),
//! a "New in your watchlist" day-grouped feed, and a "Because you read" recommendations shelf
//! (`GET /v1/me/recommendations`) — all §9.3.

use crate::api;
use crate::components::{Cover, CoverCard, EmptyBox, ErrorBox, SignInGate};
use crate::icons::{Ic, Icon};
use crate::models::{ContinueItem, FeedEntry};
use crate::state::use_session;
use crate::Route;
use dioxus::prelude::*;

#[component]
pub fn Home() -> Element {
    let session = use_session();
    let mut reload = use_signal(|| 0u32);

    let feed = use_resource(move || {
        let _ = reload.read();
        async move {
            match session.token_value() {
                Some(t) => api::feed(&t).await,
                None => Ok(Vec::new()),
            }
        }
    });
    let stats = use_resource(move || {
        let _ = reload.read();
        async move {
            match session.token_value() {
                Some(t) => api::me_stats(&t).await.ok(),
                None => None,
            }
        }
    });
    let cont = use_resource(move || {
        let _ = reload.read();
        async move {
            match session.token_value() {
                Some(t) => api::continue_reading(&t).await.unwrap_or_default(),
                None => Vec::new(),
            }
        }
    });
    let recs = use_resource(move || async move {
        match session.token_value() {
            Some(t) => api::recommendations(&t).await.unwrap_or_default(),
            None => Vec::new(),
        }
    });

    if !session.is_authenticated() {
        return rsx! {
            h1 { class: "ik-page-title", "Home" }
            SignInGate {}
        };
    }

    let name = session.username().unwrap_or_else(|| "reader".to_owned());
    let greeting = greeting_for_hour();

    // Stat tiles from the lifetime stats endpoint (§9.3); the "new chapters" figure stays the
    // current unread-feed length. Values render as "—" until the stats call resolves.
    let new_count = match &*feed.read_unchecked() {
        Some(Ok(items)) => items.len(),
        _ => 0,
    };
    let me = match &*stats.read_unchecked() {
        Some(Some(s)) => Some(s.clone()),
        _ => None,
    };
    let reading_count = me.as_ref().map(|s| s.reading.to_string()).unwrap_or_else(|| "—".to_owned());
    let chapters_read = me.as_ref().map(|s| s.chapters_read.to_string()).unwrap_or_else(|| "—".to_owned());

    let feed_body = match &*feed.read_unchecked() {
        None => rsx! {
            for _ in 0..3 {
                div { class: "ik-row",
                    div { class: "ik-skeleton", style: "height:16px;width:40%;" }
                }
            }
        },
        Some(Err(e)) => {
            let msg = e.clone();
            rsx! {
                ErrorBox { message: msg, on_retry: move |()| reload += 1 }
            }
        }
        Some(Ok(items)) if items.is_empty() => rsx! {
            EmptyBox {
                message: "You're all caught up. New chapters from your watchlist will land here."
                    .to_string(),
            }
        },
        Some(Ok(items)) => {
            let groups = group_by_day(items);
            rsx! {
                for (day , entries) in groups {
                    div { class: "ik-daygroup",
                        div { class: "ik-dayhead", "{day}" }
                        for entry in entries {
                            FeedRow { entry, reload }
                        }
                    }
                }
            }
        }
    };

    // Continue-reading rail (§9.3): in-progress, tracked series with unread chapters.
    let continue_body = match &*cont.read_unchecked() {
        None => rsx! { div { class: "ik-skeleton", style: "height:96px;" } },
        Some(list) if list.is_empty() => rsx! {
            EmptyBox {
                message: "Start reading a tracked series and it'll show up here so you can pick up where you left off."
                    .to_string(),
            }
        },
        Some(list) => {
            let cards = list.clone();
            rsx! {
                div { class: "ik-grid",
                    for c in cards {
                        ContinueCard { key: "{c.series_id}", item: c }
                    }
                }
            }
        }
    };

    // "Because you read" recommendations (§9.3): shared-tag suggestions, recent fallback.
    let recs_body = match &*recs.read_unchecked() {
        None => rsx! { div { class: "ik-skeleton", style: "height:96px;" } },
        Some(list) if list.is_empty() => rsx! {},
        Some(list) => {
            let items = list.clone();
            rsx! {
                div { class: "ik-section-head",
                    Ic { icon: Icon::AutoAwesome, size: 20 }
                    h2 { "Because you read" }
                }
                div { class: "ik-grid",
                    for s in items {
                        CoverCard { key: "{s.id}", series: s }
                    }
                }
            }
        }
    };

    rsx! {
        div { style: "display:flex;align-items:flex-end;justify-content:space-between;gap:16px;flex-wrap:wrap;margin-bottom:8px;",
            div {
                div { class: "ik-kicker", "{greeting}" }
                h1 { class: "ik-page-title", style: "margin:6px 0 0;", "Welcome back, {name}" }
            }
            div { class: "ik-stat-row",
                div { class: "ik-stat",
                    div { class: "lbl",
                        Ic { icon: Icon::Bolt, size: 13 }
                        "New chapters"
                    }
                    div { class: "val acc", "{new_count}" }
                }
                div { class: "ik-stat",
                    div { class: "lbl",
                        Ic { icon: Icon::MenuBook, size: 13 }
                        "Reading"
                    }
                    div { class: "val", "{reading_count}" }
                }
                div { class: "ik-stat",
                    div { class: "lbl",
                        Ic { icon: Icon::Check, size: 13 }
                        "Chapters read"
                    }
                    div { class: "val jade", "{chapters_read}" }
                }
            }
        }

        // Continue reading (§9.3).
        div { class: "ik-section-head",
            Ic { icon: Icon::PlayCircle, size: 20 }
            h2 { "Continue reading" }
        }
        {continue_body}

        // New in your watchlist (the folded-in feed).
        div { class: "ik-section-head",
            Ic { icon: Icon::Bolt, size: 20 }
            h2 { "New in your watchlist" }
            Link { to: Route::Notifications {}, class: "more", "See all" }
        }
        {feed_body}

        {recs_body}
    }
}

/// A continue-reading card (§9.3): cover + last-read/next-unread progress, linking to the
/// series page so the reader can resume. Unread count is shown as a small badge.
#[component]
fn ContinueCard(item: ContinueItem) -> Element {
    let last = trim_num(item.last_read_number);
    let next = item.next_number.map(trim_num);
    let unread = item.unread;
    rsx! {
        Link { to: Route::Series { id: item.series_id.clone() }, class: "ik-card",
            Cover { url: item.cover_url.clone(), title: item.series_title.clone() }
            div { class: "ik-card-body",
                div { class: "ik-card-title", "{item.series_title}" }
                div { class: "ik-card-meta",
                    if let Some(n) = next.clone() {
                        span { "Next #{n}" }
                    } else {
                        span { "Read #{last}" }
                    }
                    span { class: "ik-rail-spacer" }
                    if unread > 0 {
                        span { class: "ik-pill acc", style: "font-size:10px;", "{unread} new" }
                    }
                }
            }
        }
    }
}

#[component]
fn FeedRow(entry: FeedEntry, reload: Signal<u32>) -> Element {
    let session = use_session();
    let mut reload = reload;
    let series_id = entry.series_id.clone();
    let number = entry.chapter_number;
    let chapter_label = entry
        .chapter_title
        .clone()
        .filter(|t| !t.is_empty())
        .unwrap_or_else(|| format!("Chapter {}", trim_num(number)));

    let mark = move |_| {
        let sid = series_id.clone();
        spawn(async move {
            if let Some(t) = session.token_value() {
                if api::set_progress(&t, &sid, number).await.is_ok() {
                    reload += 1;
                }
            }
        });
    };

    rsx! {
        div { class: "ik-row unread",
            span { class: "ik-mono", style: "color:var(--acc);min-width:56px;", "#{trim_num(number)}" }
            div { class: "grow",
                div { style: "font-weight:600;", "{entry.series_title}" }
                div { class: "ik-muted", style: "font-size:13px;", "{chapter_label} · {entry.provider_slug}" }
            }
            a { class: "ik-btn", href: "{entry.url}", target: "_blank", rel: "noopener", "Open" }
            button { class: "ik-btn primary", onclick: mark, "Mark read" }
        }
    }
}

/// Time-of-day greeting from the browser clock.
fn greeting_for_hour() -> &'static str {
    let hour = js_sys::Date::new_0().get_hours();
    match hour {
        5..=11 => "Good morning",
        12..=17 => "Good afternoon",
        18..=21 => "Good evening",
        _ => "Late night",
    }
}

/// Group feed entries by the date component (YYYY-MM-DD) of `discovered_at`, preserving the
/// server's newest-first ordering.
fn group_by_day(items: &[FeedEntry]) -> Vec<(String, Vec<FeedEntry>)> {
    let mut out: Vec<(String, Vec<FeedEntry>)> = Vec::new();
    for e in items {
        let day = e.discovered_at.get(0..10).unwrap_or("").to_owned();
        match out.last_mut() {
            Some((d, v)) if *d == day => v.push(e.clone()),
            _ => out.push((day, vec![e.clone()])),
        }
    }
    out
}

/// Render a chapter number without a trailing `.0` for whole numbers.
fn trim_num(n: f64) -> String {
    if n.fract() == 0.0 {
        format!("{}", n as i64)
    } else {
        format!("{n}")
    }
}
