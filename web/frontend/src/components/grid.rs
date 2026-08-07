//! Sizing a cover grid's page to the window it is shown in.
//!
//! A fixed page leaves a ragged last row at every width but the one it was picked for: 24 covers
//! across seven columns is three full rows and three lonely cards beside four empty cells. The
//! size is therefore derived from the grid's own geometry — whole rows only, and bounded, so a
//! wide window cannot turn one screen of covers into an unbounded query.
//!
//! The geometry is measured rather than assumed. `--card` and `--gap` decide how many columns
//! `auto-fill` produces, and both move with the density preference and the narrow-viewport media
//! queries; Rust cannot read a custom property, because `getComputedStyle` exists only in the
//! browser build and `eval` is banned (see `crate::platform`). [`GridFitProbe`] therefore renders
//! the two values as the widths of two hidden boxes and reads them back through the same
//! `ResizeObserver` that reports the grid's own width, which keeps the stylesheet the only place
//! they are written. The one rule this cannot see is a media query that *replaces* the track list
//! rather than retuning the tokens — there is one, under 400 px, and the test named after it says
//! what keeps it harmless.

use crate::platform;
use dioxus::prelude::*;

/// Smallest page a fitted grid asks for. Under this a narrow window spends a round trip on a
/// handful of covers and then pages constantly.
const MIN_ITEMS: usize = 12;

/// Largest page a fitted grid asks for, however wide the window.
///
/// The catalogue endpoint clamps `limit` to 100 and the watchlist to 200; this stays well under
/// both on purpose. A page size multiplies with the column count, so without a ceiling an
/// ultrawide window would make one screen of covers the most expensive query the catalogue can
/// serve. The one thing that outranks it is a whole row: at more columns than this a single row
/// is still the answer, because asking for less than one row *is* the gap this module removes.
const MAX_ITEMS: usize = 60;

/// Covers a skeleton draws before the first measurement lands.
const FALLBACK_ITEMS: usize = 12;

/// Ceiling on the derived column count. Far past any real window — 64 covers across is some
/// 10 000 px at the narrowest card — so it only ever catches a nonsense measurement.
const MAX_COLUMNS: f64 = 64.0;

/// How long the column count must hold still before a fetch is resized to it. Dragging a window
/// edge sweeps through several counts; only the one the reader stops on is worth a request.
const SETTLE_MS: u32 = 250;

/// The measured geometry of one cover grid, and the page size derived from it.
///
/// Create it with [`use_grid_fit`] or [`use_grid_fill`], render a [`GridFitProbe`] in the column
/// the grid occupies, and gate the fetch on [`GridFit::page_size`].
#[derive(Clone, Copy, PartialEq)]
pub(crate) struct GridFit {
    /// Rows of covers one page should cover. `usize::MAX` means "as many as [`MAX_ITEMS`] allows".
    rows: usize,
    /// The raw measurements, as they arrive.
    metrics: Signal<Metrics>,
    /// The column count fetches are currently sized for — [`Metrics::columns`] after settling.
    /// `None` until the first measurement, which is what [`GridFit::page_size`] reports as
    /// "do not fetch yet".
    columns: Signal<Option<usize>>,
}

/// The three lengths that decide the column count, in CSS pixels: the width the grid is laid out
/// in, and the `minmax()` floor and gutter its tracks are built from.
#[derive(Clone, Copy, Default)]
struct Metrics {
    width: f64,
    card: f64,
    gap: f64,
}

/// Which of [`Metrics`]' lengths a given probe box carries.
#[derive(Clone, Copy)]
enum Metric {
    Width,
    Card,
    Gap,
}

impl Metrics {
    /// How many columns `repeat(auto-fill, minmax(card, 1fr))` yields at this width.
    ///
    /// This is the layout engine's own arithmetic, so the count matches what the reader sees;
    /// `None` means nothing has been measured yet.
    fn columns(self) -> Option<usize> {
        if self.width <= 0.0 || self.card <= 0.0 {
            return None;
        }
        let fitted = (self.width + self.gap) / (self.card + self.gap);
        if !fitted.is_finite() {
            return None;
        }
        #[expect(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "clamped into 1..=MAX_COLUMNS first, and truncation is the floor auto-fill does"
        )]
        Some(fitted.clamp(1.0, MAX_COLUMNS) as usize)
    }
}

impl GridFit {
    /// Covers the next fetch should ask for, or `None` while the grid has not been measured.
    ///
    /// A screen must not fetch on `None` — [`unmeasured`] is what to await instead.
    pub(crate) fn page_size(self) -> Option<usize> {
        Some(fitted_page_size((*self.columns.read())?, self.rows))
    }

    /// [`GridFit::page_size`] with the pre-measurement fallback applied, for the parts of a
    /// render that need a number rather than a decision: the skeleton's cover count, and the
    /// page arithmetic under the grid.
    pub(crate) fn page_size_or_default(self) -> usize {
        self.page_size().unwrap_or(FALLBACK_ITEMS)
    }

    /// Record one probe box's width.
    fn observe(mut self, metric: Metric, event: &Event<ResizeData>) {
        let Ok(size) = event.get_content_box_size() else {
            return;
        };
        let mut metrics = *self.metrics.peek();
        match metric {
            Metric::Width => metrics.width = size.width,
            Metric::Card => metrics.card = size.width,
            Metric::Gap => metrics.gap = size.width,
        }
        self.metrics.set(metrics);
        self.settle();
    }

    /// Adopt the measured column count once it stops moving.
    fn settle(mut self) {
        let Some(columns) = self.metrics.peek().columns() else {
            return;
        };
        if *self.columns.peek() == Some(columns) {
            return;
        }
        if self.columns.peek().is_none() {
            // Nothing has been fetched yet — screens park their first request on this — so the
            // first measurement is adopted at once rather than costing the reader a settling
            // delay before anything loads at all.
            self.columns.set(Some(columns));
            return;
        }
        let metrics = self.metrics;
        let mut applied = self.columns;
        spawn(async move {
            platform::sleep_ms(SETTLE_MS).await;
            // Only the count a drag ended on passes both checks: every task queued earlier finds
            // either that the metrics moved on or that the count is already applied.
            if metrics.peek().columns() == Some(columns) && *applied.peek() != Some(columns) {
                applied.set(Some(columns));
            }
        });
    }
}

/// A [`GridFit`] for a grid that pages: one page is `rows` rows of covers, bounded by
/// [`MIN_ITEMS`] and [`MAX_ITEMS`].
pub(crate) fn use_grid_fit(rows: usize) -> GridFit {
    GridFit {
        rows,
        metrics: use_signal(Metrics::default),
        columns: use_signal(|| None),
    }
}

/// A [`GridFit`] for a grid with no pager, where the page *is* the result set: as many whole rows
/// as [`MAX_ITEMS`] allows.
pub(crate) fn use_grid_fill() -> GridFit {
    use_grid_fit(usize::MAX)
}

/// Park a fetch until the grid has been measured.
///
/// The resource stays in the loading state the reader is already looking at, and re-runs the
/// moment [`GridFitProbe`]'s first measurement lands. Guessing a size instead would double the
/// screen's traffic: the guess is wrong at most widths, and the correction is a second, identical
/// query against the same endpoint.
pub(crate) async fn unmeasured<T>() -> T {
    std::future::pending().await
}

/// The hidden boxes a [`GridFit`] measures: its own width is the width the grid is laid out in,
/// and its two children carry `--card` and `--gap`.
///
/// Render it as a sibling of the grid, inside the same column, and **keep it mounted while the
/// grid is still a skeleton** — the first measurement is what releases the fetch, so a probe that
/// only appears alongside results would deadlock the screen it belongs to.
///
/// `tiles` selects the watchlist's smaller cover tile (`--tile`) over the catalogue card.
#[component]
pub(crate) fn GridFitProbe(fit: GridFit, #[props(default = false)] tiles: bool) -> Element {
    let class = if tiles {
        "ik-fit-probe tiles"
    } else {
        "ik-fit-probe"
    };
    rsx! {
        div {
            class: "{class}",
            "aria-hidden": "true",
            onresize: move |event| fit.observe(Metric::Width, &event),
            span { class: "card", onresize: move |event| fit.observe(Metric::Card, &event) }
            span { class: "gap", onresize: move |event| fit.observe(Metric::Gap, &event) }
        }
    }
}

/// Keep a paginated grid pointing at roughly the same series when its page size changes.
///
/// The page index is expressed in units of the page size, so a size that moves under it moves
/// the reader: page 12 of a 24-cover page is series 288, but page 12 of a 48-cover page is
/// series 576 — often past the end of the result set, which renders as "no matches" for a filter
/// that matched a moment ago. Opening the filter panel is enough to trigger it.
pub(crate) fn use_page_rescale(fit: GridFit, mut page: Signal<usize>) {
    let mut applied = use_signal(|| Option::<usize>::None);
    use_effect(move || {
        let Some(size) = fit.page_size() else {
            return;
        };
        let previous = *applied.peek();
        applied.set(Some(size));
        let Some(previous) = previous else {
            return;
        };
        let current = *page.peek();
        if previous != size && current > 0 {
            page.set(current.saturating_mul(previous) / size);
        }
    });
}

/// Covers one page holds: whole rows of `columns`, never under [`MIN_ITEMS`] and never over
/// [`MAX_ITEMS`].
///
/// The bounds are applied by adding or dropping whole rows, never by truncating the count to the
/// bound — a truncated count is exactly the ragged last row this exists to remove.
fn fitted_page_size(columns: usize, rows: usize) -> usize {
    let columns = columns.max(1);
    let rows = rows
        .max(MIN_ITEMS.div_ceil(columns))
        .min((MAX_ITEMS / columns).max(1));
    columns.saturating_mul(rows)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Column counts the loops below sweep. Past the real ceiling on purpose.
    const MAX_COLUMNS_TESTED: usize = 80;

    /// The defect the module exists for: a fixed page of 24 across a seven-column grid renders
    /// three full rows and a last row of three, with four empty cells beside it. Whatever the
    /// bounds do, a page has to divide by the column count.
    #[test]
    fn every_page_is_whole_rows() {
        for columns in 1..=MAX_COLUMNS_TESTED {
            for rows in [1, 2, 4, 8, 12, usize::MAX] {
                let size = fitted_page_size(columns, rows);
                assert_eq!(size % columns, 0, "{size} covers across {columns} columns");
            }
        }
    }

    /// The bounds hold in both directions, so a phone does not page every four covers and an
    /// ultrawide window does not ask the catalogue for hundreds of rows at once. Above
    /// [`MAX_ITEMS`] columns one whole row wins over the ceiling, which is the documented
    /// exception.
    #[test]
    fn bounds_hold() {
        for columns in 1..=MAX_COLUMNS_TESTED {
            for rows in [1, 2, 4, 8, 12, usize::MAX] {
                let size = fitted_page_size(columns, rows);
                assert!(size >= MIN_ITEMS.min(columns), "{size} under the floor");
                assert!(size <= MAX_ITEMS.max(columns), "{size} over the ceiling");
            }
        }
    }

    /// Under 400 px the stylesheet pins the catalogue grid to two columns outright rather than
    /// retuning `--card`, and a replaced track list is the one thing the probe cannot report: the
    /// measured width yields one column where the grid renders two. What phones get is therefore
    /// the floor, and the floor has to divide by two or that override brings the ragged row back.
    #[test]
    fn the_floor_survives_the_two_column_phone_override() {
        assert_eq!(fitted_page_size(1, 1) % 2, 0);
        assert_eq!(fitted_page_size(1, usize::MAX) % 2, 0);
    }

    /// A grid with no pager asks for as much as the ceiling allows rather than a fixed number of
    /// rows — dropping to one screenful on a narrow window would silently hide results a reader
    /// has no other way to reach.
    #[test]
    fn a_filling_grid_uses_the_ceiling() {
        assert_eq!(fitted_page_size(2, usize::MAX), 60);
        assert_eq!(fitted_page_size(7, usize::MAX), 56);
    }

    /// The column count must be the layout engine's, not an approximation of it: 1512 px of
    /// results column at the default 190 px card and 18 px gutter is seven columns, and the
    /// gutter is what decides it — dropping it from the arithmetic yields eight.
    #[test]
    fn columns_follow_auto_fill() {
        let metrics = Metrics {
            width: 1512.0,
            card: 190.0,
            gap: 18.0,
        };
        assert_eq!(metrics.columns(), Some(7));
    }

    /// An unmeasured, half-measured or nonsensical probe reports no column count at all, so the
    /// screen keeps waiting instead of fetching a page sized from a zero.
    #[test]
    fn nothing_is_derived_from_an_incomplete_measurement() {
        assert_eq!(Metrics::default().columns(), None);
        assert_eq!(
            Metrics {
                width: 1512.0,
                card: 0.0,
                gap: 18.0
            }
            .columns(),
            None
        );
        assert_eq!(
            Metrics {
                width: 0.0,
                card: 190.0,
                gap: 18.0
            }
            .columns(),
            None
        );
    }
}
