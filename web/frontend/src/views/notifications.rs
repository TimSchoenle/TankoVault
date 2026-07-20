//! Notifications (§17.2.5) — chronological, unread emphasised, one-click mark-all-read,
//! deep-links to the series. Also keeps the rail's unread badge in sync.

use crate::api;
use crate::components::{EmptyBox, ErrorBox, SignInGate, UnreadBadge};
use crate::models::Notification;
use crate::state::use_session;
use crate::Route;
use dioxus::prelude::*;

#[component]
pub fn Notifications() -> Element {
    let session = use_session();
    let badge = use_context::<UnreadBadge>();
    let mut reload = use_signal(|| 0u32);

    let resource = use_resource(move || {
        let _ = reload.read();
        async move {
            match session.token_value() {
                Some(t) => api::notifications(&t).await,
                None => Ok(Vec::new()),
            }
        }
    });

    // Keep the rail badge in sync with the number of unread rows whenever data changes.
    use_effect(move || {
        let mut count = badge.0;
        if let Some(Ok(list)) = &*resource.read_unchecked() {
            let unread = list.iter().filter(|n| n.read_at.is_none()).count();
            count.set(unread as i64);
        }
    });

    if !session.is_authenticated() {
        return rsx! {
            h1 { class: "ik-page-title", "Notifications" }
            SignInGate {}
        };
    }

    let mark_all = move |_| {
        // Collect ids and drop the borrow before awaiting.
        let ids: Vec<String> = match &*resource.peek() {
            Some(Ok(list)) => list
                .iter()
                .filter(|n| n.read_at.is_none())
                .map(|n| n.id.clone())
                .collect(),
            _ => Vec::new(),
        };
        spawn(async move {
            if ids.is_empty() {
                return;
            }
            if let Some(t) = session.token_value() {
                if api::mark_read(&t, &ids).await.is_ok() {
                    reload += 1;
                }
            }
        });
    };

    let body = match &*resource.read_unchecked() {
        None => rsx! {
            for _ in 0..5 {
                div { class: "ik-row", div { class: "ik-skeleton", style: "height:16px;width:50%;" } }
            }
        },
        Some(Err(e)) => {
            let msg = e.clone();
            rsx! {
                ErrorBox { message: msg, on_retry: move |()| reload += 1 }
            }
        }
        Some(Ok(items)) if items.is_empty() => rsx! {
            EmptyBox { message: "No notifications yet. We'll ping you when a watched series updates.".to_string() }
        },
        Some(Ok(items)) => {
            let items = items.clone();
            rsx! {
                for n in items {
                    NotifRow { key: "{n.id}", notif: n }
                }
            }
        }
    };

    rsx! {
        div { class: "ik-flex", style: "justify-content:space-between;",
            h1 { class: "ik-page-title", "Notifications" }
            button { class: "ik-btn", onclick: mark_all, "Mark all read" }
        }
        {body}
    }
}

#[component]
fn NotifRow(notif: Notification) -> Element {
    let unread = notif.read_at.is_none();
    let class = if unread { "ik-row unread" } else { "ik-row" };
    let (title, series_id) = describe(&notif);
    let when = notif.created_at.get(0..10).unwrap_or("").to_owned();

    let inner = rsx! {
        div { class: "grow",
            div { style: "font-weight:600;", "{title}" }
            div { class: "ik-muted", style: "font-size:12px;", "{when}" }
        }
        if unread {
            span { class: "ik-pill vermilion", "New" }
        }
    };

    match series_id {
        Some(id) => rsx! {
            Link { to: Route::Series { id }, class: "{class}", {inner} }
        },
        None => rsx! {
            div { class: "{class}", {inner} }
        },
    }
}

/// Derive a human line + optional deep-link target from a notification payload. The
/// notifier writes `{ series_id, series_title, chapter_number, .. }` for chapter events
/// (services/notifier); unknown shapes fall back to the `kind`.
fn describe(n: &Notification) -> (String, Option<String>) {
    let series_title = n
        .payload
        .get("series_title")
        .and_then(|v| v.as_str())
        .map(str::to_owned);
    let series_id = n
        .payload
        .get("series_id")
        .and_then(|v| v.as_str())
        .map(str::to_owned);
    let chapter = n
        .payload
        .get("chapter_number")
        .and_then(serde_json::Value::as_f64);

    let title = match (series_title, chapter) {
        (Some(t), Some(c)) => format!("New chapter {c} of {t}"),
        (Some(t), None) => format!("Update for {t}"),
        _ if !n.kind.is_empty() => n.kind.replace('_', " "),
        _ => "Notification".to_owned(),
    };
    (title, series_id)
}
