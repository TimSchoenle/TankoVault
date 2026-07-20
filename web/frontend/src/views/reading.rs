//! Reading dashboard (§17.2.3) — the reader's home. Shows the "New chapters" feed of
//! unread chapters across the watchlist, grouped by day, with vermilion unread badges and
//! a one-tap "Mark read to here" that advances progress.

use crate::api;
use crate::components::{EmptyBox, ErrorBox, SignInGate};
use crate::models::FeedEntry;
use crate::state::use_session;
use dioxus::prelude::*;

#[component]
pub fn Reading() -> Element {
    let session = use_session();
    let mut reload = use_signal(|| 0u32);

    let resource = use_resource(move || {
        let _ = reload.read();
        async move {
            match session.token_value() {
                Some(t) => api::feed(&t).await,
                None => Ok(Vec::new()),
            }
        }
    });

    // Rules of hooks: all hooks above run unconditionally; branch only in render.
    if !session.is_authenticated() {
        return rsx! {
            h1 { class: "ik-page-title", "Reading" }
            SignInGate {}
        };
    }

    let body = match &*resource.read_unchecked() {
        None => rsx! {
            for _ in 0..4 {
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

    rsx! {
        h1 { class: "ik-page-title", "New chapters" }
        {body}
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
            span { class: "ik-mono", style: "color:var(--vermilion);min-width:56px;", "#{trim_num(number)}" }
            div { class: "grow",
                div { style: "font-weight:600;", "{entry.series_title}" }
                div { class: "ik-muted", style: "font-size:13px;", "{chapter_label} · {entry.provider_slug}" }
            }
            a { class: "ik-btn", href: "{entry.url}", target: "_blank", rel: "noopener", "Open" }
            button { class: "ik-btn primary", onclick: mark, "Mark read" }
        }
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
