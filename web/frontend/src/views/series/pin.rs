//! The per-series source pin: what it is now, and the writes that persist it.

use crate::api::{self, Api};
use crate::hooks::{Outcome, Reload};
use crate::i18n::Translator;
use crate::models::{SeriesId, SeriesSourceId, SourcePin};
use dioxus::prelude::*;

/// A reader's pinned source for one series, and the two writes that move it.
///
/// A handle rather than a bare `Signal` because the pin is server state now: it rides the
/// watchlist entry, so it follows the reader to their other devices — which the browser-local
/// pin this replaced never did. `Copy`, so every component between the page and the source menu
/// forwards it exactly as it forwarded the signal.
#[derive(Clone, Copy)]
pub(super) struct Pinned {
    /// What the screen currently shows. Seeded from the loaded entry and moved optimistically,
    /// so the menu responds before the round trip lands.
    current: Signal<Option<SeriesSourceId>>,
    series_id: SeriesId,
    api: Api,
    i18n: Translator,
    outcome: Signal<Outcome>,
    reload: Reload,
    /// Whether the reader tracks this series. The pin is a column on the watchlist entry, so
    /// an untracked series has nowhere to keep one and the control is offered as unavailable
    /// rather than failing on click.
    tracked: bool,
}

/// Compared by what can actually differ between two renders of the same screen: which signal
/// holds the pin, which series it belongs to, and whether pinning is offered. The API handle,
/// translator, outcome slot and reload token are screen-scoped and never change identity — and
/// none of them is `PartialEq` — so folding them in would be neither possible nor meaningful.
/// Dioxus needs this to decide whether a child's props moved.
impl PartialEq for Pinned {
    fn eq(&self, other: &Self) -> bool {
        self.current == other.current
            && self.series_id == other.series_id
            && self.tracked == other.tracked
    }
}

impl Pinned {
    pub(super) const fn new(
        current: Signal<Option<SeriesSourceId>>,
        series_id: SeriesId,
        api: Api,
        i18n: Translator,
        outcome: Signal<Outcome>,
        reload: Reload,
        tracked: bool,
    ) -> Self {
        Self {
            current,
            series_id,
            api,
            i18n,
            outcome,
            reload,
            tracked,
        }
    }

    pub(super) fn current(self) -> Option<SeriesSourceId> {
        *self.current.read()
    }

    /// Whether pinning is offered at all — see [`Self::tracked`].
    pub(super) const fn is_available(self) -> bool {
        self.tracked
    }

    /// Pin `source` for this series.
    pub(super) fn set(self, source: SeriesSourceId) {
        let client = self.api.client();
        self.write(Some(source), async move {
            client
                .put_source_pin()
                .series_id(self.series_id)
                .body(SourcePin {
                    series_source_id: source,
                })
                .send()
                .await
                .map(|_| ())
        });
    }

    /// Drop the pin, returning this series to the reader's global source order.
    pub(super) fn clear(self) {
        let client = self.api.client();
        self.write(None, async move {
            client
                .delete_source_pin()
                .series_id(self.series_id)
                .send()
                .await
                .map(|_| ())
        });
    }

    /// Move the signal now, send the write, and put the signal back if the write is refused.
    ///
    /// Reverting matters more here than on a toggle: the pin decides which link every `Open`
    /// button on the screen points at, so a failed write that left the optimistic value in
    /// place would send the reader to a source the server does not agree they chose.
    fn write<F, E>(self, next: Option<SeriesSourceId>, request: F)
    where
        F: std::future::Future<Output = Result<(), progenitor_client::Error<E>>> + 'static,
    {
        let mut current = self.current;
        let previous = *current.peek();
        current.set(next);
        let mut outcome = self.outcome;
        outcome.set(None);
        let (i18n, reload) = (self.i18n, self.reload);
        spawn(async move {
            match request.await {
                // The entry carries the pin, so the refetch is what makes the rest of the page
                // agree with the write rather than with the optimistic guess.
                Ok(()) => reload.bump(),
                Err(e) => {
                    current.set(previous);
                    outcome.set(Some(Err(api::friendly_error(i18n, e))));
                }
            }
        });
    }
}
