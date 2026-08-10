//! Discover (`DESIGN_SPEC` §7.2): filter panel plus a results grid that pages on a scroll
//! sentinel, filtered/sorted/paginated server-side via `GET /v1/series` (§9.1). The panel and chip
//! bar live in [`filters`] and [`active`]; the URL contract lives in [`query`].
//!
//! Everything the screen shows is addressable: the filters *and* how far down the reader had
//! scrolled ride in the query string, so a shared link opens on the same covers. The position is
//! an item index rather than a page number because the page size is measured from the window's
//! width — page 12 is a different series on a phone than on a desktop, an item index is not.

mod active;
mod filters;
mod query;

use crate::api;
use crate::components::{
    unmeasured, use_grid_fit, CoverCard, ErrorBox, ErrorLine, GridFitProbe, SkeletonGrid,
};
use crate::hooks::use_reload;
use crate::i18n::{use_i18n, Translator};
use crate::icons::{Ic, Icon};
use crate::models::*;
use crate::util::thousands;
use crate::Route;
use active::ActiveFilters;
use dioxus::prelude::*;
use filters::FilterPanel;
use inkstone_ui::Button;
use progenitor_client::ResponseValue;
use query::{DiscoverFilters, YEAR_MAX, YEAR_MIN};
pub(crate) use query::{DiscoverQuery, Sort, Tracking};
use std::collections::{BTreeSet, HashMap, VecDeque};
/// How many *rows* of covers one fetched page holds. How many series that is depends on how
/// many columns the window fits — see [`crate::components::use_grid_fit`].
const PAGE_ROWS: usize = 4;

/// Most covers one window keeps mounted at a time.
///
/// A sentinel with no ceiling turns a 54 000-title catalogue into an unbounded DOM: nothing is
/// released as it scrolls past, so the cost of the screen grows for as long as the reader keeps
/// going. At the ceiling the window *slides* instead — the page furthest above the viewport is
/// released, a spacer holds its measured height open, and scrolling back into that spacer fetches
/// it again. The DOM stays bounded and neither direction costs the reader a click.
const WINDOW_ITEMS: usize = 600;

/// Which end of the window a request belongs to.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Edge {
    /// A page released earlier, fetched back as the reader scrolls up and prepended.
    Head,
    /// New ground below the window, appended. The end the reader pages into.
    Tail,
}

/// One request: which catalogue page, and enough of the window to tell whether the answer still
/// belongs to it.
///
/// Separate from [`Held`] because the window slides *without* asking for anything — releasing a
/// page is not a fetch, and a request keyed on the whole window would issue one for a page the
/// window already holds. `generation` keeps two otherwise identical requests distinguishable, so
/// a response can never be merged into the window that replaced it.
#[derive(Clone, PartialEq)]
struct Fetch {
    generation: usize,
    filters: DiscoverFilters,
    size: usize,
    /// 0-based catalogue page index.
    page: usize,
    edge: Edge,
}

/// The pages one window holds, and where in the catalogue they sit.
///
/// The page size is part of it because a window is *expressed* in pages: a resize that changes the
/// size makes every page boundary in the loaded list wrong, so the window is rebuilt at the
/// reader's current anchor rather than continued at a size the pages before it were never fetched
/// at.
#[derive(Clone, PartialEq)]
struct Held {
    generation: usize,
    filters: DiscoverFilters,
    size: usize,
    /// The first page this window ever asked for. Nothing above it was ever rendered, so there is
    /// no measured height to hold its place — that is what `Back to start` is for.
    origin: usize,
    /// The first page still held.
    start: usize,
    /// The held pages, in catalogue order, starting at [`Held::start`].
    pages: VecDeque<Vec<SeriesSummary>>,
}

impl Held {
    /// An empty window anchored at the page containing catalogue item `at`.
    fn new(generation: usize, filters: DiscoverFilters, size: usize, at: usize) -> Self {
        let origin = at / size.max(1);
        Self {
            generation,
            filters,
            size,
            origin,
            start: origin,
            pages: VecDeque::new(),
        }
    }

    /// The catalogue index of the first card held.
    fn first(&self) -> usize {
        self.start.saturating_mul(self.size)
    }

    /// Cards held, across every page.
    fn count(&self) -> usize {
        self.pages.iter().map(Vec::len).sum()
    }

    /// Whether an anchor falls inside what the window holds.
    ///
    /// This is what separates "the reader scrolled" from "the reader jumped": an anchor the window
    /// already covers is a scroll position to record, and one outside it is a request to rebuild
    /// the window there — which is how a deep link and the back button reach the same code path.
    /// Scrolling up into the released span is neither, because the markers only ever name a page
    /// that is still held.
    fn covers(&self, at: usize) -> bool {
        let span = self.pages.len().saturating_mul(self.size);
        at >= self.first() && at < self.first().saturating_add(span)
    }

    /// Pages the window keeps before it releases from the other end.
    ///
    /// Never under two: a window of one page would release the page the reader is looking at to
    /// make room for the one below it.
    fn capacity(&self) -> usize {
        (WINDOW_ITEMS / self.size.max(1)).max(2)
    }

    /// Pages released above the window — the span the spacer has to hold open.
    fn released(&self) -> usize {
        self.start.saturating_sub(self.origin)
    }
}

/// The height a released run of pages has to leave behind, from what those pages measured while
/// they were mounted.
///
/// A page with no measurement falls back to the mean of the ones there are: every page in a window
/// is the same whole rows of the same cards, so the mean *is* the height — the fallback only
/// covers a page released before its observer first reported.
fn released_height(heights: &HashMap<usize, f64>, from: usize, to: usize) -> f64 {
    if to <= from || heights.is_empty() {
        return 0.0;
    }
    #[expect(
        clippy::cast_precision_loss,
        reason = "a map of page heights, one entry per page of a bounded window"
    )]
    let mean = heights.values().sum::<f64>() / heights.len() as f64;
    (from..to)
        .map(|page| heights.get(&page).copied().unwrap_or(mean))
        .sum()
}

/// Discover screen.
#[component]
pub(crate) fn Discover(query: DiscoverQuery) -> Element {
    let i18n = use_i18n();
    let api = api::use_api();
    let reload = use_reload();
    let nav = navigator();
    let fit = use_grid_fit(PAGE_ROWS);

    let mut panel_open = use_signal(|| true);
    let mut held = use_signal(|| Option::<Held>::None);
    let mut fetch = use_signal(|| Option::<Fetch>::None);
    let mut generation = use_signal(|| 0usize);
    // The request that has landed. Equal to `fetch` exactly when nothing is on the wire, which is
    // what gates a second request.
    let mut merged = use_signal(|| Option::<Fetch>::None);
    let mut total = use_signal(|| 0i64);
    let mut exhausted = use_signal(|| false);
    let mut settled = use_signal(|| false);
    // Every page this window has rendered, by catalogue page index, as it measured. A released
    // page has to leave exactly its own height behind or the release scrolls the covers out from
    // under the reader.
    let mut heights = use_signal(HashMap::<usize, f64>::new);
    // Which pages of the catalogue overlap the viewport, by absolute index — relative ones would
    // shift under the set every time the window slid. The lowest is the reader's position;
    // tracking the whole set rather than the last event is what keeps a fast scroll — which can
    // leave two markers intersecting between frames — from recording the wrong one.
    let mut on_screen = use_signal(BTreeSet::<usize>::new);
    let mut restore = use_signal(|| false);

    // Degrades to an empty facet on failure rather than blocking the screen — a missing
    // tag/provider filter is cheaper than an error state.
    let tags_res = use_resource(move || {
        let client = api.client();
        async move {
            client
                .tags()
                .send()
                .await
                .map(ResponseValue::into_inner)
                .unwrap_or_default()
        }
    });
    let all_tags: Vec<TagFacet> = tags_res.read_unchecked().clone().unwrap_or_default();

    let providers_res = use_resource(move || {
        let client = api.client();
        async move {
            client
                .providers()
                .send()
                .await
                .map(ResponseValue::into_inner)
                .unwrap_or_default()
        }
    });
    let all_providers: Vec<PublicProvider> =
        providers_res.read_unchecked().clone().unwrap_or_default();

    // The route is the source of truth, so this is the one place a window is built: a filter
    // change, a resize, a deep link and the back button all reach it as "the current window does
    // not answer the current URL".
    use_effect(use_reactive!(|query| {
        let Some(size) = fit.page_size() else {
            return;
        };
        let stale = held
            .peek()
            .as_ref()
            .is_none_or(|w| w.filters != query.filters || w.size != size || !w.covers(query.at));
        if !stale {
            return;
        }
        let next = *generation.peek() + 1;
        generation.set(next);
        let window = Held::new(next, query.filters.clone(), size, query.at);
        fetch.set(Some(Fetch {
            generation: next,
            filters: window.filters.clone(),
            size,
            page: window.start,
            edge: Edge::Tail,
        }));
        held.set(Some(window));
        merged.set(None);
        heights.write().clear();
        on_screen.write().clear();
        settled.set(false);
        exhausted.set(false);
        restore.set(query.at > 0);
    }));

    // One page per run. The request is the only dependency, so a page asked for at either end is
    // exactly one more request and nothing else re-fetches.
    let page = use_resource(move || {
        reload.track();
        let current = fetch.read().clone();
        let client = api.client();
        async move {
            // Parked, not guessed: a request sized for the wrong grid would be answered and then
            // thrown away by the corrected one, doubling this screen's query load.
            let Some(request) = current else {
                return unmeasured().await;
            };
            let filters = &request.filters;
            let mut builder = client.list();
            if let Some(content_type) = filters.types.first() {
                builder = builder.content_type(content_type.token());
            }
            if let Some(status) = filters.statuses.first() {
                builder = builder.status(status.token());
            }
            if let Some(provider) = filters.provider.clone() {
                builder = builder.provider(provider);
            }
            if !filters.inc.is_empty() {
                builder = builder.tag(filters.inc.clone());
            }
            if !filters.exc.is_empty() {
                builder = builder.exclude_tag(filters.exc.clone());
            }
            if filters.year_min > YEAR_MIN {
                builder = builder.year_min(filters.year_min);
            }
            if filters.year_max < YEAR_MAX {
                builder = builder.year_max(filters.year_max);
            }
            if filters.min_chapters > 0 {
                builder = builder.min_chapters(filters.min_chapters);
            }
            if let Some(tracking) = filters.tracking.param() {
                builder = builder.tracking(tracking);
            }
            let outcome = builder
                .sort(filters.sort.token())
                .page(i64::try_from(request.page).unwrap_or(i64::MAX))
                .limit(i64::try_from(request.size).unwrap_or(24))
                .send()
                .await
                .map(|r| {
                    let total = r
                        .headers()
                        .get("x-total-count")
                        .and_then(|v| v.to_str().ok())
                        .and_then(|s| s.parse::<i64>().ok())
                        .unwrap_or_else(|| i64::try_from(r.as_ref().len()).unwrap_or(0));
                    let next_cursor = r
                        .headers()
                        .get("x-next-cursor")
                        .and_then(|v| v.to_str().ok())
                        .and_then(|s| s.parse::<i64>().ok());
                    SeriesPage {
                        items: r.into_inner(),
                        total,
                        next_cursor,
                    }
                })
                .map_err(|e| api::friendly_error(i18n, e));
            // The request travels with its own answer, success or failure. A response that outlived
            // the window it was asked for is then simply not the current one, rather than a page
            // merged into a list it does not belong to.
            (request, outcome)
        }
    });

    // Renders from the window, not from the resource: the resource holds one page, and a failed
    // second page must not blank the first.
    use_effect(move || {
        let answer = page.read();
        let Some((answered, Ok(data))) = &*answer else {
            return;
        };
        if merged.peek().as_ref() == Some(answered) {
            return;
        }
        let mut released_tail = false;
        {
            let mut window = held.write();
            let Some(window) = window.as_mut() else {
                return;
            };
            if window.generation != answered.generation {
                return;
            }
            match answered.edge {
                Edge::Tail => {
                    window.pages.push_back(data.items.clone());
                    // Release from the head to make room. The spacer above grows by exactly the
                    // height the released page occupied, so the document keeps its shape.
                    while window.pages.len() > window.capacity() {
                        window.pages.pop_front();
                        window.start += 1;
                    }
                }
                Edge::Head => {
                    window.pages.push_front(data.items.clone());
                    window.start = answered.page;
                    while window.pages.len() > window.capacity() {
                        window.pages.pop_back();
                        // The window no longer holds the end of the result set, so the sentinel
                        // below it has something to ask for again. Without this the list stayed
                        // "finished" after one scroll up and the released tail never came back.
                        released_tail = true;
                    }
                }
            }
        }
        merged.set(Some(answered.clone()));
        total.set(data.total);
        // Only a tail page can reach the end of the result set; a head page never does.
        if answered.edge == Edge::Tail {
            exhausted.set(data.next_cursor.is_none());
        } else if released_tail {
            exhausted.set(false);
        }
        settled.set(true);
    });

    let go = move |next: DiscoverQuery| {
        nav.push(Route::Discover { query: next });
    };
    let on_filters = move |filters: DiscoverFilters| go(DiscoverQuery::with_filters(filters));

    // Borrowed, not cloned: a window is up to `WINDOW_ITEMS` summaries, and cloning it on every
    // render would be the most expensive thing this screen does.
    let window = held.read();
    let size = window.as_ref().map_or(1, |w| w.size.max(1));
    let start = window.as_ref().map_or(0, |w| w.start);
    let origin = window.as_ref().map_or(0, |w| w.origin);
    let first = window.as_ref().map_or(0, Held::first);
    let count = window.as_ref().map_or(0, Held::count);
    let released = window.as_ref().map_or(0, Held::released);
    // Read only while something *is* released, so a window that never slid does not re-render on
    // every page's first measurement.
    let lift = if released > 0 {
        released_height(&heights.read(), origin, start)
    } else {
        0.0
    };
    let request = fetch.read().clone();
    // Only this window's failure counts: the resource still holds the last answer while the next
    // one is in flight, and that answer can belong to a window three filter changes ago.
    let answer = page.read();
    let failure = answer
        .as_ref()
        .and_then(|(answered, outcome)| match outcome {
            Err(message) if request.as_ref() == Some(answered) => Some(message.clone()),
            _ => None,
        });
    // A failure leaves this true, which is deliberate: paging stops at a refused page rather than
    // asking the same server the same question every time the sentinel crosses the viewport.
    let in_flight = request.is_some() && merged.read().as_ref() != request.as_ref();
    let more = !*exhausted.read();

    // Asking for a page is the only way one is ever fetched, and it is refused while another is
    // still on the wire — an ungated ask is a duplicate request per frame.
    let mut ask = move |edge: Edge| {
        if in_flight {
            return;
        }
        let request = {
            let borrowed = held.peek();
            let Some(window) = borrowed.as_ref() else {
                return;
            };
            let page = match edge {
                Edge::Tail => window.start + window.pages.len(),
                // Nothing above the window's origin was ever rendered, so there is no height to
                // hold its place and no spacer to scroll into.
                Edge::Head if window.start > window.origin => window.start - 1,
                Edge::Head => return,
            };
            Fetch {
                generation: window.generation,
                filters: window.filters.clone(),
                size: window.size,
                page,
                edge,
            }
        };
        fetch.set(Some(request));
    };

    let discover_class = if *panel_open.read() {
        "ik-discover"
    } else {
        "ik-discover collapsed"
    };
    let sort_now = query.filters.sort;
    let sort_filters = query.filters.clone();
    let head_filters = query.filters.clone();
    let panel_filters = query.filters.clone();
    let chip_filters = query.filters.clone();

    rsx! {
        div { class: "ik-results-head",
            button {
                class: "ik-panel-toggle",
                title: i18n.t("discover.toggleFilters"),
                onclick: move |_| {
                    let cur = *panel_open.peek();
                    panel_open.set(!cur);
                },
                Ic { icon: Icon::Tune, size: 18 }
            }
            h1 { class: "ik-page-title", {i18n.t("nav.discover")} }
            span { class: "ik-rail-spacer", style: "flex:1;" }
            label { class: "ik-muted", style: "font-size:13px;", {i18n.t("discover.sortLabel")} }
            select {
                class: "ik-select",
                value: "{sort_now.token()}",
                onchange: move |e| {
                    let mut next = sort_filters.clone();
                    next.sort = Sort::parse(&e.value());
                    on_filters(next);
                },
                for s in Sort::ALL {
                    option { value: "{s.token()}", selected: sort_now == s, {i18n.t(s.label_key())} }
                }
            }
        }

        ActiveFilters {
            filters: chip_filters,
            tags: all_tags.clone(),
            providers: all_providers.clone(),
            on_change: on_filters,
        }

        div { class: "{discover_class}",
            if *panel_open.read() {
                FilterPanel {
                    filters: panel_filters,
                    tags: all_tags.clone(),
                    providers: all_providers.clone(),
                    on_change: on_filters,
                    on_reset: move |()| on_filters(DiscoverFilters::default()),
                }
            }
            div {
                // Inside the results column, and outside the branches below: this is what measures
                // the grid, and the first window is parked until it reports.
                GridFitProbe { fit }

                if !*settled.read() {
                    if let Some(message) = failure.clone() {
                        ErrorBox { message, on_retry: move |()| reload.bump() }
                    } else {
                        SkeletonGrid { count: fit.page_size_or_default() }
                    }
                } else if count == 0 {
                    {empty_state(i18n, &head_filters, origin > 0, on_filters)}
                } else {
                    div { class: "ik-count-line",
                        {
                            i18n.args(
                                "discover.countLine",
                                &[
                                    ("first", &thousands(i64::try_from(first + 1).unwrap_or(0))),
                                    ("last", &thousands(i64::try_from(first + count).unwrap_or(0))),
                                    ("total", &thousands(*total.read())),
                                ],
                            )
                        }
                        // A window that does not start at the top was opened from a link, or has
                        // slid; either way the top of the result set is a jump, not a scroll.
                        if start > 0 {
                            {
                                let filters = head_filters.clone();
                                rsx! {
                                    button {
                                        class: "ik-scroll-more",
                                        r#type: "button",
                                        onclick: move |_| on_filters(filters.clone()),
                                        {i18n.t("discover.backToStart")}
                                    }
                                }
                            }
                        }
                    }
                    div { class: "ik-scroll-pages",
                        // The span released above the window, held open at the height it had while
                        // it was mounted — without it, releasing a page scrolls the covers out from
                        // under the reader. The lead-in hangs below the spacer's foot, over the
                        // first page still held, so the released page is on the wire while the
                        // reader is still a screenful short of the gap.
                        if released > 0 {
                            div { class: "ik-scroll-spacer", style: "height:{lift:.1}px;",
                                if !in_flight {
                                    div {
                                        class: "ik-scroll-lead",
                                        onvisible: move |event| {
                                            if event.data.is_intersecting().unwrap_or(false) {
                                                ask(Edge::Head);
                                            }
                                        },
                                    }
                                }
                            }
                        }
                        for (n , chunk) in window.iter().flat_map(|w| w.pages.iter()).enumerate() {
                            {
                                // One clone per page rather than per card: each marker's handler
                                // has to outlive this render to answer a scroll.
                                let filters = head_filters.clone();
                                let at_now = query.at;
                                // The page's own place in the catalogue. Everything a handler
                                // records is keyed on this rather than on `n`, which shifts under
                                // the whole set every time the window slides.
                                let index = start + n;
                                rsx! {
                                    div {
                                        key: "page-{index}",
                                        class: "ik-scroll-page",
                                        onresize: move |event| {
                                            if let Ok(box_size) = event.get_border_box_size() {
                                                heights.write().insert(index, box_size.height);
                                            }
                                        },
                                        // One marker per page, sized to the page, rather than an
                                        // observer per card: 600 covers would be 600 observers to
                                        // answer a question one per page answers as well. It is
                                        // also what the deep link scrolls to — exactly right when
                                        // the page size matches the one the link was written at,
                                        // and at most one page out when it does not.
                                        div {
                                            class: "ik-page-mark",
                                            onmounted: move |event| {
                                                if n > 0 || !*restore.peek() {
                                                    return;
                                                }
                                                restore.set(false);
                                                spawn(async move {
                                                    let _ = event.data().scroll_to(ScrollBehavior::Instant).await;
                                                });
                                            },
                                            onvisible: move |event| {
                                                let showing = event.data.is_intersecting().unwrap_or(false);
                                                {
                                                    let mut seen = on_screen.write();
                                                    if showing { seen.insert(index); } else { seen.remove(&index); }
                                                }
                                                let Some(top) = on_screen.peek().iter().next().copied() else {
                                                    return;
                                                };
                                                let at = top.saturating_mul(size);
                                                if at != at_now {
                                                    // Replace, not push: a scroll is not a
                                                    // navigation, and pushing would put one history
                                                    // entry per page between the reader and the
                                                    // screen they arrived from.
                                                    nav.replace(Route::Discover {
                                                        query: DiscoverQuery { filters: filters.clone(), at },
                                                    });
                                                }
                                            },
                                        }
                                        div { class: "ik-grid",
                                            for series in chunk.iter().cloned() {
                                                CoverCard { key: "{series.id}", series }
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                    // `onvisible` (`IntersectionObserver`) fetches the next page with no scroll
                    // handler and no polling. It unmounts while a page is on the wire, so it fires
                    // again on remount if it is still in view — which is what keeps a viewport
                    // taller than one page loading without a click.
                    if more && !in_flight {
                        div {
                            class: "ik-scroll-sentinel",
                            onvisible: move |event| {
                                if event.data.is_intersecting().unwrap_or(false) {
                                    ask(Edge::Tail);
                                }
                            },
                        }
                    }
                    div { class: "ik-scroll-foot",
                        if let Some(message) = failure.clone() {
                            ErrorLine { message }
                            button {
                                class: "ik-scroll-more",
                                r#type: "button",
                                onclick: move |_| reload.bump(),
                                {i18n.t("common.tryAgain")}
                            }
                        } else if in_flight {
                            span { {i18n.t("discover.loadingMore")} }
                        } else if more {
                            // Keyboard-reachable equivalent of the scroll sentinel, and the
                            // fallback when the observer never re-fires.
                            button {
                                class: "ik-scroll-more",
                                r#type: "button",
                                onclick: move |_| ask(Edge::Tail),
                                {i18n.t("discover.loadMore")}
                            }
                        } else {
                            span { {i18n.t("discover.endOfResults")} }
                        }
                    }
                }
            }
        }
    }
}

/// Nothing to show: either the filters match nothing, or the link named a position the current
/// result set no longer reaches. They need different offers — widening helps the first and does
/// nothing for the second.
fn empty_state(
    i18n: Translator,
    filters: &DiscoverFilters,
    windowed: bool,
    on_filters: impl FnMut(DiscoverFilters) + Clone + 'static,
) -> Element {
    let mut on_filters = on_filters;
    let filters = filters.clone();
    if windowed {
        return rsx! {
            div { class: "ik-empty",
                Ic { icon: Icon::Explore, size: 28 }
                p { style: "margin:10px 0 4px;font-weight:600;", {i18n.t("discover.pastEnd.title")} }
                p { class: "ik-muted", style: "font-size:13px;", {i18n.t("discover.pastEnd.hint")} }
                Button {
                    style: "margin-top:10px;",
                    on_click: move |_| on_filters(filters.clone()),
                    {i18n.t("discover.backToStart")}
                }
            }
        };
    }
    rsx! {
        div { class: "ik-empty",
            Ic { icon: Icon::Search, size: 28 }
            p { style: "margin:10px 0 4px;font-weight:600;", {i18n.t("discover.noMatch.title")} }
            p { class: "ik-muted", style: "font-size:13px;", {i18n.t("discover.noMatch.hint")} }
            Button {
                style: "margin-top:10px;",
                on_click: move |_| on_filters(DiscoverFilters::default()),
                {i18n.t("discover.resetFilters")}
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A window of `pages` pages of 24, starting at page `start`, built at page `origin`.
    fn window(origin: usize, start: usize, pages: usize) -> Held {
        Held {
            generation: 1,
            filters: DiscoverFilters::default(),
            size: 24,
            origin,
            start,
            pages: std::iter::repeat_with(Vec::new).take(pages).collect(),
        }
    }

    /// The distinction the whole paging state machine rests on: an anchor the window already
    /// covers is the reader scrolling within it, and one outside it is a jump that has to rebuild
    /// the window there. Wrong in one direction and every scroll tick reloads the grid; wrong in
    /// the other and a deep link renders the top of the catalogue under a URL naming item 500.
    #[test]
    fn an_anchor_inside_the_window_is_a_scroll_not_a_jump() {
        let loaded = window(4, 4, 3);
        assert!(loaded.covers(96), "the window's own first card");
        assert!(loaded.covers(120), "a page the reader scrolled to");
        assert!(loaded.covers(167), "the last card asked for");
        assert!(!loaded.covers(95), "one card above the window");
        assert!(!loaded.covers(168), "the page after the last one asked for");
    }

    /// A window that has slid covers the pages it still *holds*, not the ones it once did.
    /// Anything else and the anchor the reader scrolled past would keep the released pages
    /// looking loaded, so scrolling back would render a gap instead of fetching them again.
    #[test]
    fn a_window_that_has_slid_covers_only_what_it_holds() {
        let slid = window(0, 5, 3);
        assert!(!slid.covers(0), "released, and no longer in the window");
        assert!(slid.covers(120), "the first page still held");
        assert_eq!(slid.released(), 5);
    }

    /// The ceiling is on covers, not pages: a page is as wide as the window, so a fixed page
    /// ceiling would hold 600 covers on a phone and 1200 on a desktop.
    #[test]
    fn the_page_ceiling_follows_the_page_size() {
        assert_eq!(window(0, 0, 1).capacity(), WINDOW_ITEMS / 24);
        let wide = Held {
            size: 60,
            ..window(0, 0, 1)
        };
        assert_eq!(wide.capacity(), WINDOW_ITEMS / 60);
        // A page wider than the whole ceiling still gets two, never one: a window of one page
        // would release the page the reader is looking at to make room for the next.
        let huge = Held {
            size: WINDOW_ITEMS * 2,
            ..window(0, 0, 1)
        };
        assert_eq!(huge.capacity(), 2);
    }

    /// A released page has to leave exactly its own height behind. Summing the *measured* heights
    /// rather than multiplying one of them is what keeps the spacer honest when the last page of
    /// a result set is short; a mismatch here scrolls the grid under the reader's eyes on every
    /// release.
    #[test]
    fn the_spacer_holds_the_heights_the_released_pages_measured() {
        let heights = HashMap::from([(0, 1000.0), (1, 1200.0), (2, 400.0)]);
        assert!((released_height(&heights, 0, 3) - 2600.0).abs() < f64::EPSILON);
        assert!((released_height(&heights, 1, 2) - 1200.0).abs() < f64::EPSILON);
        assert!((released_height(&heights, 2, 2)).abs() < f64::EPSILON);
        // A page released before its observer reported falls back to the mean of the rest.
        let partial = HashMap::from([(0, 1000.0), (1, 1000.0)]);
        assert!((released_height(&partial, 0, 3) - 3000.0).abs() < f64::EPSILON);
        // Nothing measured at all is a spacer of nothing, never a NaN height in the style attribute.
        assert!(released_height(&HashMap::new(), 0, 3).abs() < f64::EPSILON);
    }
}
