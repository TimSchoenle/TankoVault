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
use std::collections::BTreeSet;
/// How many *rows* of covers one fetched page holds. How many series that is depends on how
/// many columns the window fits — see [`crate::components::use_grid_fit`].
const PAGE_ROWS: usize = 4;

/// Most covers one window will page in before it stops on its own.
///
/// A sentinel with no ceiling turns a 54 000-title catalogue into an unbounded DOM: nothing is
/// released as it scrolls past, so the cost of the screen grows for as long as the reader keeps
/// going. At the ceiling the grid offers to continue from where it stopped, which is this same
/// window rebuilt at that anchor — the reader keeps scrolling, the DOM does not keep growing.
const MAX_WINDOW_ITEMS: usize = 600;

/// One loaded window of the catalogue: the filter it belongs to, where it starts, and how far it
/// has been paged.
///
/// The page size is part of it because a window is *expressed* in pages: a resize that changes the
/// size makes every page boundary in the loaded list wrong, so the window is rebuilt at the
/// reader's current anchor rather than continued at a size the pages before it were never fetched
/// at. `generation` keeps two otherwise identical windows distinguishable, so a response can never
/// be merged into the window that replaced it.
#[derive(Clone, PartialEq)]
struct Window {
    generation: usize,
    filters: DiscoverFilters,
    size: usize,
    /// 0-based index of the window's first page.
    start: usize,
    /// Pages asked for so far, counted from `start`.
    want: usize,
}

impl Window {
    /// The catalogue index of this window's first card.
    fn first(&self) -> usize {
        self.start.saturating_mul(self.size)
    }

    /// Whether an anchor falls inside the window as it has been asked for.
    ///
    /// This is what separates "the reader scrolled" from "the reader jumped": an anchor the window
    /// already covers is a scroll position to record, and one outside it is a request to rebuild
    /// the window there — which is how a deep link, the back button and the continue-from-here
    /// control all arrive at the same code path.
    fn covers(&self, at: usize) -> bool {
        let span = self.want.saturating_mul(self.size);
        at >= self.first() && at < self.first().saturating_add(span)
    }

    /// Pages this window will fetch before it stops paging on its own.
    fn max_pages(&self) -> usize {
        (MAX_WINDOW_ITEMS / self.size.max(1)).max(1)
    }
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
    let mut window = use_signal(|| Option::<Window>::None);
    let mut generation = use_signal(|| 0usize);
    // Accumulated rather than refetched as one prefix (which is what the watchlist does): the
    // catalogue endpoint caps `limit` at 100, so a prefix request cannot express a deep window.
    let mut items = use_signal(Vec::<SeriesSummary>::new);
    let mut total = use_signal(|| 0i64);
    let mut exhausted = use_signal(|| false);
    // Pages merged into `items`; `want` minus this is what is still on the wire.
    let mut loaded = use_signal(|| 0usize);
    let mut merged = use_signal(|| Option::<Window>::None);
    let mut settled = use_signal(|| false);
    // Which pages of the window overlap the viewport. The lowest is the reader's position;
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
        let stale = window
            .peek()
            .as_ref()
            .is_none_or(|w| w.filters != query.filters || w.size != size || !w.covers(query.at));
        if !stale {
            return;
        }
        let next = *generation.peek() + 1;
        generation.set(next);
        window.set(Some(Window {
            generation: next,
            filters: query.filters.clone(),
            size,
            start: query.at / size,
            want: 1,
        }));
        items.write().clear();
        on_screen.write().clear();
        loaded.set(0);
        settled.set(false);
        exhausted.set(false);
        restore.set(query.at > 0);
    }));

    // One page per run. The window is the only dependency, so a bumped `want` is exactly one more
    // request and nothing else re-fetches.
    let page = use_resource(move || {
        reload.track();
        let current = window.read().clone();
        let client = api.client();
        async move {
            // Parked, not guessed: a request sized for the wrong grid would be answered and then
            // thrown away by the corrected one, doubling this screen's query load.
            let Some(window) = current else {
                return unmeasured().await;
            };
            let index = window.start + window.want - 1;
            let filters = &window.filters;
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
                .page(i64::try_from(index).unwrap_or(i64::MAX))
                .limit(i64::try_from(window.size).unwrap_or(24))
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
            // The window travels with its own answer, success or failure. A response that outlived
            // the window it was asked for is then simply not the current one, rather than a page
            // appended to a list it does not belong to.
            (window, outcome)
        }
    });

    // Renders from `items`, not from the resource: the resource holds one page, and a failed
    // second page must not blank the first.
    use_effect(move || {
        let outcome = page.read();
        let Some((answered, Ok(data))) = &*outcome else {
            return;
        };
        if window.peek().as_ref() != Some(answered) || merged.peek().as_ref() == Some(answered) {
            return;
        }
        merged.set(Some(answered.clone()));
        if answered.want == 1 {
            items.set(data.items.clone());
        } else {
            items.write().extend(data.items.iter().cloned());
        }
        total.set(data.total);
        exhausted.set(data.next_cursor.is_none());
        loaded.set(answered.want);
        settled.set(true);
    });

    let go = move |next: DiscoverQuery| {
        nav.push(Route::Discover { query: next });
    };
    let on_filters = move |filters: DiscoverFilters| go(DiscoverQuery::with_filters(filters));

    let snapshot = window.read().clone();
    let size = snapshot.as_ref().map_or(1, |w| w.size.max(1));
    let start = snapshot.as_ref().map_or(0, |w| w.start);
    let first = snapshot.as_ref().map_or(0, Window::first);
    let in_flight = snapshot.as_ref().is_some_and(|w| w.want > *loaded.read());
    let capped = snapshot
        .as_ref()
        .is_some_and(|w| *loaded.read() >= w.max_pages());
    let more = !*exhausted.read() && !capped;
    // Only this window's failure counts: the resource still holds the last answer while the next
    // one is in flight, and that answer can belong to a window three filter changes ago.
    let answer = page.read();
    let failure = answer
        .as_ref()
        .and_then(|(answered, outcome)| match outcome {
            Err(message) if snapshot.as_ref() == Some(answered) => Some(message.clone()),
            _ => None,
        });

    // Bumping `want` is the only way a page is ever asked for, and it is refused while the
    // previous one is still on the wire — an ungated bump is a duplicate request per frame.
    let mut advance = move || {
        if in_flight {
            return;
        }
        if let Some(pending) = window.write().as_mut() {
            pending.want += 1;
        }
    };

    let cards = items.read();
    let count = cards.len();
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
                    {empty_state(i18n, &head_filters, start > 0, on_filters)}
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
                        // A window that does not start at the top was opened from a link or
                        // continued past the ceiling, so the covers above it were never fetched.
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
                        for (n, chunk) in cards.chunks(size).enumerate() {
                            {
                                // One clone per page rather than per card: each marker's handler
                                // has to outlive this render to answer a scroll.
                                let filters = head_filters.clone();
                                let at_now = query.at;
                                rsx! {
                                    div { key: "page-{start + n}", class: "ik-scroll-page",
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
                                                    if showing { seen.insert(n); } else { seen.remove(&n); }
                                                }
                                                let Some(top) = on_screen.peek().iter().next().copied() else {
                                                    return;
                                                };
                                                let at = (start + top).saturating_mul(size);
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
                                    advance();
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
                        } else if capped {
                            span { {i18n.t("discover.windowFull")} }
                            {
                                let filters = head_filters.clone();
                                rsx! {
                                    button {
                                        class: "ik-scroll-more",
                                        r#type: "button",
                                        onclick: move |_| {
                                            go(DiscoverQuery { filters: filters.clone(), at: first + count });
                                        },
                                        {i18n.t("discover.continueHere")}
                                    }
                                }
                            }
                        } else if more {
                            // Keyboard-reachable equivalent of the scroll sentinel, and the
                            // fallback when the observer never re-fires.
                            button {
                                class: "ik-scroll-more",
                                r#type: "button",
                                onclick: move |_| advance(),
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

    fn window(start: usize, want: usize) -> Window {
        Window {
            generation: 1,
            filters: DiscoverFilters::default(),
            size: 24,
            start,
            want,
        }
    }

    /// The distinction the whole paging state machine rests on: an anchor the window already
    /// covers is the reader scrolling within it, and one outside it is a jump that has to rebuild
    /// the window there. Wrong in one direction and every scroll tick reloads the grid; wrong in
    /// the other and a deep link renders the top of the catalogue under a URL naming item 500.
    #[test]
    fn an_anchor_inside_the_window_is_a_scroll_not_a_jump() {
        let loaded = window(4, 3);
        assert!(loaded.covers(96), "the window's own first card");
        assert!(loaded.covers(120), "a page the reader scrolled to");
        assert!(loaded.covers(167), "the last card asked for");
        assert!(!loaded.covers(95), "one card above the window");
        assert!(!loaded.covers(168), "the page after the last one asked for");
    }

    /// Continuing at the ceiling hands the next window the index just past the loaded cards, which
    /// must read as a jump — if it did not, the button would rewrite the URL and load nothing.
    #[test]
    fn continuing_past_the_last_loaded_card_rebuilds_the_window() {
        let full = window(0, 5);
        assert!(!full.covers(full.first() + 5 * full.size));
    }

    /// The ceiling is on covers, not pages: a page is as wide as the window, so a fixed page
    /// ceiling would hold 600 covers on a phone and 1200 on a desktop.
    #[test]
    fn the_page_ceiling_follows_the_page_size() {
        assert_eq!(window(0, 1).max_pages(), MAX_WINDOW_ITEMS / 24);
        let wide = Window {
            size: 60,
            ..window(0, 1)
        };
        assert_eq!(wide.max_pages(), MAX_WINDOW_ITEMS / 60);
        // A page wider than the whole ceiling still gets one page rather than none.
        let huge = Window {
            size: MAX_WINDOW_ITEMS * 2,
            ..window(0, 1)
        };
        assert_eq!(huge.max_pages(), 1);
    }
}
