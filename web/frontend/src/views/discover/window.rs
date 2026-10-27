//! The window of catalogue pages the Discover screen keeps mounted, and the requests that fill it.

use std::collections::{HashMap, VecDeque};

use super::query::DiscoverFilters;
use crate::models::*;

/// Most covers one window keeps mounted at a time.
///
/// A sentinel with no ceiling turns a 54 000-title catalogue into an unbounded DOM: nothing is
/// released as it scrolls past, so the cost of the screen grows for as long as the reader keeps
/// going. At the ceiling the window *slides* instead — the page furthest above the viewport is
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
/// Separate from [`Held`] because the window slides *without* asking for anything — releasing a
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
/// reader's current anchor rather than continued at a size the pages before it were never fetched
/// at.
#[derive(Clone, PartialEq)]
pub(super) struct Held {
    pub(super) generation: usize,
    pub(super) filters: DiscoverFilters,
    pub(super) size: usize,
    /// The first page this window ever asked for. Nothing above it was ever rendered, so there is
    /// no measured height to hold its place — that is what `Back to start` is for.
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
    /// the window there — which is how a deep link and the back button reach the same code path.
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

    /// Pages released above the window — the span the spacer has to hold open.
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

    /// Add an answered page at its end of the window, releasing from the other end past the
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
/// is the same whole rows of the same cards, so the mean *is* the height — the fallback only
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
