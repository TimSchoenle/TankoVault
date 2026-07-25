//! Notifications (§17.2.5) — chronological, unread emphasised, one-click mark-all-read, and
//! deep links into the series. Also keeps the rail's unread badge in sync.
//!
//! The list is free-form JSON on the server (the notifier writes an open `payload` per kind),
//! so this screen reads it defensively: unknown kinds still render, with the kind token as
//! their line, rather than being dropped or crashing the list.

use crate::api;
use crate::components::{async_list, SignInGate, SkeletonRows, UnreadBadge};
use crate::hooks::use_reload;
use crate::icons::{Ic, Icon};
use crate::models::*;
use crate::state::use_session;
use crate::util::iso_date;
use crate::Route;
use dioxus::prelude::*;
use uuid::Uuid;

/// Filter tabs (`DESIGN_SPEC` §7.5), applied client-side to the loaded list.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Tab {
    All,
    Unread,
    Chapters,
    Sync,
}

impl Tab {
    const ALL: [Tab; 4] = [Self::All, Self::Unread, Self::Chapters, Self::Sync];

    fn label(self) -> &'static str {
        match self {
            Self::All => "All",
            Self::Unread => "Unread",
            Self::Chapters => "Chapters",
            Self::Sync => "Sync",
        }
    }

    fn matches(self, notification: &Notification) -> bool {
        match self {
            Self::All => true,
            Self::Unread => read_at(notification).is_none(),
            Self::Chapters => {
                matches!(Kind::of(notification), Kind::NewChapter | Kind::SourceAdded)
            }
            Self::Sync => matches!(Kind::of(notification), Kind::Sync),
        }
    }
}

/// Normalised notification kind → icon + tint.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    NewChapter,
    SourceAdded,
    Completed,
    Sync,
    Unknown,
}

impl Kind {
    fn of(notification: &Notification) -> Self {
        match kind_token(notification) {
            "new_chapter" | "chapter" => Self::NewChapter,
            "source_added" | "source" => Self::SourceAdded,
            "completed" | "series_completed" => Self::Completed,
            "sync" | "sync_event" => Self::Sync,
            // A kind we don't know, but with a chapter number in the payload, is a chapter
            // event under a new name — better to show it correctly than to bucket it as
            // unknown.
            _ if payload(notification).get("chapter_number").is_some() => Self::NewChapter,
            _ => Self::Unknown,
        }
    }

    fn icon(self) -> Icon {
        match self {
            Self::NewChapter => Icon::AutoAwesome,
            Self::SourceAdded => Icon::ArrowForward,
            Self::Completed => Icon::Check,
            Self::Sync => Icon::CloudDone,
            Self::Unknown => Icon::Circle,
        }
    }

    fn color(self) -> &'static str {
        match self {
            Self::NewChapter => "var(--acc)",
            Self::SourceAdded => "var(--color-type-manga)",
            Self::Completed | Self::Sync => "var(--jade-bright)",
            Self::Unknown => "var(--muted)",
        }
    }
}

fn string_field<'a>(notification: &'a Notification, key: &str) -> Option<&'a str> {
    notification.get(key).and_then(|v| v.as_str())
}

/// `None` means unread. A present-but-null `read_at` is unread; a present non-string value is
/// treated as read, since only the presence of a timestamp matters here.
fn read_at(notification: &Notification) -> Option<&str> {
    match notification.get("read_at") {
        None | Some(serde_json::Value::Null) => None,
        Some(value) => value.as_str().or(Some("")),
    }
}

fn kind_token(notification: &Notification) -> &str {
    string_field(notification, "kind").unwrap_or("")
}

fn payload(notification: &Notification) -> &serde_json::Value {
    notification
        .get("payload")
        .unwrap_or(&serde_json::Value::Null)
}

fn id_of(notification: &Notification) -> Option<Uuid> {
    notification
        .get("id")
        .and_then(|v| v.as_str())
        .and_then(|s| s.parse().ok())
}

fn unread_count(list: &[Notification]) -> i64 {
    let count = list.iter().filter(|n| read_at(n).is_none()).count();
    i64::try_from(count).unwrap_or(i64::MAX)
}

#[component]
pub(crate) fn Notifications() -> Element {
    let session = use_session();
    let api = api::use_api();
    let badge = use_context::<UnreadBadge>();
    let reload = use_reload();
    let mut tab = use_signal(|| Tab::All);

    let notifications = use_resource(move || {
        reload.track();
        let client = api.client();
        let authed = session.is_authenticated();
        async move {
            if !authed {
                return Ok(Vec::new());
            }
            client
                .notifications()
                .send()
                .await
                .map(|r| match r.into_inner() {
                    serde_json::Value::Array(items) => items,
                    other => vec![other],
                })
                .map_err(api::friendly_error)
        }
    });

    // Keep the rail badge honest whenever the list changes: the SSE push is best-effort, so
    // this navigation-time recount is what guarantees the two agree.
    use_effect(move || {
        let mut count = badge.0;
        if let Some(Ok(list)) = &*notifications.read_unchecked() {
            count.set(unread_count(list));
        }
    });

    if !session.is_authenticated() {
        return rsx! {
            h1 { class: "ik-page-title", "Notifications" }
            SignInGate {}
        };
    }

    let mark_all = move |_| {
        // Collect the ids and drop the borrow before awaiting.
        let ids: Vec<Uuid> = match &*notifications.peek() {
            Some(Ok(list)) => list
                .iter()
                .filter(|n| read_at(n).is_none())
                .filter_map(id_of)
                .collect(),
            _ => Vec::new(),
        };
        if ids.is_empty() {
            return;
        }
        let client = api.client();
        spawn(async move {
            if client
                .mark_read()
                .body(MarkRead { ids })
                .send()
                .await
                .is_ok()
            {
                reload.bump();
            }
        });
    };

    let current = *tab.read();
    let unread = match &*notifications.read_unchecked() {
        Some(Ok(list)) => unread_count(list),
        _ => 0,
    };

    rsx! {
        div { class: "ik-page-head",
            div {
                h1 { class: "ik-page-title", style: "margin-bottom:2px;", "Notifications" }
                div { class: "ik-mono ik-muted", style: "font-size:12px;", "{unread} unread · live push via SSE" }
            }
            button { class: "ik-btn", disabled: unread == 0, onclick: mark_all, "Mark all read" }
        }
        div { class: "ik-tabs",
            for t in Tab::ALL {
                button {
                    key: "{t.label()}",
                    class: if current == t { "ik-tab active" } else { "ik-tab" },
                    onclick: move |_| tab.set(t),
                    "{t.label()}"
                }
            }
        }
        {
            async_list(
                &notifications,
                reload,
                || rsx! { SkeletonRows { count: 5 } },
                "No notifications yet. We'll ping you when a watched series updates.",
                |items| {
                    let filtered: Vec<&Notification> =
                        items.iter().filter(|n| current.matches(n)).collect();
                    if filtered.is_empty() {
                        return rsx! {
                            div { class: "ik-empty", "Nothing in this filter." }
                        };
                    }
                    rsx! {
                        for (index , notification) in filtered.into_iter().enumerate() {
                            NotifRow {
                                key: "{id_of(notification).map_or_else(|| index.to_string(), |id| id.to_string())}",
                                notification: notification.clone(),
                            }
                        }
                    }
                },
            )
        }
    }
}

#[component]
fn NotifRow(notification: Notification) -> Element {
    let unread = read_at(&notification).is_none();
    let class = if unread { "ik-row unread" } else { "ik-row" };
    let (title, series_id) = describe(&notification);
    let when = iso_date(string_field(&notification, "created_at")).to_owned();
    let kind = Kind::of(&notification);
    let tile = format!(
        "background:color-mix(in srgb, {c} 16%, transparent);color:{c};",
        c = kind.color()
    );

    let inner = rsx! {
        div { class: "ik-kind", style: "{tile}",
            Ic { icon: kind.icon(), size: 18 }
        }
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

/// Derive a human line and an optional deep-link target from a notification payload.
///
/// The notifier writes `{ series_id, series_title, chapter_number, .. }` for chapter events;
/// an unrecognised shape degrades to the kind token with the underscores spaced out, which is
/// still readable, rather than to a generic placeholder that tells the reader nothing.
fn describe(notification: &Notification) -> (String, Option<String>) {
    let payload = payload(notification);
    let series_title = payload
        .get("series_title")
        .and_then(|v| v.as_str())
        .map(str::to_owned);
    let series_id = payload
        .get("series_id")
        .and_then(|v| v.as_str())
        .map(str::to_owned);
    let chapter = payload
        .get("chapter_number")
        .and_then(serde_json::Value::as_f64);

    let kind = kind_token(notification);
    let title = match (series_title, chapter) {
        (Some(title), Some(number)) => {
            format!(
                "New chapter {} of {title}",
                crate::util::chapter_number(number)
            )
        }
        (Some(title), None) => format!("Update for {title}"),
        _ if !kind.is_empty() => kind.replace('_', " "),
        _ => "Notification".to_owned(),
    };
    (title, series_id)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn a_missing_or_null_read_at_means_unread() {
        assert!(read_at(&json!({})).is_none());
        assert!(read_at(&json!({ "read_at": null })).is_none());
        assert!(read_at(&json!({ "read_at": "2026-07-25T00:00:00Z" })).is_some());
    }

    #[test]
    fn classifies_an_unnamed_kind_with_a_chapter_number_as_a_chapter_event() {
        let n = json!({ "kind": "brand_new_kind", "payload": { "chapter_number": 12.0 } });
        assert!(matches!(Kind::of(&n), Kind::NewChapter));
    }

    #[test]
    fn describes_a_chapter_event_and_links_to_the_series() {
        let n = json!({
            "kind": "new_chapter",
            "payload": { "series_id": "abc", "series_title": "Blame!", "chapter_number": 7.0 },
        });
        assert_eq!(
            describe(&n),
            ("New chapter 7 of Blame!".to_owned(), Some("abc".to_owned()))
        );
    }

    #[test]
    fn falls_back_to_the_kind_token_for_an_unknown_shape() {
        let n = json!({ "kind": "some_event" });
        assert_eq!(describe(&n), ("some event".to_owned(), None));
    }

    #[test]
    fn counts_only_unread_rows() {
        let list = vec![
            json!({ "read_at": null }),
            json!({ "read_at": "2026-07-25T00:00:00Z" }),
            json!({}),
        ];
        assert_eq!(unread_count(&list), 2);
    }
}
