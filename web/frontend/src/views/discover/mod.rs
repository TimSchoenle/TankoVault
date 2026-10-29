//! Owns Discover (`docs/DESIGN_SYSTEM.md` §7.2): filter panel and a results grid that pages on a
//! scroll sentinel, filtered/sorted/paginated server-side via `GET /v1/series` (§9.1). The panel
//! and chip bar live in [`filters`] and [`active`]; the URL contract lives in [`query`].
//!
//! Everything the screen shows is addressable: the filters *and* how far down the reader had
//! scrolled ride in the query string, so a shared link opens on the same covers. The position is
//! an item index instead of a page number because the page size is measured from the window's
//! width: page 12 is a different series on a phone than on a desktop, an item index is not.

mod active;
mod filters;
mod query;
mod requests;
mod window;

use crate::api;
use crate::components::{
    unmeasured, use_grid_fit, CoverCard, ErrorBox, ErrorLine, GridFitProbe, SkeletonGrid,
};
use crate::hooks::{use_reload, Reload};
use crate::i18n::{use_i18n, Translator};
use crate::icons::{Ic, Icon};
use crate::util::thousands;
use crate::Route;
use active::ActiveFilters;
use dioxus::prelude::*;
use filters::FilterPanel;
use inkstone_ui::Button;
use query::DiscoverFilters;
pub(crate) use query::{DiscoverQuery, Sort, Tracking};
use std::collections::{BTreeSet, HashMap};
use window::{released_height, Edge, Fetch, Held};
/// How many *rows* of covers one fetched page holds. How many series that is depends on how
/// many columns the window fits: see [`crate::components::use_grid_fit`].
const PAGE_ROWS: usize = 4;

/// Discover screen.
#[component]
pub(crate) fn Discover(query: DiscoverQuery) -> Element {
    let i18n = use_i18n();
    let api = api::use_api();
    let reload = use_reload();
    let nav = navigator();
    let fit = use_grid_fit(PAGE_ROWS);

    let panel_open = use_signal(|| true);
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
    // Which pages of the catalogue overlap the viewport, by absolute index: relative ones would
    // shift under the set every time the window slid. The lowest is the reader's position;
    // tracking the whole set instead of the last event is what keeps a fast scroll (which can
    // leave two markers intersecting between frames) from recording the wrong one.
    let mut on_screen = use_signal(BTreeSet::<usize>::new);
    let mut restore = use_signal(|| false);

    let (all_tags, all_providers) = requests::use_facets(api);

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
        fetch.set(Some(window.first_fetch()));
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
            requests::page(&client, request, i18n).await
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
        let released_tail = {
            let mut window = held.write();
            let Some(window) = window.as_mut() else {
                return;
            };
            if window.generation != answered.generation {
                return;
            }
            window.absorb(answered.edge, answered.page, data.items.clone())
        };
        merged.set(Some(answered.clone()));
        if let Some(reported) = data.total {
            total.set(reported);
        }
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
    // A failure leaves this true, which is deliberate: paging stops at a refused page instead of
    // asking the same server the same question every time the sentinel crosses the viewport.
    let in_flight = request.is_some() && merged.read().as_ref() != request.as_ref();
    let more = !*exhausted.read();

    // Asking for a page is the only way one is ever fetched, and it is refused while another is
    // still on the wire: an ungated ask is a duplicate request per frame.
    let mut ask = move |edge: Edge| {
        if in_flight {
            return;
        }
        let request = held
            .peek()
            .as_ref()
            .and_then(|window| window.next_fetch(edge));
        if let Some(request) = request {
            fetch.set(Some(request));
        }
    };

    let discover_class = if *panel_open.read() {
        "ik-discover"
    } else {
        "ik-discover collapsed"
    };
    let head_filters = query.filters.clone();
    let panel_filters = query.filters.clone();
    let chip_filters = query.filters.clone();

    rsx! {
        {results_head(i18n, panel_open, &query.filters, on_filters)}

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
                    {count_line(i18n, first, count, *total.read(), start, &head_filters, on_filters)}
                    div { class: "ik-scroll-pages",
                        // The span released above the window, held open at the height it had while
                        // it was mounted: without it, releasing a page scrolls the covers out from
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
                                // One clone per page instead of per card: each marker's handler
                                // has to outlive this render to answer a scroll.
                                let filters = head_filters.clone();
                                let at_now = query.at;
                                // The page's own place in the catalogue. Everything a handler
                                // records is keyed on this instead of on `n`, which shifts under
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
                                        // One marker per page, sized to the page, not an
                                        // observer per card: 600 covers would be 600 observers to
                                        // answer a question one per page answers as well. It is
                                        // also what the deep link scrolls to: exactly right when
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
                    // again on remount if it is still in view, which is what keeps a viewport
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
                    {scroll_foot(i18n, failure.as_deref(), in_flight, more, reload, ask)}
                }
            }
        }
    }
}

/// The foot of the results: a failed page's retry, the loading line, the load-more button, or
/// the end of the result set.
fn scroll_foot(
    i18n: Translator,
    failure: Option<&str>,
    in_flight: bool,
    more: bool,
    reload: Reload,
    mut ask: impl FnMut(Edge) + Clone + 'static,
) -> Element {
    rsx! {
        div { class: "ik-scroll-foot",
            if let Some(message) = failure {
                ErrorLine { message: message.to_owned() }
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

/// The results heading: the filter panel's toggle, the page title and the sort control.
fn results_head(
    i18n: Translator,
    mut panel_open: Signal<bool>,
    filters: &DiscoverFilters,
    mut on_filters: impl FnMut(DiscoverFilters) + Clone + 'static,
) -> Element {
    let sort_now = filters.sort;
    let sort_filters = filters.clone();
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
    }
}

/// Which cards of how many are on screen, with a jump back to the top of the result set.
fn count_line(
    i18n: Translator,
    first: usize,
    count: usize,
    total: i64,
    start: usize,
    filters: &DiscoverFilters,
    mut on_filters: impl FnMut(DiscoverFilters) + Clone + 'static,
) -> Element {
    let head_filters = filters.clone();
    rsx! {
        div { class: "ik-count-line",
            {
                i18n.args(
                    "discover.countLine",
                    &[
                        ("first", &thousands(i64::try_from(first + 1).unwrap_or(0))),
                        ("last", &thousands(i64::try_from(first + count).unwrap_or(0))),
                        ("total", &thousands(total)),
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
    }
}

/// Nothing to show: either the filters match nothing, or the link named a position the current
/// result set no longer reaches. They need different offers: widening helps the first and does
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
