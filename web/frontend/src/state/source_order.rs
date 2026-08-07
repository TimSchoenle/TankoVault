//! The reader's global provider order, cached app-wide.
//!
//! Fetched once per session rather than per series page: every series screen needs it to decide
//! which source leads, and a preference that changes only when the reader edits it has no
//! business costing a request per navigation.
//!
//! Held as slugs because that is the key both ends already agree on — the source lists the
//! catalogue publishes carry `provider_slug`, and matching on it needs no id table in between.

use dioxus::prelude::*;

/// App-wide provider order, provided via context at the router root.
#[derive(Debug, Clone, Copy)]
pub(crate) struct SourceOrder {
    /// Ranked provider slugs, most preferred first. Empty means "no opinion recorded", which is
    /// also what a signed-out reader has — both resolve by the catalogue's own order.
    inner: Signal<Vec<String>>,
}

impl SourceOrder {
    pub(crate) fn new() -> Self {
        Self {
            inner: Signal::new(Vec::new()),
        }
    }

    /// Adopt a freshly fetched or freshly saved order.
    pub(crate) fn set(self, slugs: Vec<String>) {
        let mut inner = self.inner;
        inner.set(slugs);
    }

    /// Forget the order — on sign-out, or when a fetch fails and the previous answer can no
    /// longer be trusted to describe the current session.
    pub(crate) fn clear(self) {
        self.set(Vec::new());
    }

    /// The ranked slugs, cloned for a comparison that outlives the borrow.
    pub(crate) fn slugs(&self) -> Vec<String> {
        self.inner.read().clone()
    }
}

impl Default for SourceOrder {
    fn default() -> Self {
        Self::new()
    }
}

/// The provider order for any descendant component.
pub(crate) fn use_source_order() -> SourceOrder {
    use_context::<SourceOrder>()
}
