//! Owns the window of catalogue pages the Discover screen keeps mounted, and the requests that fill
//! it.

use std::collections::{HashMap, VecDeque};

use super::query::DiscoverFilters;
use crate::models::*;

/// Most covers one window keeps mounted at a time.
///
/// A sentinel with no ceiling turns a 54 000-title catalogue into an unbounded DOM: nothing is
/// released as it scrolls past, so the cost of the screen grows for as long as the reader keeps
/// going. At the ceiling the window *slides* instead: the page furthest above the viewport is
/// released, a spacer holds its measured height open, and scrolling back into that spacer fetches
/// it again. The DOM stays bounded and neither direction costs the reader a click.
pub(super) const WINDOW_ITEMS: usize = 600;

/// Which end of the window a request belongs to.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum Edge {
    /// A page released earlier, fetched back as the reader scrolls up and prepended.
    Head,
    /// New ground below the window, appended. The end the reader pages into.
    Tail,
}

/// One request: which catalogue page, and enough of the window to tell whether the answer still
/// belongs to it.
///
/// Separate from [`Held`] because the window slides *without* asking for anything: releasing a
/// page is not a fetch, and a request keyed on the whole window would issue one for a page the
/// window already holds. `generation` keeps two otherwise identical requests distinguishable, so
/// a response can never be merged into the window that replaced it.
#[derive(Clone, PartialEq)]
pub(super) struct Fetch {
    pub(super) generation: usize,
    pub(super) filters: DiscoverFilters,
    pub(super) size: usize,
    /// 0-based catalogue page index.
    pub(super) page: usize,
    pub(super) edge: Edge,
    /// Whether the server should count the matches. Only a window's first page asks: every later
    /// page has the same filters, so its count would be the same answer paid for again.
    pub(super) with_total: bool,
}

/// The pages one window holds, and where in the catalogue they sit.
///
/// The page size is part of it because a window is *expressed* in pages: a resize that changes the
/// size makes every page boundary in the loaded list wrong, so the window is rebuilt at the
/// reader's current anchor, not continued at a size the pages before it were never fetched
/// at.
#[derive(Clone, PartialEq)]
pub(super) struct Held {
    pub(super) generation: usize,
    pub(super) filters: DiscoverFilters,
    pub(super) size: usize,
    /// The first page this window ever asked for. Nothing above it was ever rendered, so there is
    /// no measured height to hold its place: that is what `Back to start` is for.
    pub(super) origin: usize,
    /// The first page still held.
    pub(super) start: usize,
    /// The held pages, in catalogue order, starting at [`Held::start`].
    pub(super) pages: VecDeque<Vec<SeriesSummary>>,
}

impl Held {
    /// An empty window anchored at the page containing catalogue item `at`.
    pub(super) fn new(generation: usize, filters: DiscoverFilters, size: usize, at: usize) -> Self {
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
    pub(super) fn first(&self) -> usize {
        self.start.saturating_mul(self.size)
    }

    /// Cards held, across every page.
    pub(super) fn count(&self) -> usize {
        self.pages.iter().map(Vec::len).sum()
    }

    /// Whether an anchor falls inside what the window holds.
    ///
    /// This is what separates "the reader scrolled" from "the reader jumped": an anchor the window
    /// already covers is a scroll position to record, and one outside it is a request to rebuild
    /// the window there, which is how a deep link and the back button reach the same code path.
    /// Scrolling up into the released span is neither, because the markers only ever name a page
    /// that is still held.
    pub(super) fn covers(&self, at: usize) -> bool {
        let span = self.pages.len().saturating_mul(self.size);
        at >= self.first() && at < self.first().saturating_add(span)
    }

    /// Pages the window keeps before it releases from the other end.
    ///
    /// Never under two: a window of one page would release the page the reader is looking at to
    /// make room for the one below it.
    pub(super) fn capacity(&self) -> usize {
        (WINDOW_ITEMS / self.size.max(1)).max(2)
    }

    /// Pages released above the window: the span the spacer has to hold open.
    pub(super) fn released(&self) -> usize {
        self.start.saturating_sub(self.origin)
    }

    /// The request for the window's first page, which also asks the server for the match count.
    pub(super) fn first_fetch(&self) -> Fetch {
        Fetch {
            generation: self.generation,
            filters: self.filters.clone(),
            size: self.size,
            page: self.start,
            edge: Edge::Tail,
            with_total: true,
        }
    }

    /// Adds an answered page at its end of the window, releasing from the other end past the
    /// window's capacity. Returns whether the tail was released, which means the end of the result
    /// set has something to ask for again.
    pub(super) fn absorb(&mut self, edge: Edge, page: usize, items: Vec<SeriesSummary>) -> bool {
        let mut released_tail = false;
        match edge {
            Edge::Tail => {
                self.pages.push_back(items);
                // Release from the head to make room. The spacer above grows by exactly the
                // height the released page occupied, so the document keeps its shape.
                while self.pages.len() > self.capacity() {
                    self.pages.pop_front();
                    self.start += 1;
                }
            }
            Edge::Head => {
                self.pages.push_front(items);
                self.start = page;
                while self.pages.len() > self.capacity() {
                    self.pages.pop_back();
                    // The window no longer holds the end of the result set, so the sentinel
                    // below it has something to ask for again. Without this the list stayed
                    // "finished" after one scroll up and the released tail never came back.
                    released_tail = true;
                }
            }
        }
        released_tail
    }

    /// The request for the page beyond `edge`, or `None` when the head is the window's origin.
    pub(super) fn next_fetch(&self, edge: Edge) -> Option<Fetch> {
        let page = match edge {
            Edge::Tail => self.start + self.pages.len(),
            // Nothing above the window's origin was ever rendered, so there is no height to
            // hold its place and no spacer to scroll into.
            Edge::Head if self.start > self.origin => self.start - 1,
            Edge::Head => return None,
        };
        Some(Fetch {
            generation: self.generation,
            filters: self.filters.clone(),
            size: self.size,
            page,
            edge,
            with_total: false,
        })
    }
}

/// The height a released run of pages has to leave behind, from what those pages measured while
/// they were mounted.
///
/// A page with no measurement falls back to the mean of the ones there are: every page in a window
/// is the same whole rows of the same cards, so the mean *is* the height; the fallback only
/// covers a page released before its observer first reported.
pub(super) fn released_height(heights: &HashMap<usize, f64>, from: usize, to: usize) -> f64 {
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
    /// instead of multiplying one of them is what keeps the spacer honest when the last page of
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
        // Nothing measured at all is a spacer of nothing, never a NaN height in the style
        // attribute.
        assert!(released_height(&HashMap::new(), 0, 3).abs() < f64::EPSILON);
    }

    /// The tail end slides: past capacity, every page appended releases one from the head, and
    /// the released span is what the spacer above has to hold open.
    #[test]
    fn absorbing_past_capacity_at_the_tail_releases_the_head() {
        let mut held = Held::new(1, DiscoverFilters::default(), 24, 0);
        let capacity = held.capacity();
        for page in 0..capacity {
            assert!(!held.absorb(Edge::Tail, page, Vec::new()));
        }
        assert_eq!(
            (held.start, held.pages.len(), held.released()),
            (0, capacity, 0)
        );

        assert!(
            !held.absorb(Edge::Tail, capacity, Vec::new()),
            "a head release is not a tail one"
        );
        assert_eq!(
            (held.start, held.pages.len(), held.released()),
            (1, capacity, 1)
        );
    }

    /// Fetching a released page back at the head pushes the tail out, and the caller has to be
    /// told: the list stayed "finished" after one scroll up and the released tail never came back.
    #[test]
    fn absorbing_past_capacity_at_the_head_reports_the_released_tail() {
        let mut held = Held::new(1, DiscoverFilters::default(), 24, 0);
        let capacity = held.capacity();
        for page in 0..=capacity {
            held.absorb(Edge::Tail, page, Vec::new());
        }
        assert!(held.absorb(Edge::Head, 0, Vec::new()));
        assert_eq!(
            (held.start, held.pages.len(), held.released()),
            (0, capacity, 0)
        );
    }

    /// The tail asks for the page after the last one held. The head asks for the page before the
    /// first one held, and for nothing at the origin, above which no page was ever rendered.
    #[test]
    fn the_next_fetch_names_the_page_beyond_each_edge() {
        let at_origin = window(4, 4, 3);
        let tail = at_origin
            .next_fetch(Edge::Tail)
            .expect("the tail always has a next page");
        assert_eq!(
            (tail.page, tail.edge, tail.with_total),
            (7, Edge::Tail, false)
        );
        assert!(at_origin.next_fetch(Edge::Head).is_none());

        let slid = window(4, 6, 3);
        let head = slid
            .next_fetch(Edge::Head)
            .expect("a released page above the window");
        assert_eq!(
            (head.page, head.edge, head.with_total),
            (5, Edge::Head, false)
        );
    }
}
