//! Notifications (§17.2.5) — chronological, unread emphasised, one-click mark-all-read, and
//! deep links into the series. Also keeps the rail's unread badge in sync.
//!
//! The list is free-form JSON on the server (the notifier writes an open `payload` per kind),
//! so this screen reads it defensively: unknown kinds still render, with the kind token as
//! their line, rather than being dropped or crashing the list.

use crate::api;
use crate::components::{
    async_view, AuthRequired, EmptyBox, Pagination, SkeletonRows, TabBar, TabKind, UnreadBadge,
};
use crate::hooks::use_reload;
use crate::i18n::{use_i18n, Translator};
use crate::icons::{Ic, Icon};
use crate::models::*;
use crate::state::use_session;
use crate::util::iso_date;
use crate::Route;
use dioxus::prelude::*;
use progenitor_client::ResponseValue;
use uuid::Uuid;

/// Filter tabs (`DESIGN_SPEC` §7.5), applied client-side to the loaded list.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Tab {
    All,
    Unread,
    Chapters,
    Sync,
}

impl TabKind for Tab {
    fn all() -> &'static [Self] {
        &[Self::All, Self::Unread, Self::Chapters, Self::Sync]
    }

    /// The catalogue key of this tab's display name (see [`crate::i18n`]).
    fn label_key(self) -> &'static str {
        match self {
            Self::All => "notifications.tab.all",
            Self::Unread => "notifications.tab.unread",
            Self::Chapters => "notifications.tab.chapters",
            Self::Sync => "notifications.tab.sync",
        }
    }
}

impl Tab {
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
            // An unknown kind with a chapter number is still a chapter event under a new name.
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

/// Rows per request. The inbox is paged rather than truncated: it used to arrive as one
/// hard-capped batch, which is why a busy account's list and bell both sat at exactly 100.
const PAGE_SIZE: usize = 50;

#[component]
pub(crate) fn Notifications() -> Element {
    let session = use_session();
    let i18n = use_i18n();
    let api = api::use_api();
    let badge = use_context::<UnreadBadge>();
    let reload = use_reload();
    let mut tab = use_signal(|| Tab::All);
    let page = use_signal(|| 0usize);

    let notifications = use_resource(move || {
        reload.track();
        let client = api.client();
        let authed = session.is_authenticated();
        let offset = i64::try_from(*page.read() * PAGE_SIZE).unwrap_or(0);
        async move {
            if !authed {
                return Ok(NotificationsView {
                    items: Vec::new(),
                    total: 0,
                    unread: 0,
                });
            }
            client
                .notifications()
                .limit(i64::try_from(PAGE_SIZE).unwrap_or(50))
                .offset(offset)
                .send()
                .await
                .map(ResponseValue::into_inner)
                .map_err(|e| api::friendly_error(i18n, e))
        }
    });

    // The SSE push is best-effort; this navigation-time recount is what keeps the rail badge
    // honest. It takes the server's inbox-wide count, not this page's — counting the page is
    // what pinned the badge at the page size.
    use_effect(move || {
        let mut count = badge.0;
        if let Some(Ok(view)) = &*notifications.read_unchecked() {
            count.set(view.unread);
        }
    });

    if !session.is_authenticated() {
        return rsx! { AuthRequired { title: i18n.t("nav.notifications") } };
    }

    let mark_all = move |_| {
        let client = api.client();
        spawn(async move {
            // `all`, not the ids on screen: the reader asked for the inbox, and only one page
            // of it is loaded.
            if client
                .mark_read()
                .body(MarkRead {
                    ids: Vec::new(),
                    all: Some(true),
                })
                .send()
                .await
                .is_ok()
            {
                reload.bump();
            }
        });
    };

    let current = *tab.read();
    let (unread, total) = match &*notifications.read_unchecked() {
        Some(Ok(view)) => (view.unread, view.total),
        _ => (0, 0),
    };
    let pages = usize::try_from(total)
        .unwrap_or(0)
        .div_ceil(PAGE_SIZE)
        .max(1);

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
        TabBar { selected: *tab.read(), on_select: move |next| tab.set(next) }
        {
            async_view(
                &notifications,
                reload,
                || rsx! { SkeletonRows { count: 5 } },
                |view| {
                    if view.items.is_empty() {
                        return rsx! {
                            EmptyBox { message: i18n.t("notifications.empty") }
                        };
                    }
                    let filtered: Vec<&Notification> =
                        view.items.iter().filter(|n| current.matches(n)).collect();
                    rsx! {
                        if filtered.is_empty() {
                            EmptyBox { message: i18n.t("notifications.emptyFilter") }
                        }
                        for (index , notification) in filtered.into_iter().enumerate() {
                            NotifRow {
                                key: "{id_of(notification).map_or_else(|| index.to_string(), |id| id.to_string())}",
                                notification: notification.clone(),
                            }
                        }
                        if pages > 1 {
                            Pagination { page, pages, has_next: *page.read() + 1 < pages }
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
/// Kept separate from the wording so the payload-shape logic stays testable on the host target
/// (a message needs a Dioxus runtime) rather than baked into the parser.
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

/// The one-line summary an OS notification carries, worded exactly as the inbox row is.
///
/// The desktop build's toast and this screen must not drift into two readings of the same
/// free-form payload, which is what a second parser would become the first time the notifier
/// ships a kind only one of them knows about.
#[cfg(feature = "desktop")]
pub(crate) fn headline(notification: &Notification, i18n: Translator) -> String {
    describe(notification).0.render(i18n)
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
}
