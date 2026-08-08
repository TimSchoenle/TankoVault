//! The step-up grant the sensitive screens present.
//!
//! Held in memory beside the access token and for the same reason: it is a bearer credential
//! with a short life, and `localStorage` would put it where an XSS foothold could read it. It is
//! deliberately *not* persisted or re-adopted on boot — an elevation is a statement about the
//! last few minutes, and one that survived a page reload would be a statement about nothing.
//!
//! Cleared whenever the session is, and whenever the server rejects it: a grant the API has
//! stopped honouring is worse than no grant, because the screen would keep retrying with it
//! instead of prompting.

use dioxus::prelude::*;

/// The current elevation, if the reader has confirmed themselves recently.
#[derive(Debug, Clone, Copy)]
pub(crate) struct StepUp {
    token: Signal<Option<String>>,
}

impl StepUp {
    fn new() -> Self {
        Self {
            token: Signal::new(None),
        }
    }

    /// The live grant, or `None` when the next sensitive action must prompt.
    pub(crate) fn token(self) -> Option<String> {
        self.token.read().clone()
    }

    /// Store a freshly earned grant.
    pub(crate) fn set(mut self, token: String) {
        self.token.set(Some(token));
    }

    /// Forget the grant — on sign-out, or after the API refuses it.
    pub(crate) fn clear(mut self) {
        self.token.set(None);
    }
}

/// Provide the elevation. Call once, at the router root, beside [`crate::state::Session`].
pub(crate) fn provide_step_up() {
    use_context_provider(StepUp::new);
}

/// The elevation for any descendant component.
pub(crate) fn use_step_up() -> StepUp {
    use_context::<StepUp>()
}
