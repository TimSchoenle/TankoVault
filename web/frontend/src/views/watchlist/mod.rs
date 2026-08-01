//! Watchlist — one filtered, sorted, virtualized list (design turn 4, option `4a`; the cover
//! grid is `4b` and lives in [`grid`]).
//!
//! # What replaced what
//!
//! This screen used to be a five-column drag-and-drop kanban. At the size a real account
//! reaches — 598 tracked titles, 564 of them `Reading` — that layout has no chance: a column is
//! a 500-card scroll, drag cannot retarget forty titles, and every card is a fixed block so
//! nothing is scannable. **Status is now a filter, not a layout.** One list, ordered by newest
//! release and banded Today / This week / Earlier, puts the triage queue at the top of the page
//! where the reader's actual question ("what do I read next?") is answered.
//!
//! Drag-and-drop is deleted outright rather than kept alongside. It was never the accessible
//! path — the per-card `<select>` was — and keeping a second, worse mover would have meant two
//! code paths for every status change. `J`/`K`/`X`/`↵` replace it, and the row menu is now the
//! keyboard-operable mover the quality floor (§11) requires.
//!
//! # Where the work happens
//!
//! Filtering, sorting, grouping and paging are **server-side** (`GET /v1/me/watchlist`, §4.2).
//! The client never holds 598 rows in order to sort them; that is the entire point of the
//! redesign, and it is why this module has no comparator in it. The tab counts and the group
//! aggregates come from the same response, so a header cannot disagree with the rows under it.
//!
//! # Layout of this module
//!
//! | module | owns |
//! |---|---|
//! | [`query`] | the URL-mirrored view state and its encoding |
//! | [`toolbar`] | the status tabs and the filter/sort toolbar |
//! | [`row`] | the 54px row, its overflow menu and the group header |
//! | [`bulk`] | the multi-select bulk bar |
//! | [`grid`] | the cover-grid alternate (`4b`) |

mod bulk;
mod grid;
mod query;
mod row;
mod toolbar;

use crate::api;
use crate::components::{AuthRequired, ErrorBox, OutcomeLine, SkeletonRows};
use crate::hooks::{use_busy, use_outcome, use_reload};
use crate::i18n::{use_i18n, Translator};
use crate::icons::{Ic, Icon};
use crate::models::*;
use crate::state::use_session;
use crate::util::{rel_time, thousands};
use crate::Route;
use bulk::BulkBar;
use dioxus::prelude::*;
use grid::CoverGrid;
use progenitor_client::ResponseValue;
pub(crate) use query::WatchlistQuery;
use query::{Order, Released, Sort, View};
use row::{GroupHeader, RowCtx, WatchRow};
use std::collections::HashSet;
use toolbar::{FilterBar, StatusTabs};

/// Rows fetched per page. The list pages on a scroll sentinel, so this is the size of one
/// bite, not of the list.
const PAGE_SIZE: i64 = 60;

/// The largest selection a bulk call accepts, mirroring the API's own cap. Enforced here so
/// `Select all` on a 564-row tab does not build a request the server will refuse.
const BULK_LIMIT: usize = 200;

/// The provider the "Sync now" button drives. Per-provider control lives on Account → Sync.
const SYNC_PROVIDER: &str = "anilist";

/// Which release-recency band a row renders under.
///
/// **Rolling windows, matching the server's** (`ReleaseBucket` in
/// `crates/db/src/repo/tracking/watchlist.rs`): "Today" is the last 24 hours, not the current
/// calendar day. The server has no idea what timezone this browser is in, so a calendar-day
/// bucket would be wrong by up to a day for anyone off UTC — and the aggregates in the group
/// headers come from the server, so the two rules have to be the same one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(super) enum Bucket {
    Today,
    ThisWeek,
    Earlier,
}

impl Bucket {
    /// The key the server's group aggregates are labelled with.
    fn key(self) -> &'static str {
        match self {
            Self::Today => "today",
            Self::ThisWeek => "week",
            Self::Earlier => "earlier",
        }
    }

    /// The catalogue key of this band's heading (see [`crate::i18n`]).
    fn label_key(self) -> &'static str {
        match self {
            Self::Today => "watchlist.group.today",
            Self::ThisWeek => "watchlist.group.thisWeek",
            Self::Earlier => "watchlist.group.earlier",
        }
    }
}

/// Band a release timestamp, given its age in milliseconds.
///
/// Split from the clock so the boundaries stay unit-testable on the host target, the way
/// [`crate::util`]'s freshness rule is. A *negative* age — this browser's clock behind the
/// server's — bands as `Today` rather than falling through to `Earlier`: it is a chapter that
/// just landed, and the alternative is a fresh release hiding at the bottom of the page.
fn bucket_of_age(age_ms: f64) -> Bucket {
    const DAY: f64 = 24.0 * 3_600_000.0;
    if age_ms < DAY {
        Bucket::Today
    } else if age_ms < 7.0 * DAY {
        Bucket::ThisWeek
    } else {
        Bucket::Earlier
    }
}

/// Band a row by its newest chapter. A row with no chapters has no release instant and bands as
/// `Earlier` — the same arm the server's `ELSE` uses, so the counts still add up.
fn bucket_of(ts: Option<&str>) -> Bucket {
    let Some(s) = ts.filter(|s| !s.is_empty()) else {
        return Bucket::Earlier;
    };
    let parsed = js_sys::Date::parse(s);
    if parsed.is_nan() {
        return Bucket::Earlier;
    }
    bucket_of_age(js_sys::Date::now() - parsed)
}

/// What the list renders, in order: band headings interleaved with the rows under them.
///
/// Precomputed rather than decided inside the `for` loop, because a heading has to be emitted
/// *between* two rows and `rsx!` has nowhere to keep the "what band was the last row in?" state
/// that would need.
enum Entry {
    Band(Bucket),
    Row(usize),
}

/// Interleave band headings into the row order.
///
/// Relies on the rows arriving grouped, which they do: the server orders by release instant,
/// so rows of one band are contiguous. Under any other sort the bands describe nothing and
/// [`Sort::groups_by_release`] switches them off entirely.
fn layout(items: &[WatchlistItem], grouped: bool) -> Vec<Entry> {
    let mut out = Vec::with_capacity(items.len() + 3);
    let mut current: Option<Bucket> = None;
    for (index, item) in items.iter().enumerate() {
        if grouped {
            let band = bucket_of(item.latest_chapter_at.as_deref());
            if current != Some(band) {
                out.push(Entry::Band(band));
                current = Some(band);
            }
        }
        out.push(Entry::Row(index));
    }
    out
}

/// One fetch's worth of state: the view the URL describes, plus how many pages deep the reader
/// has scrolled.
///
/// The two live in **one** signal rather than two so they can only ever change together. Kept
/// apart, a filter change and the page-depth reset that must accompany it are two separate
/// invalidations, and the resource fires once against the stale depth in between — refetching
/// six hundred rows for a list the reader has already navigated away from.
#[derive(Clone, PartialEq)]
struct Request {
    query: WatchlistQuery,
    pages: i64,
}

impl Request {
    /// Rows to ask for. Every page the reader has reached is refetched, not just the newest
    /// one: the response is then always the complete prefix of the list, which removes the
    /// entire class of accumulate-and-deduplicate bugs that an append-only cache has (a filter
    /// change racing an in-flight page, a `reload` at depth appending a second copy of
    /// everything). The server walks the same indexed set either way.
    fn limit(&self) -> i64 {
        PAGE_SIZE.saturating_mul(self.pages)
    }
}

/// Everything one response tells the chrome around the list. Mirrored out of the resource into
/// signals so an optimistic mutation can adjust it without waiting for a refetch.
#[derive(Clone, PartialEq, Default)]
struct Board {
    items: Vec<WatchlistItem>,
    counts: Option<WatchlistCounts>,
    groups: Vec<WatchlistGroup>,
    total: i64,
}

#[component]
pub(crate) fn Watchlist(query: WatchlistQuery) -> Element {
    let session = use_session();
    let i18n = use_i18n();
    let api = api::use_api();
    let reload = use_reload();
    let syncing = use_busy();
    let mut outcome = use_outcome();
    let nav = navigator();
    // Provided once instead of threaded through the row, its menu and the bulk bar as six
    // separate props. Every handle in it is `Copy`, so this is a context lookup, not a clone.
    let ctx = use_context_provider(|| RowCtx {
        api,
        i18n,
        reload,
        outcome,
    });

    let mut request = use_signal(|| Request {
        query: query.clone(),
        pages: 1,
    });
    let mut board = use_signal(Board::default);
    let mut settled = use_signal(|| false);
    let mut selected = use_signal(HashSet::<SeriesId>::new);
    let mut focus = use_signal(|| 0usize);
    let menu_for = use_signal(|| Option::<SeriesId>::None);

    // The route is the source of truth for the view state; this is what makes a route change
    // (a tab click, the back button, a pasted link) restart paging from the top. Guarded on
    // inequality because `Signal::set` invalidates unconditionally — without the guard the
    // first render would schedule a second, identical fetch.
    use_effect(use_reactive!(|query| {
        if request.peek().query != query {
            request.set(Request { query, pages: 1 });
            selected.write().clear();
            focus.set(0);
        }
    }));

    let page = use_resource(move || {
        reload.track();
        let req = request.read().clone();
        let client = api.client();
        let authed = session.is_authenticated();
        async move {
            if !authed {
                return Ok(WatchlistView {
                    items: Vec::new(),
                    counts: WatchlistCounts {
                        reading: 0,
                        planned: 0,
                        paused: 0,
                        completed: 0,
                        dropped: 0,
                        all: 0,
                        source_issues: 0,
                    },
                    groups: Vec::new(),
                    total: 0,
                });
            }
            let mut builder = client
                .watchlist()
                .sort(req.query.sort.token())
                .order(req.query.effective_order().token())
                .unread_only(req.query.unread_only)
                .source_issues(req.query.source_issues)
                .limit(req.limit())
                .offset(0);
            if let Some(status) = req.query.status_token() {
                builder = builder.status(status);
            }
            if !req.query.q.is_empty() {
                builder = builder.q(req.query.q.clone());
            }
            if req.query.released != Released::Any {
                builder = builder.released_since(req.query.released.token());
            }
            builder
                .send()
                .await
                .map(ResponseValue::into_inner)
                .map_err(|e| api::friendly_error(i18n, e))
        }
    });

    // Mirror the response into local state. The list renders from `board`, not from the
    // resource, so an optimistic mutation has somewhere to write — and so a refetch does not
    // blank the list while it is in flight.
    use_effect(move || {
        if let Some(Ok(view)) = &*page.read() {
            board.set(Board {
                items: view.items.clone(),
                counts: Some(view.counts.clone()),
                groups: view.groups.clone(),
                total: view.total,
            });
            settled.set(true);
        }
    });

    // Best-effort: the sync chip is a nicety, and a sync service that is down must not take the
    // watchlist with it. Mirrors how Discover treats its facet lists.
    let sync_status = use_resource(move || {
        reload.track();
        let client = api.client();
        let authed = session.is_authenticated();
        async move {
            if !authed {
                return None;
            }
            client
                .sync_status()
                .provider(SYNC_PROVIDER)
                .send()
                .await
                .ok()
                .map(ResponseValue::into_inner)
        }
    });

    if !session.is_authenticated() {
        return rsx! { AuthRequired { title: i18n.t("nav.watchlist") } };
    }

    let go = move |next: WatchlistQuery| {
        nav.push(Route::Watchlist { query: next });
    };
    // The filter box commits on a 200ms debounce, so it would otherwise push one history entry
    // per keystroke and make the back button useless for the whole screen.
    let go_replace = move |next: WatchlistQuery| {
        nav.replace(Route::Watchlist { query: next });
    };

    let sync_now = move |_| {
        if !syncing.claim() {
            return;
        }
        outcome.set(None);
        let client = api.client();
        spawn(async move {
            let opts = SyncOpts {
                policy: Some(ConflictPolicy::NewestWins.into()),
            };
            // Pull first, then push: importing the remote list before reflecting local state
            // means a title added on the other side is not immediately overwritten.
            let result = match client
                .sync_pull()
                .provider(SYNC_PROVIDER)
                .body(SyncPullBody::Variant1(opts.clone()))
                .send()
                .await
            {
                Ok(_) => client
                    .sync_push()
                    .provider(SYNC_PROVIDER)
                    .body(SyncPushBody::Variant1(opts))
                    .send()
                    .await
                    .map(|_| i18n.t("watchlist.synced"))
                    .map_err(|e| api::friendly_error(i18n, e)),
                Err(e) => Err(api::friendly_error(i18n, e)),
            };
            if result.is_ok() {
                reload.bump();
            }
            outcome.set(Some(result));
            syncing.release();
        });
    };

    let snapshot = board.read().clone();
    let counts = snapshot.counts.clone();
    let loaded = snapshot.items.len();
    let has_more = i64::try_from(loaded).unwrap_or(i64::MAX) < snapshot.total;
    let grouped = query.sort.groups_by_release();
    let entries = layout(&snapshot.items, grouped);

    // Both halves describe *this* view, under the identical predicate set: `total` is the rows
    // the filter matches and the band aggregates sum the unread across exactly those rows.
    // Mixing in `counts.all` here would have read "598 titles · 1,684 chapters" over a list of
    // 40 — the tab counts deliberately drop the status filter, and the group aggregates
    // deliberately keep it, so the two are not interchangeable.
    let headline = if counts.is_some() {
        i18n.args(
            "watchlist.headline",
            &[
                ("titles", &thousands(snapshot.total)),
                ("chapters", &thousands(unread_total(&snapshot))),
            ],
        )
    } else {
        String::new()
    };

    rsx! {
        div { class: "ik-page-head",
            div {
                h1 { class: "ik-page-title", style: "margin-bottom:2px;", {i18n.t("nav.watchlist")} }
                div { class: "ik-muted", style: "font-size:13px;", "{headline}" }
            }
            div { class: "ik-flex", style: "gap:10px;align-items:center;",
                {sync_chip(i18n, &sync_status)}
                button { class: "ik-btn", disabled: syncing.is_busy(), onclick: sync_now,
                    Ic { icon: Icon::CloudSync, size: 16 }
                    if syncing.is_busy() {
                        {i18n.t("watchlist.syncing")}
                    } else {
                        {i18n.t("watchlist.sync")}
                    }
                }
            }
        }
        OutcomeLine { outcome: outcome.read().clone() }

        StatusTabs { query: query.clone(), counts: counts.clone(), on_change: go }
        FilterBar {
            query: query.clone(),
            visible: loaded,
            source_issues: counts.as_ref().map_or(0, |c| c.source_issues),
            on_change: go,
            on_change_quiet: go_replace,
        }

        if let Some(Err(message)) = &*page.read() {
            ErrorBox { message: message.clone(), on_retry: move |()| reload.bump() }
        } else if !*settled.read() {
            SkeletonRows { count: 8 }
        } else if snapshot.items.is_empty() {
            {empty_state(i18n, &query, go)}
        } else if query.view == View::Grid {
            CoverGrid { items: snapshot.items.clone(), selected }
        } else {
            div { class: "ik-wl-head", role: "row",
                span {}
                span { {i18n.t("watchlist.col.title")} }
                span { {i18n.t("watchlist.col.progress")} }
                span { style: "text-align:right;", {i18n.t("watchlist.col.unread")} }
                {sort_header(i18n, &query, go)}
                span {}
            }
            div {
                class: "ik-wl-list",
                role: "grid",
                tabindex: "0",
                "aria-label": i18n.t("nav.watchlist"),
                // The container holds focus and `J`/`K` move a cursor within it, so the row
                // under that cursor has to be named for a screen reader — otherwise the whole
                // keyboard contract is silent to anyone not looking at the highlight.
                "aria-activedescendant": snapshot
                    .items
                    .get(*focus.read())
                    .map_or_else(String::new, |item| format!("wl-row-{}", item.series_id)),
                onkeydown: move |event| on_key(&event, board, selected, focus, menu_for, ctx),
                for entry in entries {
                    match entry {
                        Entry::Band(band) => {
                            let stats = snapshot.groups.iter().find(|g| g.key == band.key());
                            rsx! {
                                GroupHeader {
                                    key: "band-{band.key()}",
                                    band,
                                    title_count: stats.map_or(0, |g| g.title_count),
                                    chapter_count: stats.map_or(0, |g| g.chapter_count),
                                    ids: band_ids(&snapshot.items, band),
                                    complete: stats.is_some_and(|g| {
                                        band_ids(&snapshot.items, band).len() as i64 == g.title_count
                                    }),
                                }
                            }
                        }
                        Entry::Row(index) => {
                            let item = snapshot.items[index].clone();
                            rsx! {
                                WatchRow {
                                    key: "{item.series_id}",
                                    item,
                                    index,
                                    focused: *focus.read() == index,
                                    selected,
                                    focus,
                                    menu_for,
                                    board,
                                }
                            }
                        }
                    }
                }
            }
        }

        if *settled.read() && !snapshot.items.is_empty() {
            // The sentinel: one empty div below the last row that asks for the next page when
            // it scrolls into view. `onvisible` is an `IntersectionObserver` under the hood, so
            // this costs no scroll handler and no polling.
            if has_more {
                div {
                    class: "ik-wl-sentinel",
                    onvisible: move |event| {
                        if event.data.is_intersecting().unwrap_or(false) {
                            request.write().pages += 1;
                        }
                    },
                }
            }
            div { class: "ik-wl-foot",
                span {
                    {
                        i18n.args(
                            "watchlist.rowsOf",
                            &[
                                ("shown", &thousands(i64::try_from(loaded).unwrap_or(0))),
                                ("total", &thousands(snapshot.total)),
                            ],
                        )
                    }
                }
                // The sentinel is a scroll trigger, and scrolling is not a keyboard gesture
                // anyone should have to perform 30 times. This is the same action, reachable by
                // `Tab`, and it is also the fallback if the observer never re-fires because the
                // sentinel stayed in view (a viewport taller than a whole page of rows).
                if has_more {
                    button {
                        class: "ik-wl-more-btn",
                        r#type: "button",
                        onclick: move |_| {
                            request.write().pages += 1;
                        },
                        {i18n.t("watchlist.loadMore")}
                    }
                }
                span { class: "ik-wl-legend", {i18n.t("watchlist.keyLegend")} }
            }
        }

        BulkBar { selected, board, reload }
    }
}

/// The unread total across the whole filtered list, not just the loaded page — the group
/// aggregates carry it, so the headline stays right as the reader scrolls.
fn unread_total(board: &Board) -> i64 {
    board.groups.iter().map(|g| g.chapter_count).sum()
}

/// The ids of the loaded rows in one band.
fn band_ids(items: &[WatchlistItem], band: Bucket) -> Vec<SeriesId> {
    items
        .iter()
        .filter(|i| bucket_of(i.latest_chapter_at.as_deref()) == band)
        .map(|i| i.series_id)
        .collect()
}

/// The sortable `Released` column header, with the caret showing the direction in force.
fn sort_header(
    i18n: Translator,
    query: &WatchlistQuery,
    go: impl FnMut(WatchlistQuery) + Clone + 'static,
) -> Element {
    let active = query.sort == Sort::Released;
    let order = query.effective_order();
    let query = query.clone();
    let mut go = go;
    rsx! {
        button {
            class: if active { "ik-wl-sortcol on" } else { "ik-wl-sortcol" },
            r#type: "button",
            "aria-sort": match (active, order) {
                (false, _) => "none",
                (true, Order::Asc) => "ascending",
                (true, Order::Desc) => "descending",
            },
            onclick: move |_| {
                let mut next = query.clone();
                if active {
                    // Clicking the active column flips it; clicking an inactive one adopts its
                    // natural direction rather than whatever the previous column was using.
                    next.order = Some(order.flip());
                } else {
                    next.sort = Sort::Released;
                    next.order = None;
                }
                go(next);
            },
            {i18n.t("watchlist.col.released")}
            if active {
                Ic { icon: Icon::ChevronDown, size: 13 }
            }
        }
    }
}

/// "AniList synced 4m ago", when there is an account linked to say it about.
fn sync_chip(i18n: Translator, status: &Resource<Option<SyncAccountStatus>>) -> Element {
    let Some(Some(status)) = status.read().clone() else {
        return rsx! {};
    };
    if !status.linked {
        return rsx! {};
    }
    let when = rel_time(i18n, status.last_synced_at.as_deref());
    rsx! {
        span { class: "ik-pill jade", title: "{when}",
            {i18n.args("watchlist.syncedAgo", &[("when", &when)])}
        }
    }
}

/// Nothing matched. Which advice to give depends on *why* there is nothing: an account with no
/// tracked titles needs a pointer to Discover, while a filtered-to-nothing list needs the
/// filter widened — and offering "reset your filters" to someone with an empty watchlist is
/// worse than saying nothing.
fn empty_state(
    i18n: Translator,
    query: &WatchlistQuery,
    go: impl FnMut(WatchlistQuery) + Clone + 'static,
) -> Element {
    if !query.is_narrowed() && query.status.is_none() {
        return rsx! {
            div { class: "ik-empty",
                Ic { icon: Icon::Watchlist, size: 28 }
                p { style: "margin:10px 0 4px;font-weight:600;", {i18n.t("watchlist.empty")} }
                Link { to: Route::Discover {}, class: "ik-btn", style: "margin-top:10px;",
                    {i18n.t("watchlist.emptyCta")}
                }
            }
        };
    }
    let mut go = go;
    rsx! {
        div { class: "ik-empty",
            Ic { icon: Icon::Search, size: 28 }
            p { style: "margin:10px 0 4px;font-weight:600;", {i18n.t("watchlist.noMatch.title")} }
            p { class: "ik-muted", style: "font-size:13px;", {i18n.t("watchlist.noMatch.hint")} }
            button {
                class: "ik-btn",
                style: "margin-top:10px;",
                onclick: move |_| go(WatchlistQuery { status: None, unread_only: false, ..WatchlistQuery::default() }),
                {i18n.t("watchlist.resetFilters")}
            }
        }
    }
}

/// The list's keyboard contract (§3.5), which is what replaced drag-and-drop.
///
/// `J`/`K` rather than only the arrow keys because the reader's hands are on the home row while
/// triaging, and both are bound so neither habit is punished. Every key here is inert while a
/// modifier other than `Shift` is held, so the browser's own `⌘K`/`Ctrl+F` keep working.
fn on_key(
    event: &Event<KeyboardData>,
    board: Signal<Board>,
    mut selected: Signal<HashSet<SeriesId>>,
    mut focus: Signal<usize>,
    mut menu_for: Signal<Option<SeriesId>>,
    ctx: RowCtx,
) {
    let modifiers = event.modifiers();
    if modifiers.ctrl() || modifiers.alt() || modifiers.meta() {
        if modifiers.ctrl() || modifiers.meta() {
            if let Key::Character(c) = event.key() {
                if c.eq_ignore_ascii_case("a") {
                    event.prevent_default();
                    let ids: HashSet<SeriesId> = board
                        .read()
                        .items
                        .iter()
                        .take(BULK_LIMIT)
                        .map(|i| i.series_id)
                        .collect();
                    selected.set(ids);
                }
            }
        }
        return;
    }

    let items = board.read().items.clone();
    if items.is_empty() {
        return;
    }
    let last = items.len() - 1;
    let current = (*focus.read()).min(last);
    let extend = modifiers.shift();

    let mut step = |to: usize, selected: &mut Signal<HashSet<SeriesId>>| {
        if extend {
            // Shift-stepping selects the row it lands on, so holding it sweeps a range without
            // needing an anchor to be tracked separately.
            selected.write().insert(items[to].series_id);
        }
        focus.set(to);
    };

    match event.key() {
        Key::ArrowDown => {
            event.prevent_default();
            step(current.saturating_add(1).min(last), &mut selected);
        }
        Key::ArrowUp => {
            event.prevent_default();
            step(current.saturating_sub(1), &mut selected);
        }
        Key::Escape => {
            if menu_for.peek().is_some() {
                menu_for.set(None);
            } else {
                selected.write().clear();
            }
        }
        Key::Enter => {
            event.prevent_default();
            row::continue_reading(&items[current]);
        }
        Key::Character(c) => match c.to_ascii_lowercase().as_str() {
            "j" => {
                event.prevent_default();
                step(current.saturating_add(1).min(last), &mut selected);
            }
            "k" => {
                event.prevent_default();
                step(current.saturating_sub(1), &mut selected);
            }
            "x" => {
                event.prevent_default();
                let id = items[current].series_id;
                let mut selection = selected.write();
                if !selection.remove(&id) && selection.len() < BULK_LIMIT {
                    selection.insert(id);
                }
            }
            "s" => {
                event.prevent_default();
                menu_for.set(Some(items[current].series_id));
            }
            "m" => {
                event.prevent_default();
                row::toggle_mute(&items[current], board, ctx);
            }
            "/" => {
                event.prevent_default();
                crate::browser::focus_and_select(toolbar::FILTER_INPUT_ID);
            }
            _ => {}
        },
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The bands must match the server's, or a group header counts rows that are not under it.
    /// The server's rule is `>= now() - 24 hours` and `>= now() - 7 days`; these are the same
    /// two boundaries from the other side.
    #[test]
    fn bands_match_the_servers_rolling_windows() {
        const HOUR: f64 = 3_600_000.0;
        assert_eq!(bucket_of_age(0.0), Bucket::Today);
        assert_eq!(bucket_of_age(23.9 * HOUR), Bucket::Today);
        assert_eq!(bucket_of_age(24.0 * HOUR), Bucket::ThisWeek);
        assert_eq!(bucket_of_age(6.9 * 24.0 * HOUR), Bucket::ThisWeek);
        assert_eq!(bucket_of_age(7.0 * 24.0 * HOUR), Bucket::Earlier);
    }

    /// A browser clock behind the server's produces a negative age. Banding that as `Earlier`
    /// would drop a just-released chapter to the bottom of the page — the one place the reader
    /// will not look for it.
    #[test]
    fn a_skewed_clock_does_not_bury_a_fresh_release() {
        assert_eq!(bucket_of_age(-60_000.0), Bucket::Today);
    }
}
