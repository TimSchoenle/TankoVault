//! Notifications (§17.2.5) — chronological, unread emphasised, one-click mark-all-read, and
//! deep links into the series. Also keeps the rail's unread badge in sync.
//!
//! The list is free-form JSON on the server (the notifier writes an open `payload` per kind),
//! so this screen reads it defensively: unknown kinds still render, with the kind token as
//! their line, rather than being dropped or crashing the list.

use crate::api;
use crate::components::{EmptyBox, async_list, AuthRequired, SkeletonRows, UnreadBadge};
use crate::hooks::use_reload;
use crate::i18n::{use_i18n, Translator};
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

    /// The catalogue key of this tab's display name (see [`crate::i18n`]).
    fn label_key(self) -> &'static str {
        match self {
            Self::All => "notifications.tab.all",
            Self::Unread => "notifications.tab.unread",
            Self::Chapters => "notifications.tab.chapters",
            Self::Sync => "notifications.tab.sync",
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
    let i18n = use_i18n();
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
                .map_err(|e| api::friendly_error(i18n, e))
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
        return rsx! { AuthRequired { title: i18n.t("nav.notifications") } };
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
                h1 { class: "ik-page-title", style: "margin-bottom:2px;", {i18n.t("nav.notifications")} }
                div { class: "ik-mono ik-muted", style: "font-size:12px;",
                    {i18n.args("notifications.summary", &[("count", &unread.to_string())])}
                }
            }
            button { class: "ik-btn", disabled: unread == 0, onclick: mark_all,
                {i18n.t("notifications.markAllRead")}
            }
        }
        div { class: "ik-tabs",
            for t in Tab::ALL {
                button {
                    key: "{t.label_key()}",
                    class: if current == t { "ik-tab active" } else { "ik-tab" },
                    onclick: move |_| tab.set(t),
                    {i18n.t(t.label_key())}
                }
            }
        }
        {
            async_list(
                &notifications,
                reload,
                || rsx! { SkeletonRows { count: 5 } },
                &i18n.t("notifications.empty"),
                |items| {
                    let filtered: Vec<&Notification> =
                        items.iter().filter(|n| current.matches(n)).collect();
                    if filtered.is_empty() {
                        return rsx! {
                            EmptyBox { message: i18n.t("notifications.emptyFilter") }
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
    let i18n = use_i18n();
    let unread = read_at(&notification).is_none();
    let class = if unread { "ik-row unread" } else { "ik-row" };
    let (line, series_id) = describe(&notification);
    let title = line.render(i18n);
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
            span { class: "ik-pill vermilion", {i18n.t("notifications.new")} }
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

/// What a row says, before it is worded.
///
/// Kept separate from the wording so the payload-shape logic stays unit-testable on the host
/// target (resolving a message needs a Dioxus runtime) and so the phrasing is a catalogue
/// concern rather than something baked into the parser.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Line {
    /// A new chapter of a series we know the title of.
    NewChapter { title: String, number: String },
    /// Some other update to a series we know the title of.
    Update { title: String },
    /// An unrecognised payload, but a named kind — shown with its underscores spaced out,
    /// which is still readable, rather than replaced by a placeholder that says nothing.
    Kind(String),
    /// Nothing usable in the payload at all.
    Generic,
}

impl Line {
    fn render(&self, i18n: Translator) -> String {
        match self {
            Self::NewChapter { title, number } => i18n.args(
                "notifications.line.newChapter",
                &[("number", number), ("title", title)],
            ),
            Self::Update { title } => i18n.args("notifications.line.update", &[("title", title)]),
            // A server-defined token, deliberately passed through untranslated: the catalogue
            // cannot enumerate kinds the notifier has not shipped yet.
            Self::Kind(kind) => kind.clone(),
            Self::Generic => i18n.t("notifications.line.generic"),
        }
    }
}

/// Derive a row's line and an optional deep-link target from a notification payload.
///
/// The notifier writes `{ series_id, series_title, chapter_number, .. }` for chapter events;
/// an unrecognised shape degrades through [`Line::Kind`] to [`Line::Generic`].
fn describe(notification: &Notification) -> (Line, Option<String>) {
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
    let line = match (series_title, chapter) {
        (Some(title), Some(number)) => Line::NewChapter {
            title,
            number: crate::util::chapter_number(number),
        },
        (Some(title), None) => Line::Update { title },
        _ if !kind.is_empty() => Line::Kind(kind.replace('_', " ")),
        _ => Line::Generic,
    };
    (line, series_id)
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
            (
                Line::NewChapter {
                    title: "Blame!".to_owned(),
                    number: "7".to_owned(),
                },
                Some("abc".to_owned()),
            )
        );
    }

    #[test]
    fn falls_back_to_the_kind_token_for_an_unknown_shape() {
        let n = json!({ "kind": "some_event" });
        assert_eq!(describe(&n), (Line::Kind("some event".to_owned()), None));
    }

    #[test]
    fn falls_back_to_a_generic_line_when_there_is_no_kind_either() {
        assert_eq!(describe(&json!({})), (Line::Generic, None));
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
