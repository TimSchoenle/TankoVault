//! Notifications (§17.2.5) — chronological, unread emphasised, one-click mark-all-read,
//! deep-links to the series. Also keeps the rail's unread badge in sync.

use crate::api;
use crate::components::{EmptyBox, ErrorBox, SignInGate, UnreadBadge};
use crate::icons::{Ic, Icon};
use crate::models::*;
use crate::state::use_session;
use crate::Route;
use dioxus::prelude::*;
use uuid::Uuid;

/// Filter tabs (DESIGN_SPEC §7.5). Filters the loaded list client-side by `kind`.
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
    fn matches(self, n: &Notification) -> bool {
        match self {
            Self::All => true,
            Self::Unread => notif_read_at(n).is_none(),
            Self::Chapters => matches!(kind_of(n), NotifKind::NewChapter | NotifKind::SourceAdded),
            Self::Sync => matches!(kind_of(n), NotifKind::Sync),
        }
    }
}

/// Normalised notification kind → icon + tint.
#[derive(Clone, Copy, PartialEq, Eq)]
enum NotifKind {
    NewChapter,
    SourceAdded,
    Completed,
    Sync,
    Unknown,
}

fn notif_str<'a>(n: &'a Notification, key: &str) -> Option<&'a str> {
    n.get(key).and_then(|v| v.as_str())
}

fn notif_read_at(n: &Notification) -> Option<&str> {
    match n.get("read_at") {
        None | Some(serde_json::Value::Null) => None,
        Some(v) => v.as_str().or(Some("")),
    }
}

fn notif_kind_str(n: &Notification) -> &str {
    notif_str(n, "kind").unwrap_or("")
}

fn notif_payload(n: &Notification) -> &serde_json::Value {
    n.get("payload").unwrap_or(&serde_json::Value::Null)
}

fn notif_id(n: &Notification) -> Option<Uuid> {
    n.get("id").and_then(|v| {
        v.as_str()
            .and_then(|s| s.parse().ok())
            .or_else(|| serde_json::from_value::<Uuid>(v.clone()).ok())
    })
}

fn kind_of(n: &Notification) -> NotifKind {
    match notif_kind_str(n) {
        "new_chapter" | "chapter" => NotifKind::NewChapter,
        "source_added" | "source" => NotifKind::SourceAdded,
        "completed" | "series_completed" => NotifKind::Completed,
        "sync" | "sync_event" => NotifKind::Sync,
        _ if notif_payload(n).get("chapter_number").is_some() => NotifKind::NewChapter,
        _ => NotifKind::Unknown,
    }
}

impl NotifKind {
    fn icon(self) -> Icon {
        match self {
            Self::NewChapter => Icon::AutoAwesome,
            Self::SourceAdded => Icon::ArrowForward,
            Self::Completed => Icon::Check,
            Self::Sync => Icon::CloudDone,
            Self::Unknown => Icon::Circle,
        }
    }
    /// Icon-tile tint color (matches the design's kind→color map).
    fn color(self) -> &'static str {
        match self {
            Self::NewChapter => "var(--acc)",
            Self::SourceAdded => "#6FA8DC",
            Self::Completed | Self::Sync => "var(--jade-bright)",
            Self::Unknown => "var(--muted)",
        }
    }
}

fn parse_notifications(value: serde_json::Value) -> Vec<Notification> {
    match value {
        serde_json::Value::Array(items) => items,
        other => vec![other],
    }
}

#[component]
pub fn Notifications() -> Element {
    let session = use_session();
    let badge = use_context::<UnreadBadge>();
    let mut reload = use_signal(|| 0u32);
    let mut tab = use_signal(|| Tab::All);
    let api_client = api::use_api();

    let resource = {
        let client = api_client.clone();
        use_resource(move || {
            let _ = reload.read();
            let client = client.clone();
            async move {
                if session.is_authenticated() {
                    client
                        .notifications()
                        .send()
                        .await
                        .map(|r| parse_notifications(r.into_inner()))
                        .map_err(api::friendly_error)
                } else {
                    Ok(Vec::new())
                }
            }
        })
    };

    // Keep the rail badge in sync with the number of unread rows whenever data changes.
    use_effect(move || {
        let mut count = badge.0;
        if let Some(Ok(list)) = &*resource.read_unchecked() {
            let unread = list.iter().filter(|n| notif_read_at(n).is_none()).count();
            count.set(unread as i64);
        }
    });

    if !session.is_authenticated() {
        return rsx! {
            h1 { class: "ik-page-title", "Notifications" }
            SignInGate {}
        };
    }

    let mark_all = {
        let client = api_client.clone();
        move |_| {
            // Collect ids and drop the borrow before awaiting.
            let ids: Vec<Uuid> = match &*resource.peek() {
                Some(Ok(list)) => list
                    .iter()
                    .filter(|n| notif_read_at(n).is_none())
                    .filter_map(notif_id)
                    .collect(),
                _ => Vec::new(),
            };
            if ids.is_empty() {
                return;
            }
            let client = client.clone();
            spawn(async move {
                let body = MarkRead { ids };
                if client.mark_read().body(body).send().await.is_ok() {
                    reload += 1;
                }
            });
        }
    };

    let current = *tab.read();
    let unread_total = match &*resource.read_unchecked() {
        Some(Ok(list)) => list.iter().filter(|n| notif_read_at(n).is_none()).count(),
        _ => 0,
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
            let filtered: Vec<Notification> = items
                .iter()
                .filter(|n| current.matches(n))
                .cloned()
                .collect();
            if filtered.is_empty() {
                rsx! {
                    EmptyBox { message: "Nothing in this filter.".to_string() }
                }
            } else {
                rsx! {
                    for n in filtered {
                        NotifRow { key: "{notif_id(&n).map(|id| id.to_string()).unwrap_or_default()}", notif: n }
                    }
                }
            }
        }
    };

    rsx! {
        div { class: "ik-flex", style: "justify-content:space-between;align-items:flex-end;",
            div {
                h1 { class: "ik-page-title", style: "margin-bottom:2px;", "Notifications" }
                div { class: "ik-mono ik-muted", style: "font-size:12px;", "{unread_total} unread · live push via SSE" }
            }
            button { class: "ik-btn", onclick: mark_all, "Mark all read" }
        }
        div { class: "ik-tabs",
            for t in Tab::ALL {
                button {
                    class: if current == t { "ik-tab active" } else { "ik-tab" },
                    onclick: move |_| tab.set(t),
                    "{t.label()}"
                }
            }
        }
        {body}
    }
}

#[component]
fn NotifRow(notif: Notification) -> Element {
    let unread = notif_read_at(&notif).is_none();
    let class = if unread { "ik-row unread" } else { "ik-row" };
    let (title, series_id) = describe(&notif);
    let when = notif_str(&notif, "created_at")
        .and_then(|p| p.get(0..10))
        .unwrap_or("")
        .to_owned();
    let kind = kind_of(&notif);
    let tile_style = format!(
        "background:color-mix(in srgb, {c} 16%, transparent);color:{c};",
        c = kind.color()
    );

    let inner = rsx! {
        div { class: "ik-kind", style: "{tile_style}",
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

/// Derive a human line + optional deep-link target from a notification payload. The
/// notifier writes `{ series_id, series_title, chapter_number, .. }` for chapter events
/// (services/notifier); unknown shapes fall back to the `kind`.
fn describe(n: &Notification) -> (String, Option<String>) {
    let payload = notif_payload(n);
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

    let kind = notif_kind_str(n);
    let title = match (series_title, chapter) {
        (Some(t), Some(c)) => format!("New chapter {c} of {t}"),
        (Some(t), None) => format!("Update for {t}"),
        _ if !kind.is_empty() => kind.replace('_', " "),
        _ => "Notification".to_owned(),
    };
    (title, series_id)
}
