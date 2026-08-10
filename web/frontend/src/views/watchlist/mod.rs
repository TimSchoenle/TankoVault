//! Watchlist — one filtered, sorted, virtualized list (design turn 4, option `4a`; the cover
//! grid is `4b`, in [`grid`]). Filtering, sorting, grouping and paging are server-side
//! (`GET /v1/me/watchlist`, §4.2), so this module holds no comparator over the rows.

mod bulk;
mod grid;
mod query;
mod row;
mod toolbar;

use crate::api;
use crate::components::{
    unmeasured, use_grid_fit, AuthRequired, ErrorBox, FocusTargets, GridFit, GridFitProbe,
    OutcomeLine, SkeletonRows,
};
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
use inkstone_ui::{button_class, Button, Pill, Size, Tone};
use progenitor_client::ResponseValue;
pub(crate) use query::WatchlistQuery;
use query::{Order, Released, Sort, View};
use row::{GroupHeader, RowCtx, WatchRow};
use std::collections::HashSet;
use tankovault_api_client::Client;
use toolbar::{FilterBar, StatusTabs};
/// Rows fetched per page in the list view. The list pages on a scroll sentinel, so this is the
/// size of one bite, not of the list. The cover grid sizes its own bite from [`PAGE_ROWS`],
/// because a bite that is not a whole number of tiles ends every page in a half-empty row.
const PAGE_SIZE: i64 = 60;

/// Rows of covers one bite of the grid view holds; the window's width decides the rest.
const PAGE_ROWS: usize = 8;

/// The most rows this list holds at once, matching the API's own `limit` cap.
///
/// Past it the window *slides*: the page furthest above the viewport is released and a spacer
/// holds the height it had open, so scrolling on costs one request per page and the DOM stays
/// bounded. It used to be where the list simply stopped — the sentinel had nothing left to ask
/// for, so every row past the 200th of a longer watchlist was unreachable.
const MAX_ROWS: i64 = 200;

/// The largest selection a bulk call accepts, mirroring the API's own cap. Enforced here so
/// `Select all` on a 564-row tab does not build a request the server will refuse.
const BULK_LIMIT: usize = 200;

/// The provider the "Sync now" button drives. Per-provider control lives on Account → Sync.
const SYNC_PROVIDER: &str = "anilist";

/// Which release-recency band a row renders under.
///
/// Must match the server's `ReleaseBucket` rolling windows — "Today" is the last 24 hours, not
/// the calendar day, since the server doesn't know this browser's timezone and the group
/// header aggregates come from the server too.
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
/// Split from the clock for host-target testability. A negative age (clock behind the
/// server's) bands as `Today` rather than `Earlier`, so a skewed clock can't bury a fresh release.
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
    let parsed = crate::platform::parse_timestamp_ms(s);
    if parsed.is_nan() {
        return Bucket::Earlier;
    }
    bucket_of_age(crate::platform::now_ms() - parsed)
}

/// What the list renders, in order: band headings interleaved with the rows under them.
///
/// Precomputed rather than decided in the `for` loop — `rsx!` has nowhere to keep "what band
/// was the last row in" state between iterations.
enum Entry {
    Band(Bucket),
    Row(usize),
}

/// Interleave band headings into the row order.
///
/// Relies on rows arriving already grouped (the server sorts by release instant); under any
/// other sort the bands are meaningless, so [`Sort::groups_by_release`] disables them.
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

/// One fetch's worth of state: the view the URL describes, and which slice of it the window holds.
///
/// Kept in one signal so a filter change and its page-depth reset can't land as two separate
/// invalidations — split, the resource would fire once against the stale depth first.
#[derive(Clone, PartialEq)]
struct Request {
    query: WatchlistQuery,
    /// 0-based index of the first page held.
    start: i64,
    /// Pages held, counted from `start`.
    pages: i64,
}

impl Request {
    /// Rows to ask for: the whole window, refetched as one slice rather than appended — this
    /// avoids the accumulate-and-dedupe bugs an append-only cache has (a filter change racing an
    /// in-flight page, a `reload` appending a duplicate).
    fn limit(&self, bite: i64) -> i64 {
        bite.saturating_mul(self.pages).min(MAX_ROWS)
    }

    /// Rows the window has released above it — what the request skips and what the spacer holds
    /// open.
    fn skipped(&self, bite: i64) -> i64 {
        bite.saturating_mul(self.start)
    }

    /// Pages the window keeps before it releases from the other end.
    ///
    /// Never under two: a window of one page would release the page the reader is looking at to
    /// make room for the one below it.
    fn capacity(bite: i64) -> i64 {
        (MAX_ROWS / bite.max(1)).max(2)
    }

    /// Take in one more page, sliding the window forward once it is full.
    fn advance(&mut self, bite: i64) {
        if self.pages < Self::capacity(bite) {
            self.pages += 1;
        } else {
            self.start += 1;
        }
    }

    /// Take back the page above the window. Nothing happens at the top of the list — `saturating_sub`
    /// would not do here, since it saturates at `i64::MIN` and a negative offset is a 400.
    fn retreat(&mut self) {
        self.start = (self.start - 1).max(0);
    }
}

/// Rows one bite of the list holds: whole rows of tiles in the cover grid, a fixed page in the
/// list, and `None` until the grid has been measured — which is what parks the first request.
fn bite_size(view: View, fit: GridFit) -> Option<i64> {
    match view {
        View::Grid => fit.page_size().and_then(|size| i64::try_from(size).ok()),
        View::List => Some(PAGE_SIZE),
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
    // Context, not six threaded props — every handle in `RowCtx` is `Copy`, so this is a
    // lookup, not a clone.
    let ctx = use_context_provider(|| RowCtx {
        api,
        i18n,
        reload,
        outcome,
    });

    let mut request = use_signal(|| Request {
        query: query.clone(),
        start: 0,
        pages: 1,
    });
    let mut board = use_signal(Board::default);
    // The request the rendered rows answer. Equal to `request` exactly when nothing is on the
    // wire, which is what keeps the scroll sentinels from asking twice for the same page.
    let mut applied = use_signal(|| Option::<Request>::None);
    // How tall the rendered window is. The spacer above it is built from this over the rows in
    // it, because a row's height is a stylesheet decision the narrow-viewport rules re-make.
    let mut window_px = use_signal(|| 0f64);
    let mut settled = use_signal(|| false);
    let mut selected = use_signal(HashSet::<SeriesId>::new);
    let mut focus = use_signal(|| 0usize);
    let menu_for = use_signal(|| Option::<SeriesId>::None);
    let focus_targets = crate::components::use_focus_targets();
    // Only the cover grid has columns to fill; the list view is one row per title at any width.
    let fit = use_grid_fit(PAGE_ROWS);

    // Route is the source of truth for view state, so a route change restarts paging. Guarded
    // on inequality, or `Signal::set`'s unconditional invalidation double-fetches on first render.
    use_effect(use_reactive!(|query| {
        if request.peek().query != query {
            request.set(Request {
                query,
                start: 0,
                pages: 1,
            });
            selected.write().clear();
            focus.set(0);
        }
    }));

    let page = use_resource(move || {
        reload.track();
        let req = request.read().clone();
        // In the grid view the bite is whole rows of tiles, and the fetch waits for the first
        // measurement rather than firing a request the corrected one would immediately replace.
        let bite = bite_size(req.query.view, fit);
        let client = api.client();
        let authed = session.is_authenticated();
        async move {
            if !authed {
                let empty = WatchlistView {
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
                    // Offset paging, not keyset: this list refetches its whole window on every
                    // page rather than appending, which is immune to the shifting-list defect the
                    // cursor exists to fix and cannot use a cursor to express "rows 120 to 180".
                    // The token is there for consumers that do append.
                    next_cursor: None,
                };
                return (req, Ok(empty));
            }
            let Some(bite) = bite else {
                return unmeasured().await;
            };
            let outcome = watchlist_slice(&client, &req.query, req.limit(bite), req.skipped(bite))
                .send()
                .await
                .map(ResponseValue::into_inner)
                .map_err(|e| api::friendly_error(i18n, e));
            // The request travels with its own answer, so the chrome can tell a rendered window
            // from one still on the wire without a second flag to keep in step.
            (req, outcome)
        }
    });

    // Renders from `board`, not the resource directly, so an optimistic mutation has somewhere
    // to write and a refetch doesn't blank the list mid-flight.
    use_effect(move || {
        if let Some((answered, Ok(view))) = &*page.read() {
            board.set(Board {
                items: view.items.clone(),
                counts: Some(view.counts.clone()),
                groups: view.groups.clone(),
                total: view.total,
            });
            applied.set(Some(answered.clone()));
            settled.set(true);
        }
    });

    // Best-effort — a down sync service must not take the watchlist with it (same as Discover's facets).
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
    // Replace, not push — the filter box debounces at 200ms, so push would spam one history
    // entry per keystroke.
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
            // Pull before push, or a title just added on the other side gets overwritten.
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
    let loaded_rows = i64::try_from(loaded).unwrap_or(i64::MAX);
    let bite = bite_size(query.view, fit).unwrap_or(PAGE_SIZE);
    let skipped = request.read().skipped(bite);
    let has_more = skipped + loaded_rows < snapshot.total;
    // A failure leaves this true, which is deliberate: paging stops at a refused slice rather than
    // asking the same server the same question every time a sentinel crosses the viewport.
    let in_flight = applied.read().as_ref() != Some(&*request.read());
    let lift = released_height(*window_px.read(), loaded_rows, skipped);
    let grouped = query.sort.groups_by_release();
    let entries = layout(&snapshot.items, grouped);

    // The only two ways the window moves, and both are refused while a slice is on the wire — an
    // ungated sentinel asks for the same page once per frame.
    let mut slide = move |forward: bool| {
        if in_flight || (!forward && request.peek().start == 0) {
            return;
        }
        let mut pending = request.write();
        if forward {
            pending.advance(bite);
        } else {
            pending.retreat();
        }
    };

    // The rows released above the window, held open at the height they had. Without it, sliding
    // would jerk the list by a page under the reader's hands. The lead-in covers the last
    // screenful of the spacer, so the rows are back before the reader reaches the gap.
    let released_above = rsx! {
        if skipped > 0 {
            div { class: "ik-wl-spacer", style: "height:{lift:.1}px;", "aria-hidden": "true",
                if !in_flight {
                    div {
                        class: "ik-wl-lead",
                        onvisible: move |event| {
                            if event.data.is_intersecting().unwrap_or(false) {
                                slide(false);
                            }
                        },
                    }
                }
            }
        }
    };

    // `total` and the unread sum both come from the filtered view; `counts.all` ignores the
    // status filter, so mixing it in here would show e.g. "598 titles" over a list of 40.
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
                Button {
                    disabled: syncing.is_busy(),
                    on_click: sync_now,
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

        // Mounted for the whole grid view, skeleton included: its first measurement is what
        // releases the parked fetch above.
        if query.view == View::Grid {
            GridFitProbe { fit, tiles: true }
        }

        if let Some((_, Err(message))) = &*page.read() {
            ErrorBox { message: message.clone(), on_retry: move |()| reload.bump() }
        } else if !*settled.read() {
            SkeletonRows { count: 8 }
        } else if snapshot.items.is_empty() {
            {empty_state(i18n, &query, go)}
        } else if query.view == View::Grid {
            {released_above.clone()}
            div {
                onresize: move |event| {
                    if let Ok(size) = event.get_border_box_size() {
                        window_px.set(size.height);
                    }
                },
                CoverGrid { items: snapshot.items.clone(), selected }
            }
        } else {
            // The header carries the row's cell classes so both are hidden by the same rule:
            // the responsive block used to address the header by `nth-child`, which every
            // column added between them silently shifted.
            div { class: "ik-wl-head", role: "row",
                span {}
                span { {i18n.t("watchlist.col.title")} }
                span { class: "ik-wl-next", {i18n.t("watchlist.col.nextUnread")} }
                span { class: "ik-wl-progress", {i18n.t("watchlist.col.progress")} }
                span { style: "text-align:right;", {i18n.t("watchlist.col.unread")} }
                {sort_header(i18n, &query, go)}
                span { class: "ik-wl-sources", {i18n.t("watchlist.col.sources")} }
                span {}
            }
            {released_above.clone()}
            div {
                class: "ik-wl-list",
                role: "grid",
                tabindex: "0",
                "aria-label": i18n.t("nav.watchlist"),
                // What the spacer above is derived from. Safe on this element because nothing
                // inside it observes its own size — a resize event bubbles in the desktop build,
                // so a nested observer would report its box as this one's (see `GridFitProbe`).
                onresize: move |event| {
                    if let Ok(size) = event.get_border_box_size() {
                        window_px.set(size.height);
                    }
                },
                // Names the focused row for a screen reader — without it the keyboard contract
                // is silent to anyone not looking at the highlight.
                "aria-activedescendant": snapshot
                    .items
                    .get(*focus.read())
                    .map_or_else(String::new, |item| format!("wl-row-{}", item.series_id)),
                onkeydown: move |event| on_key(&event, board, selected, focus, menu_for, ctx, focus_targets),
                for entry in entries {
                    match entry {
                        Entry::Band(band) => {
                            let stats = snapshot.groups.iter().find(|g| g.key == band.key());
                            let span = band_span(&snapshot.groups, band, query.effective_order());
                            let band_query = query.clone();
                            rsx! {
                                GroupHeader {
                                    key: "band-{band.key()}",
                                    band,
                                    title_count: stats.map_or(0, |g| g.title_count),
                                    chapter_count: stats.map_or(0, |g| g.chapter_count),
                                    blocked: mark_group_blocked(i18n, span.count),
                                    on_mark: move |()| mark_band_read(&band_query, span, ctx),
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
            // Sentinel div: `onvisible` (`IntersectionObserver`) takes in the next page with no
            // scroll handler or polling. It unmounts while a slice is on the wire, so it fires
            // again on remount if it is still in view — which is what keeps a viewport taller
            // than one page loading without a click.
            if has_more && !in_flight {
                div {
                    class: "ik-wl-sentinel",
                    onvisible: move |event| {
                        if event.data.is_intersecting().unwrap_or(false) {
                            slide(true);
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
                                ("first", &thousands(skipped + 1)),
                                ("last", &thousands(skipped + loaded_rows)),
                                ("total", &thousands(snapshot.total)),
                            ],
                        )
                    }
                }
                // Keyboard-reachable equivalent of the scroll sentinel: the list's own contract
                // is `J`/`K`, which moves a highlight without moving the viewport, so a reader
                // who never scrolls would otherwise have no way to reach the next page.
                if has_more {
                    button {
                        class: "ik-wl-more-btn",
                        r#type: "button",
                        onclick: move |_| slide(true),
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

/// Where a band sits in the filtered list: rows above it, and rows in it.
///
/// The bands are contiguous runs — grouping only applies under `Released`, which is the same
/// instant they band by — so a band is addressable by offset, and the group aggregates say how
/// long each one is. That is what lets `Mark group read` act on the whole band rather than on
/// whichever part of it the window happens to hold.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
struct BandSpan {
    offset: i64,
    count: i64,
}

/// Locate `band` in the order the list actually renders it.
///
/// The direction matters: the `Released` column header flips the sort, and with it which band is
/// at the top. Reading the offsets in rank order regardless would address `Earlier` at offset 0
/// while it renders last, and mark the wrong titles read.
fn band_span(groups: &[WatchlistGroup], band: Bucket, order: Order) -> BandSpan {
    let mut displayed = [Bucket::Today, Bucket::ThisWeek, Bucket::Earlier];
    if order == Order::Asc {
        displayed.reverse();
    }
    let mut offset = 0;
    for candidate in displayed {
        let count = groups
            .iter()
            .find(|g| g.key == candidate.key())
            .map_or(0, |g| g.title_count);
        if candidate == band {
            return BandSpan { offset, count };
        }
        offset += count;
    }
    BandSpan { offset, count: 0 }
}

/// Why a band cannot be marked read in one go, or `None` when it can.
fn mark_group_blocked(i18n: Translator, count: i64) -> Option<String> {
    if count > i64::try_from(BULK_LIMIT).unwrap_or(i64::MAX) {
        return Some(i18n.args(
            "watchlist.markGroupTooBig",
            &[("limit", &BULK_LIMIT.to_string())],
        ));
    }
    (count == 0).then(|| i18n.t("watchlist.markGroupEmpty"))
}

/// Mark every title in one band read.
///
/// The ids come from the server, not from the rendered rows: the action used to be refused
/// whenever the band was not fully loaded, which — on any list longer than one page, and on every
/// list at all now that the window slides — is nearly always. The button was disabled and said so
/// in a `title` nobody opens, so clicking it did nothing and looked like a broken control.
#[expect(
    clippy::large_types_passed_by_value,
    reason = "`RowCtx` outlives this call inside a spawned future; see its doc comment"
)]
fn mark_band_read(query: &WatchlistQuery, span: BandSpan, ctx: RowCtx) {
    if span.count <= 0 {
        return;
    }
    let query = query.clone();
    let client = ctx.api.client();
    spawn(async move {
        let listed = watchlist_slice(&client, &query, span.count, span.offset)
            .send()
            .await;
        let ids: Vec<SeriesId> = match listed {
            Ok(view) => view
                .into_inner()
                .items
                .iter()
                .map(|i| i.series_id)
                .collect(),
            Err(e) => return ctx.failed(e),
        };
        if ids.is_empty() {
            return;
        }
        match client
            .bulk_mark_read()
            .body(WatchlistBulkIds { series_ids: ids })
            .send()
            .await
        {
            // Changes unread counts across rows and band aggregates — genuinely warrants a
            // refetch, not a local edit.
            Ok(_) => ctx.reload.bump(),
            Err(e) => ctx.failed(e),
        }
    });
}

/// One watchlist request, built from the view state.
///
/// Shared by the page fetch and by [`mark_band_read`], so a filter can never apply to what the
/// reader is looking at and not to what the band action marks.
fn watchlist_slice<'a>(
    client: &'a Client,
    query: &WatchlistQuery,
    limit: i64,
    offset: i64,
) -> tankovault_api_client::builder::Watchlist<'a> {
    let mut builder = client
        .watchlist()
        .sort(query.sort.token())
        .order(query.effective_order().token())
        .unread_only(query.unread_only)
        .source_issues(query.source_issues)
        .limit(limit)
        .offset(offset);
    if let Some(status) = query.status_token() {
        builder = builder.status(status);
    }
    if !query.q.is_empty() {
        builder = builder.q(query.q.clone());
    }
    if query.released != Released::Any {
        builder = builder.released_since(query.released.token());
    }
    builder
}

/// The height the rows released above the window have to leave behind.
///
/// Derived from the window's own measured height rather than from a row constant: the row height
/// is a stylesheet decision the narrow-viewport rules re-make, and the band headings between the
/// rows are part of what a released page occupied. It is an average over the window, so it is
/// exact while the rows are uniform and close where they are not — the alternative is a page-tall
/// jump on every slide.
fn released_height(window_px: f64, loaded: i64, released: i64) -> f64 {
    if loaded <= 0 || released <= 0 {
        return 0.0;
    }
    #[expect(
        clippy::cast_precision_loss,
        reason = "both counts are list lengths, far inside f64's exact integer range"
    )]
    {
        window_px / loaded as f64 * released as f64
    }
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
            // `ik-wl-released` so the header cell is dropped by the same rule as the row cell.
            class: if active { "ik-wl-sortcol ik-wl-released on" } else { "ik-wl-sortcol ik-wl-released" },
            r#type: "button",
            "aria-sort": match (active, order) {
                (false, _) => "none",
                (true, Order::Asc) => "ascending",
                (true, Order::Desc) => "descending",
            },
            onclick: move |_| {
                let mut next = query.clone();
                if active {
                    // Flips the active column; an inactive one adopts its natural default instead.
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

/// "`AniList` synced 4m ago", when there is an account linked to say it about.
fn sync_chip(i18n: Translator, status: &Resource<Option<SyncAccountStatus>>) -> Element {
    let Some(Some(status)) = status.read().clone() else {
        return rsx! {};
    };
    if !status.linked {
        return rsx! {};
    }
    let when = rel_time(i18n, status.last_synced_at.as_deref());
    rsx! {
        Pill {
            tone: Tone::Positive,
            title: "{when}",
            {i18n.args("watchlist.syncedAgo", &[("when", &when)])}
        }
    }
}

/// Nothing matched: an empty account needs a pointer to Discover, a filtered-to-nothing list
/// needs "reset filters" — offering the reset to an empty account would be worse than nothing.
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
                Link { to: Route::Discover { query: crate::views::DiscoverQuery::default() }, class: button_class(Tone::Neutral, Size::Md, false), style: "margin-top:10px;",
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
            Button {
                style: "margin-top:10px;",
                on_click: move |_| go(WatchlistQuery { status: None, unread_only: false, ..WatchlistQuery::default() }),
                {i18n.t("watchlist.resetFilters")}
            }
        }
    }
}

/// The list's keyboard contract (§3.5).
///
/// `J`/`K` alongside the arrow keys (hands stay on the home row while triaging). Every key is
/// inert under any modifier but `Shift`, so the browser's own `⌘K`/`Ctrl+F` keep working.
#[expect(
    clippy::large_types_passed_by_value,
    reason = "`RowCtx` reaches a spawned future through the actions this dispatches to; see \
              its doc comment in `row.rs`"
)]
fn on_key(
    event: &Event<KeyboardData>,
    board: Signal<Board>,
    mut selected: Signal<HashSet<SeriesId>>,
    mut focus: Signal<usize>,
    mut menu_for: Signal<Option<SeriesId>>,
    ctx: RowCtx,
    focus_targets: FocusTargets,
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
            // Shift-stepping selects the landing row, sweeping a range without tracking an anchor.
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
                crate::components::focus_and_select(focus_targets.filter);
            }
            _ => {}
        },
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Bands must match the server's rule (`>= now() - 24h`, `>= now() - 7d`), or a group
    /// header counts rows that aren't under it.
    #[test]
    fn bands_match_the_servers_rolling_windows() {
        const HOUR: f64 = 3_600_000.0;
        assert_eq!(bucket_of_age(0.0), Bucket::Today);
        assert_eq!(bucket_of_age(23.9 * HOUR), Bucket::Today);
        assert_eq!(bucket_of_age(24.0 * HOUR), Bucket::ThisWeek);
        assert_eq!(bucket_of_age(6.9 * 24.0 * HOUR), Bucket::ThisWeek);
        assert_eq!(bucket_of_age(7.0 * 24.0 * HOUR), Bucket::Earlier);
    }

    /// A negative age (browser clock behind the server's) must band as `Today`, not `Earlier` —
    /// which would bury a just-released chapter at the bottom of the page.
    #[test]
    fn a_skewed_clock_does_not_bury_a_fresh_release() {
        assert_eq!(bucket_of_age(-60_000.0), Bucket::Today);
    }

    /// The window must stop growing at the API's own `limit` cap and start *sliding* instead.
    /// Past the cap the server answers the same 200 rows however many pages are asked for, so the
    /// request got steadily more expensive while nothing new arrived — and the list simply ended,
    /// leaving every row past the 200th of a longer watchlist unreachable.
    #[test]
    fn the_window_slides_once_it_reaches_the_api_cap() {
        let mut window = Request {
            query: WatchlistQuery::default(),
            start: 0,
            pages: 1,
        };
        assert_eq!(window.limit(PAGE_SIZE), PAGE_SIZE);
        assert_eq!(window.skipped(PAGE_SIZE), 0);

        for _ in 0..10 {
            window.advance(PAGE_SIZE);
        }
        // Whole pages only, so the offset stays a page boundary — three of them here, not the
        // 200 the cap would allow, because 200 is not a whole number of pages.
        assert_eq!(
            window.limit(PAGE_SIZE),
            Request::capacity(PAGE_SIZE) * PAGE_SIZE
        );
        assert!(
            window.limit(PAGE_SIZE) <= MAX_ROWS,
            "never past the API's cap"
        );
        assert!(window.start > 0, "the window has to move once it is full");
        assert_eq!(
            window.skipped(PAGE_SIZE),
            window.start * PAGE_SIZE,
            "what the request skips is what the spacer holds open"
        );

        // And back: scrolling up returns the rows the slide released, and stops at the top.
        while window.start > 0 {
            window.retreat();
        }
        window.retreat();
        assert_eq!(window.skipped(PAGE_SIZE), 0);
    }

    /// A window of one page would release the page the reader is looking at to make room for the
    /// next one, which reads as the list erasing itself as it is scrolled.
    #[test]
    fn a_window_is_never_one_page() {
        assert_eq!(Request::capacity(MAX_ROWS * 2), 2);
        assert_eq!(Request::capacity(PAGE_SIZE), MAX_ROWS / PAGE_SIZE);
    }

    /// `Mark group read` addresses a band by offset, so the offsets have to follow the order the
    /// list is *rendered* in. Read in rank order regardless of direction, `Earlier` would be
    /// addressed at offset 0 while it renders last — and the action would mark today's releases
    /// read instead.
    #[test]
    fn a_band_is_addressed_in_the_order_it_renders() {
        let groups = vec![
            WatchlistGroup {
                key: "today".to_owned(),
                title_count: 3,
                chapter_count: 9,
            },
            WatchlistGroup {
                key: "week".to_owned(),
                title_count: 5,
                chapter_count: 12,
            },
            WatchlistGroup {
                key: "earlier".to_owned(),
                title_count: 40,
                chapter_count: 88,
            },
        ];

        // Newest first: today, then this week, then earlier.
        assert_eq!(
            band_span(&groups, Bucket::Earlier, Order::Desc),
            BandSpan {
                offset: 8,
                count: 40
            }
        );
        assert_eq!(
            band_span(&groups, Bucket::Today, Order::Desc),
            BandSpan {
                offset: 0,
                count: 3
            }
        );
        // Oldest first flips it, and `Today` is now the run at the bottom.
        assert_eq!(
            band_span(&groups, Bucket::Today, Order::Asc),
            BandSpan {
                offset: 45,
                count: 3
            }
        );
        // A band the server did not report is empty, not offset zero of the whole list.
        assert_eq!(
            band_span(&[], Bucket::ThisWeek, Order::Desc),
            BandSpan {
                offset: 0,
                count: 0
            }
        );
    }

    /// The spacer has to hold exactly what was released, or every slide moves the rows under the
    /// reader's hands. Nothing measured yet is a spacer of nothing — never a NaN in a style
    /// attribute, which the browser drops and the list then jumps by a whole window.
    #[test]
    fn the_spacer_holds_the_height_the_released_rows_had() {
        assert!((released_height(6800.0, 100, 200) - 13600.0).abs() < f64::EPSILON);
        assert!(released_height(0.0, 100, 200).abs() < f64::EPSILON);
        assert!(released_height(6800.0, 0, 200).abs() < f64::EPSILON);
        assert!(released_height(6800.0, 100, 0).abs() < f64::EPSILON);
    }
}
