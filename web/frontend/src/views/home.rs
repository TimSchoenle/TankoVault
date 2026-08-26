//! Home dashboard (`DESIGN_SPEC` §7.1) — the signed-in reader's landing screen. Greeting +
//! lifetime stat tiles, a continue-reading rail and a day-grouped "New in your watchlist" feed.
//! Signed out, the same route is [`GuestHome`] instead, and signed in with an empty watchlist it
//! is [`FirstRun`]: every section here is derived from a watchlist, so until one exists there is
//! nothing to show and the screen has to say what it is for instead. [`home_shape`] picks.
//!
//! Recommendations used to close this screen and now live at [`crate::Route::Recommendations`].
//! They were the fourth section down, so they got whatever room was left: a short shelf below
//! the fold with each suggestion's reason squeezed into one grey line. This screen is about what
//! the reader is *already* reading; what they might read next is a different question and now
//! has a page that can answer it properly.

use super::auth::AuthBrand;
use crate::api;
use crate::components::{async_list, Cover, SkeletonBlock, SkeletonRows};
use crate::hooks::{use_reload, Reload};
use crate::i18n::use_i18n;
use crate::icons::{Ic, Icon};
use crate::models::*;
use crate::state::capabilities::use_capabilities;
use crate::state::use_session;
use crate::util::{chapter_number, greeting_key, iso_date};
use crate::views::{DiscoverQuery, SearchQuery};
use crate::wire::types::{Feature, MeStats};
use crate::Route;
use dioxus::prelude::*;
use inkstone_ui::{button_class, Button, Pill, Size, Tone};
use progenitor_client::ResponseValue;
#[component]
pub(crate) fn Home() -> Element {
    let session = use_session();
    let i18n = use_i18n();
    let api = api::use_api();
    let caps = use_capabilities();
    let reload = use_reload();

    // Each resource builds its client from the live session token, so the boot-time silent
    // refresh (landing just after first paint) refetches everything automatically.
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
                .map(|mut items| {
                    // Fewest unread chapters first, so the closest-to-caught-up series lead
                    // the rail; ties keep the server's deterministic activity order.
                    items.sort_by(|a, b| a.unread.cmp(&b.unread));
                    items
                })
                .map_err(|e| api::friendly_error(i18n, e))
        }
    });

    if !session.is_authenticated() {
        // Until the boot-time silent refresh settles, "signed out" only means "we have not
        // looked yet" — so the landing waits rather than painting over the reader's own home.
        if !session.is_settled() {
            return rsx! {
                SkeletonBlock { height: 96 }
                SkeletonRows { count: 3 }
            };
        }
        return rsx! { GuestHome {} };
    }

    let name = session
        .username()
        .unwrap_or_else(|| i18n.t("common.readerFallback"));

    let shape = home_shape(stats.read_unchecked().as_ref());
    // A shimmer while the lookup is in flight, the number once it lands, an em dash only if it
    // failed — never a provisional zero. "New chapters" comes from stats' uncapped `unread`
    // total, not the feed length, which caps at 100 rows.
    let (new_chapters, reading, chapters_read) = match &*stats.read_unchecked() {
        Some(Ok(Some(stats))) => (
            TileValue::Known(stats.unread.to_string()),
            TileValue::Known(stats.reading.to_string()),
            TileValue::Known(stats.chapters_read.to_string()),
        ),
        None => (TileValue::Pending, TileValue::Pending, TileValue::Pending),
        Some(_) => (TileValue::Unknown, TileValue::Unknown, TileValue::Unknown),
    };

    rsx! {
        div { class: "ik-home-head",
            div {
                div { class: "ik-kicker", {i18n.t(greeting_key())} }
                h1 { class: "ik-page-title", style: "margin:6px 0 0;",
                    {i18n.args("home.welcome", &[("name", &name)])}
                }
            }
            // Three tiles that can only read zero are noise on the one screen that has to
            // explain itself, so a first run does without them entirely.
            if shape != HomeShape::FirstRun {
                div { class: "ik-stat-row",
                    StatTile { icon: Icon::Bolt, label: i18n.t("home.stat.newChapters"), value: new_chapters, tone: "acc" }
                    StatTile { icon: Icon::MenuBook, label: i18n.t("home.stat.reading"), value: reading, tone: "" }
                    StatTile { icon: Icon::Check, label: i18n.t("home.stat.chaptersRead"), value: chapters_read, tone: "jade" }
                }
            }
        }

        if shape == HomeShape::FirstRun {
            FirstRun {}
        } else {
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
                Link { to: Route::Notifications { query: crate::views::NotificationsQuery::default() }, class: "more", {i18n.t("common.seeAll")} }
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
                                for release in group_by_series(&entries) {
                                    FeedRow {
                                        key: "{feed_row_key(&release)}",
                                        release,
                                        reload,
                                    }
                                }
                            }
                        }
                    },
                )
            }

            // A pointer, not the shelf. The screen no longer carries recommendations, and a
            // reader who only ever lands here would otherwise never learn the surface exists.
            if caps.has_feature(Feature::CatalogueRecommendations) {
                Link { to: Route::Recommendations {}, class: "ik-cta-row",
                    span { class: "ik-flex", style: "gap:9px;align-items:center;",
                        Ic { icon: Icon::AutoAwesome, size: 18 }
                        span { style: "font-weight:600;", {i18n.t("home.recommendations.title")} }
                    }
                    span { class: "ik-muted", style: "font-size:13px;",
                        {i18n.t("home.recommendations.cta")}
                    }
                    Ic { icon: Icon::ArrowForward, size: 16 }
                }
            }
        }
    }
}

/// Which of the three screens Home is, decided by the stats resource alone.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum HomeShape {
    /// The stats lookup has not answered yet.
    Pending,
    /// It answered, and the reader tracks nothing.
    FirstRun,
    /// There is something to count.
    Tracking,
}

/// Decide Home's shape from the stats resource.
///
/// Every section of this screen derives from the watchlist, so `tracking == 0` is exactly the
/// condition under which all of them are guaranteed empty — and the only one that earns
/// [`FirstRun`]. The distinction a refactor must not lose is *not answered yet* versus
/// *answered, nothing tracked*: flashing "your watchlist is empty" at a reader who has a hundred
/// series is a worse screen than the one this replaces. A failed lookup is not evidence of an
/// empty watchlist either, so it keeps the normal body.
fn home_shape(stats: Option<&Result<Option<MeStats>, String>>) -> HomeShape {
    match stats {
        None => HomeShape::Pending,
        Some(Ok(Some(stats))) if stats.tracking == 0 => HomeShape::FirstRun,
        Some(_) => HomeShape::Tracking,
    }
}

/// What Home is on an account's first day: what a watchlist is for, and the two moves that fill
/// one.
///
/// Deliberately [`GuestHome`]'s shape — the two are the same screen at two moments, and the
/// reader who actually signed up must not be given less than the one who did not.
#[component]
fn FirstRun() -> Element {
    let i18n = use_i18n();
    let caps = use_capabilities();
    rsx! {
        div { class: "ik-auth", style: "margin:18px auto 0;",
            h1 { {i18n.t("home.firstRun.title")} }
            p { class: "ik-muted", {i18n.t("home.firstRun.body")} }
            if caps.has_feature(Feature::CatalogueBrowse) {
                Link {
                    to: Route::Discover { query: DiscoverQuery::default() },
                    class: button_class(Tone::Primary, Size::Md, true),
                    style: "margin-top:20px;",
                    {i18n.t("home.firstRun.discover")}
                }
            }
            if caps.has_feature(Feature::CatalogueSearch) {
                Link {
                    to: Route::Search { query: SearchQuery::default() },
                    class: button_class(Tone::Neutral, Size::Md, true),
                    style: "margin-top:10px;",
                    {i18n.t("home.firstRun.search")}
                }
            }
        }
    }
}

/// What `/` is for a reader with no session: the brand, what the product does, and the two
/// moves that actually work from here.
///
/// It replaces the bare "Home — sign in to see this" gate. Every section of the signed-in
/// screen is derived from a watchlist that does not exist yet, so there is nothing to withhold;
/// what was missing was a reason to sign in and a way into the part of the app that is public.
#[component]
fn GuestHome() -> Element {
    let i18n = use_i18n();
    let caps = use_capabilities();
    rsx! {
        div { class: "ik-auth",
            AuthBrand {}
            h1 { {i18n.t("home.guest.title")} }
            p { class: "ik-muted", {i18n.t("home.guest.subtitle")} }
            Link {
                to: Route::Login {},
                class: button_class(Tone::Primary, Size::Md, true),
                style: "margin-top:20px;",
                {i18n.t("common.signIn")}
            }
            if caps.has_feature(Feature::CatalogueBrowse) {
                Link {
                    to: Route::Discover { query: DiscoverQuery::default() },
                    class: button_class(Tone::Neutral, Size::Md, true),
                    style: "margin-top:10px;",
                    {i18n.t("home.guest.browse")}
                }
            }
        }
    }
}

/// A stat tile's value: the number, a shimmer while the lookup is in flight, or an em dash once
/// it has failed.
#[derive(Clone, PartialEq)]
enum TileValue {
    Pending,
    Known(String),
    Unknown,
}

/// One lifetime-stat tile in the header row.
#[component]
fn StatTile(icon: Icon, label: String, value: TileValue, tone: &'static str) -> Element {
    rsx! {
        div { class: "ik-stat",
            div { class: "lbl",
                Ic { icon, size: 13 }
                "{label}"
            }
            match value {
                // A shimmer rather than the em dash this used to show while loading: the dash is
                // set in the 26px display face, so three tiles resized around their real numbers
                // and re-flowed the header in the first second of every visit.
                TileValue::Pending => rsx! {
                    span { class: "val skel", "aria-hidden": "true" }
                },
                TileValue::Known(value) => rsx! {
                    div { class: "val {tone}", "{value}" }
                },
                // Resolved and unknown. A dash is honest here; a zero would be a claim.
                TileValue::Unknown => rsx! {
                    div { class: "val {tone}", "—" }
                },
            }
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
                        Pill {
                            tone: Tone::Accent,
                            style: "font-size:10px;",
                            {i18n.args("home.continue.new", &[("count", &item.unread.to_string())])}
                        }
                    }
                }
            }
        }
    }
}

/// Everything one series released on one day, as a single row.
///
/// The feed arrives one row per chapter. A series that dropped twelve overnight therefore took
/// twelve rows of the reader's home screen and pushed everything else below the fold — the same
/// shape the notification inbox already coalesces server-side, and it has to read the same way
/// here or the two surfaces disagree about what happened.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Release {
    pub(crate) series_id: SeriesId,
    pub(crate) series_title: String,
    pub(crate) provider_slug: String,
    /// Lowest chapter number in the group.
    pub(crate) first: f64,
    /// Highest — the one "mark read" catches the reader up to, and the one the link opens.
    pub(crate) last: f64,
    pub(crate) count: usize,
    /// The newest chapter's own title, kept only for a single-chapter row: naming one of twelve
    /// is worse than naming none. Same rule as the inbox's supporting line.
    pub(crate) chapter_title: Option<String>,
    /// Where the newest chapter is.
    pub(crate) url: String,
}

/// The rsx key for one feed row, which must be exactly what [`group_by_series`] folds on.
///
/// Anything less is a duplicate key among siblings, and dioxus's keyed diff answers that by
/// diffing one old node twice: the second pass reads a mount the first already took and panics
/// `invalid key` inside `dioxus-core`, aborting the whole app rather than returning an error.
/// See [`tests::two_sources_carrying_one_chapter_do_not_share_a_row_key`].
fn feed_row_key(release: &Release) -> String {
    format!("{}-{}", release.series_id, release.provider_slug)
}

/// Fold one day's entries into one row per series, newest chapter first.
///
/// The server's ordering inside a day is by discovery, which is not chapter order, so this takes
/// the extremes rather than the ends. Grouping is by series *and* provider: the same chapter
/// carried by two sources is two links, and merging them would silently pick one.
fn group_by_series(entries: &[FeedEntry]) -> Vec<Release> {
    let mut out: Vec<Release> = Vec::new();
    for entry in entries {
        let existing = out
            .iter_mut()
            .find(|r| r.series_id == entry.series_id && r.provider_slug == entry.provider_slug);
        match existing {
            Some(release) => {
                release.first = release.first.min(entry.chapter_number);
                release.count += 1;
                if entry.chapter_number > release.last {
                    release.last = entry.chapter_number;
                    release.url.clone_from(&entry.url);
                    release.chapter_title.clone_from(&entry.chapter_title);
                }
            }
            None => out.push(Release {
                series_id: entry.series_id,
                series_title: entry.series_title.clone(),
                provider_slug: entry.provider_slug.clone(),
                first: entry.chapter_number,
                last: entry.chapter_number,
                count: 1,
                chapter_title: entry.chapter_title.clone(),
                url: entry.url.clone(),
            }),
        }
    }
    out
}

/// One series' new chapters, with an open link and a mark-read action.
#[component]
fn FeedRow(release: Release, reload: Reload) -> Element {
    let api = api::use_api();
    let i18n = use_i18n();
    let series_id = release.series_id;
    let number = release.last;
    let grouped = release.count > 1;
    let marker = if grouped {
        i18n.args(
            "home.feed.range",
            &[
                ("first", &chapter_number(release.first)),
                ("last", &chapter_number(release.last)),
            ],
        )
    } else {
        format!("#{}", chapter_number(number))
    };
    // A grouped row says how many arrived; a single one names the chapter, exactly as the inbox
    // does. Naming one chapter out of twelve reads as though only that one landed.
    let label = if grouped {
        i18n.plural(
            "notifications.newChapters",
            i64::try_from(release.count).unwrap_or(i64::MAX),
            &[],
        )
    } else {
        release
            .chapter_title
            .clone()
            .filter(|title| !title.is_empty())
            .unwrap_or_else(|| {
                i18n.args(
                    "series.chapterNumbered",
                    &[("number", &chapter_number(number))],
                )
            })
    };

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
            span { class: "ik-mono", style: "color:var(--acc);min-width:56px;", "{marker}" }
            div { class: "grow",
                div { style: "font-weight:600;", "{release.series_title}" }
                div { class: "ik-muted", style: "font-size:13px;", "{label} · {release.provider_slug}" }
            }
            a { class: button_class(Tone::Neutral, Size::Md, false), href: "{release.url}", target: "_blank", rel: "noopener", {i18n.t("common.open")} }
            Button {
                tone: Tone::Primary,
                on_click: mark_read,
                {i18n.t("common.markRead")}
            }
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

#[cfg(test)]
#[expect(
    clippy::float_cmp,
    reason = "these compare against the exact chapter numbers the fixtures were built from, \
              not against a computed value"
)]
mod tests {
    use super::{feed_row_key, group_by_series, home_shape, FeedEntry, HomeShape, MeStats};

    fn stats(tracking: i64) -> MeStats {
        MeStats {
            chapters_read: 0,
            completed: 0,
            reading: tracking,
            tracking,
            unread: 0,
        }
    }

    /// The bug: every section of Home derives from the watchlist, so an account on its first day
    /// got three em dashes, two empty sections and no action anywhere — less than the signed-out
    /// screen offers. The first-run block that replaces them keys off *resolved and nothing
    /// tracked*; keying it off "no numbers to show" instead would flash "your watchlist is empty"
    /// at a reader who has a hundred series, every time the stats request is in flight.
    #[test]
    fn first_run_needs_stats_resolved_not_merely_absent() {
        assert_eq!(home_shape(None), HomeShape::Pending);
        assert_eq!(home_shape(Some(&Ok(Some(stats(0))))), HomeShape::FirstRun);
        assert_eq!(home_shape(Some(&Ok(Some(stats(7))))), HomeShape::Tracking);
        // A lookup that fell over is not evidence of an empty watchlist.
        assert_eq!(
            home_shape(Some(&Err("offline".to_owned()))),
            HomeShape::Tracking
        );
    }

    fn entry(series: &str, provider: &str, number: f64) -> FeedEntry {
        FeedEntry {
            series_id: series.parse().expect("a series id"),
            series_title: "Blame!".to_owned(),
            chapter_number: number,
            chapter_title: Some(format!("chapter {number}")),
            provider_slug: provider.to_owned(),
            url: format!("https://example.invalid/{provider}/{number}"),
            discovered_at: "2026-08-09T00:00:00Z".to_owned(),
        }
    }

    /// The bug: a series that released twelve chapters overnight took twelve rows of the home
    /// screen and pushed the rest of the day below the fold, while the notification inbox showed
    /// the same event as one coalesced line. One row per series, carrying the range and the
    /// count — and the *newest* chapter's link, whatever order discovery happened to record them
    /// in.
    #[test]
    fn a_days_chapters_for_one_series_become_one_row() {
        let id = "018f4c2a-0000-7000-8000-000000000001";
        let day = vec![
            entry(id, "kunmanga", 7.0),
            entry(id, "kunmanga", 12.0),
            entry(id, "kunmanga", 9.0),
        ];

        let grouped = group_by_series(&day);
        assert_eq!(grouped.len(), 1);
        assert_eq!(grouped[0].count, 3);
        assert_eq!(grouped[0].first, 7.0);
        assert_eq!(grouped[0].last, 12.0);
        assert!(grouped[0].url.ends_with("/12"), "the link opens the newest");
    }

    /// Two providers carrying the same chapter are two links, and picking one of them silently
    /// is the wrong kind of tidiness — the row's Open button would take the reader somewhere they
    /// did not choose.
    #[test]
    fn the_same_series_on_two_providers_stays_two_rows() {
        let id = "018f4c2a-0000-7000-8000-000000000001";
        let day = vec![entry(id, "kunmanga", 7.0), entry(id, "mangadex", 7.0)];
        assert_eq!(group_by_series(&day).len(), 2);
    }

    /// The bug that crashed the desktop app (7.3.0): the row key was
    /// `"{series_id}-{last}"`, which is the grouping identity *minus the provider* — so the two
    /// rows the test above insists on were siblings carrying one key. Dioxus's keyed diff maps
    /// both new nodes onto the same old node, the second pass reads a mount the first already
    /// took, and `dioxus-core` panics `invalid key`, aborting the process.
    ///
    /// The keys have to differ for the case that produces two rows, which is why this asserts
    /// against `group_by_series` rather than against two hand-built `Release`s.
    #[test]
    fn two_sources_carrying_one_chapter_do_not_share_a_row_key() {
        let id = "018f4c2a-0000-7000-8000-000000000001";
        let day = vec![entry(id, "kunmanga", 7.0), entry(id, "mangadex", 7.0)];

        let keys: Vec<String> = group_by_series(&day).iter().map(feed_row_key).collect();
        let unique: std::collections::HashSet<&String> = keys.iter().collect();
        assert_eq!(
            unique.len(),
            keys.len(),
            "sibling rows share a key: {keys:?}"
        );
    }

    /// A single chapter must not start reading as a range.
    #[test]
    fn one_chapter_is_not_a_group() {
        let day = vec![entry(
            "018f4c2a-0000-7000-8000-000000000001",
            "kunmanga",
            7.0,
        )];
        let grouped = group_by_series(&day);
        assert_eq!(grouped[0].count, 1);
        assert_eq!(grouped[0].first, grouped[0].last);
        assert!(grouped[0].chapter_title.is_some());
    }
}
