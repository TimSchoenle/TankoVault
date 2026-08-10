//! Notifications (§17.2.5) — chronological, unread emphasised, one-click mark-all-read, and
//! deep links into the series. Also keeps the rail's unread badge in sync.
//!
//! A row has to be readable *without* being opened. The server resolves each stored document into
//! the display fields ([`Notification`]), so this screen renders rather than parses: it used to
//! read a free-form payload looking for a `series_title` no writer ever set, which is why every
//! row said, literally, "new chapter".

use crate::api;
use crate::components::{
    async_view, AuthRequired, Cover, EmptyBox, Pagination, SkeletonRows, TabBar, TabKind,
    UnreadBadge,
};
use crate::hooks::use_reload;
use crate::i18n::{use_i18n, Translator};
use crate::icons::{Ic, Icon};
use crate::models::*;
use crate::state::use_session;
use crate::util::{chapter_number, rel_time};
use crate::Route;
use dioxus::prelude::*;
use inkstone_ui::{button_class, Button, Pill, Size, Tone};
use progenitor_client::ResponseValue;
/// Filter tabs (`DESIGN_SPEC` §7.5). Applied server-side: filtering the one loaded page is what
/// let "Unread" render empty while unread rows sat on page two.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Tab {
    All,
    Unread,
    Chapters,
}

impl TabKind for Tab {
    fn all() -> &'static [Self] {
        &[Self::All, Self::Unread, Self::Chapters]
    }

    /// The catalogue key of this tab's display name (see [`crate::i18n`]).
    fn label_key(self) -> &'static str {
        match self {
            Self::All => "notifications.tab.all",
            Self::Unread => "notifications.tab.unread",
            Self::Chapters => "notifications.tab.chapters",
        }
    }
}

impl Tab {
    fn unread_only(self) -> bool {
        matches!(self, Self::Unread)
    }

    /// The `kind` token this tab restricts to, if any.
    fn kind(self) -> Option<&'static str> {
        match self {
            Self::Chapters => Some("new_chapter"),
            Self::All | Self::Unread => None,
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
    /// Reads the server's token, and still classifies an unrecognised one carrying a chapter
    /// number as a chapter event: the notifier can ship a kind before a release of this bundle
    /// knows the name.
    fn of(notification: &Notification) -> Self {
        match notification.kind.as_str() {
            "new_chapter" => Self::NewChapter,
            "source_added" => Self::SourceAdded,
            "series_completed" => Self::Completed,
            "sync_conflict" => Self::Sync,
            _ if notification.last_number.is_some() => Self::NewChapter,
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
    let mut page = use_signal(|| 0usize);

    let notifications = use_resource(move || {
        reload.track();
        let client = api.client();
        let authed = session.is_authenticated();
        let current = *tab.read();
        let offset = i64::try_from(*page.read() * PAGE_SIZE).unwrap_or(0);
        async move {
            if !authed {
                return Ok(NotificationsView {
                    items: Vec::new(),
                    total: 0,
                    unread: 0,
                });
            }
            let mut request = client
                .notifications()
                .limit(i64::try_from(PAGE_SIZE).unwrap_or(50))
                .offset(offset)
                .unread(current.unread_only());
            if let Some(kind) = current.kind() {
                request = request.kind(kind);
            }
            request
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
            Button {
                disabled: unread == 0,
                on_click: mark_all,
                {i18n.t("notifications.markAllRead")}
            }
        }
        TabBar {
            selected: *tab.read(),
            on_select: move |next| {
                tab.set(next);
                // The filter changes the row set, so page 2 of the old one means nothing.
                page.set(0);
            },
        }
        {
            async_view(
                &notifications,
                reload,
                || rsx! { SkeletonRows { count: 5 } },
                |view| {
                    if view.items.is_empty() {
                        let key = if *tab.read() == Tab::All {
                            "notifications.empty"
                        } else {
                            "notifications.emptyFilter"
                        };
                        return rsx! {
                            EmptyBox { message: i18n.t(key) }
                        };
                    }
                    rsx! {
                        for notification in view.items.iter().cloned() {
                            NotifRow { key: "{notification.id}", notification }
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
    let api = api::use_api();
    let reload = use_reload();
    let mut read = use_signal(|| notification.read_at.is_some());

    let class = if *read.read() {
        "ik-row"
    } else {
        "ik-row unread"
    };
    let kind = Kind::of(&notification);
    let tile = format!(
        "background:color-mix(in srgb, {c} 16%, transparent);color:{c};",
        c = kind.color()
    );
    let title = Line::of(&notification).render(i18n);
    let sub = Sub::of(&notification).render(i18n);
    let when = rel_time(i18n, Some(notification.created_at.as_str()));
    let cover_title = notification.series_title.clone().unwrap_or_default();
    let chapter_url = notification.chapter_url.clone();

    let id = notification.id;
    let already_read = *read.read();
    let mark = move |_| {
        if already_read {
            return;
        }
        // Optimistic: the row loses its emphasis at once, and the badge catches up on the next
        // recount. A click that opens the series but leaves the row bold reads as a broken list.
        read.set(true);
        let client = api.client();
        spawn(async move {
            if client
                .mark_read()
                .body(MarkRead {
                    ids: vec![id],
                    all: Some(false),
                })
                .send()
                .await
                .is_ok()
            {
                reload.bump();
            }
        });
    };

    let inner = rsx! {
        div { class: "ik-notif-kind", style: "{tile}",
            Ic { icon: kind.icon(), size: 18 }
        }
        if notification.series_title.is_some() {
            div { class: "ik-notif-thumb",
                Cover { url: notification.cover_url.clone(), title: cover_title }
            }
        }
        div { class: "grow",
            div { class: "ik-notif-title", "{title}" }
            div { class: "ik-notif-sub",
                span { "{when}" }
                if let Some(sub) = sub {
                    span { "·" }
                    span { "{sub}" }
                }
            }
        }
        if !already_read {
            Pill {
                tone: Tone::Accent,
                {i18n.t("notifications.new")}
            }
        }
    };

    rsx! {
        div { class: "ik-notif-wrap",
            match notification.series_id {
                Some(id) => rsx! {
                    Link {
                        to: Route::Series { id: id.to_string() },
                        class: "{class} ik-notif",
                        onclick: mark,
                        {inner}
                    }
                },
                None => rsx! {
                    div { class: "{class} ik-notif", onclick: mark, {inner} }
                },
            }
            if let Some(url) = chapter_url {
                a {
                    class: format!("{} ik-notif-read", button_class(Tone::Neutral, Size::Md, false)),
                    href: "{url}",
                    target: "_blank",
                    rel: "noopener noreferrer",
                    onclick: mark,
                    {i18n.t("notifications.read")}
                }
            }
        }
    }
}

/// The summary an OS notification carries, worded exactly as the inbox row is.
///
/// The desktop build's toast and this screen must not drift into two readings of the same row,
/// which is what a second wording would become the first time the notifier ships a kind only one
/// of them knows about.
#[cfg(feature = "desktop")]
pub(crate) fn headline(notification: &Notification, i18n: Translator) -> String {
    let line = Line::of(notification).render(i18n);
    match Sub::of(notification).render(i18n) {
        Some(sub) => format!("{line}\n{sub}"),
        None => line,
    }
}

/// What a row's headline says, before it is worded.
///
/// Kept separate from the wording so the shape logic stays testable on the host target — a
/// message needs a Dioxus runtime.
#[derive(Debug, Clone, PartialEq)]
enum Line {
    /// A grouped row covers a range, and says so rather than showing the newest chapter and
    /// hiding the other eleven.
    Range {
        title: String,
        first: f64,
        last: f64,
    },
    /// One chapter of a series we know the title of.
    Single { title: String, number: f64 },
    /// Some other update to a series we know the title of.
    Update { title: String },
    /// No title resolved — shown as the server's kind token with its underscores spaced out,
    /// which is still readable. The catalogue cannot enumerate kinds the notifier has not
    /// shipped yet.
    Kind(String),
}

impl Line {
    fn of(notification: &Notification) -> Self {
        let Some(title) = notification.series_title.clone() else {
            return Self::Kind(notification.kind.replace('_', " "));
        };
        match (notification.first_number, notification.last_number) {
            (Some(first), Some(last)) if first < last => Self::Range { title, first, last },
            (_, Some(number)) | (Some(number), None) => Self::Single { title, number },
            (None, None) => Self::Update { title },
        }
    }

    fn render(&self, i18n: Translator) -> String {
        match self {
            Self::Range { title, first, last } => i18n.args(
                "notifications.line.chapterRange",
                &[
                    ("title", title),
                    ("first", &chapter_number(*first)),
                    ("last", &chapter_number(*last)),
                ],
            ),
            Self::Single { title, number } => i18n.args(
                "notifications.line.newChapter",
                &[("title", title), ("number", &chapter_number(*number))],
            ),
            Self::Update { title } => i18n.args("notifications.line.update", &[("title", title)]),
            Self::Kind(kind) => kind.clone(),
        }
    }
}

/// The supporting line's parts, before wording. Separate from [`Line`] for the same reason.
#[derive(Debug, Clone, PartialEq, Default)]
struct Sub {
    /// Set when the row groups more than one chapter, in which case the individual chapter title
    /// is deliberately dropped: naming one of twelve is worse than naming none.
    count: Option<i64>,
    chapter_title: Option<String>,
    provider: Option<String>,
}

impl Sub {
    fn of(notification: &Notification) -> Self {
        let grouped = notification.chapter_count > 1;
        Self {
            count: grouped.then_some(notification.chapter_count),
            chapter_title: (!grouped)
                .then(|| notification.chapter_title.clone())
                .flatten(),
            provider: notification.provider_slug.clone(),
        }
    }

    fn render(&self, i18n: Translator) -> Option<String> {
        let mut parts: Vec<String> = Vec::new();
        if let Some(count) = self.count {
            parts.push(i18n.plural("notifications.newChapters", count, &[]));
        } else if let Some(title) = self.chapter_title.as_deref() {
            parts.push(title.to_owned());
        }
        if let Some(provider) = self.provider.as_deref() {
            parts.push(provider.to_owned());
        }
        (!parts.is_empty()).then(|| parts.join(" · "))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(kind: &str) -> Notification {
        Notification {
            id: uuid::Uuid::nil(),
            kind: kind.to_owned(),
            created_at: "2026-08-07T00:00:00Z".to_owned(),
            read_at: None,
            series_id: None,
            series_title: None,
            cover_url: None,
            provider_slug: None,
            first_number: None,
            last_number: None,
            chapter_count: 1,
            chapter_title: None,
            chapter_url: None,
            payload: serde_json::Value::Null,
        }
    }

    /// A kind this bundle predates still has to render as a chapter event when it carries one.
    #[test]
    fn classifies_an_unnamed_kind_with_a_chapter_number_as_a_chapter_event() {
        let mut n = row("brand_new_kind");
        n.last_number = Some(12.0);
        assert!(matches!(Kind::of(&n), Kind::NewChapter));
        assert!(matches!(Kind::of(&row("brand_new_kind")), Kind::Unknown));
    }

    /// Every row used to read, literally, "new chapter": the view demanded a `series_title` the
    /// notifier never wrote, so the titled arms were unreachable and every row fell through to
    /// the kind token. With a title, the headline must name the series.
    #[test]
    fn a_titleless_row_falls_back_to_the_kind_token() {
        assert_eq!(
            Line::of(&row("some_event")),
            Line::Kind("some event".to_owned())
        );
    }

    #[test]
    fn a_named_series_reaches_the_titled_arms() {
        let mut n = row("new_chapter");
        n.series_title = Some("Blame!".to_owned());
        n.last_number = Some(7.0);
        assert_eq!(
            Line::of(&n),
            Line::Single {
                title: "Blame!".to_owned(),
                number: 7.0,
            }
        );

        n.chapter_title = Some("The Silicon Life".to_owned());
        n.provider_slug = Some("mangadex".to_owned());
        assert_eq!(
            Sub::of(&n),
            Sub {
                count: None,
                chapter_title: Some("The Silicon Life".to_owned()),
                provider: Some("mangadex".to_owned()),
            }
        );
    }

    /// A coalesced row says the range and the count, and drops the one chapter title it happens
    /// to hold — otherwise twelve chapters read as though only the newest arrived.
    #[test]
    fn a_grouped_row_says_the_range_and_the_count() {
        let mut n = row("new_chapter");
        n.series_title = Some("Blame!".to_owned());
        n.first_number = Some(7.0);
        n.last_number = Some(18.0);
        n.chapter_count = 12;
        n.chapter_title = Some("The Silicon Life".to_owned());
        assert_eq!(
            Line::of(&n),
            Line::Range {
                title: "Blame!".to_owned(),
                first: 7.0,
                last: 18.0,
            }
        );
        assert_eq!(
            Sub::of(&n),
            Sub {
                count: Some(12),
                chapter_title: None,
                provider: None,
            }
        );
    }

    #[test]
    fn a_series_with_no_chapter_numbers_still_names_itself() {
        let mut n = row("series_completed");
        n.series_title = Some("Blame!".to_owned());
        assert_eq!(
            Line::of(&n),
            Line::Update {
                title: "Blame!".to_owned(),
            }
        );
        assert_eq!(Sub::of(&n), Sub::default());
    }
}
