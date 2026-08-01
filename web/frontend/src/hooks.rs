//! Small reactive helpers shared by the screens.

use dioxus::prelude::*;

/// A refetch trigger shared between a `use_resource` and the handlers that invalidate it.
///
/// [`Reload::track`] must be called synchronously at the top of the resource closure — it looks
/// like a discardable read, but removing it silently stops the screen refreshing after a mutation.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct Reload(Signal<u32>);

impl Reload {
    /// Subscribe the calling reactive scope. Call once at the top of a `use_resource`
    /// closure, in its synchronous part.
    pub(crate) fn track(self) {
        let _ = self.0.read();
    }

    /// Invalidate every resource tracking this handle, causing them to refetch.
    pub(crate) fn bump(mut self) {
        self.0 += 1;
    }
}

/// Create a [`Reload`] handle scoped to the calling component.
pub(crate) fn use_reload() -> Reload {
    Reload(use_signal(|| 0u32))
}

/// A boolean latch for "an action is in flight", with re-entry already handled — a missed guard
/// here means a double click fires the mutation twice.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct Busy(Signal<bool>);

impl Busy {
    /// Claim the latch. Returns `false` when an action is already running, in which case the
    /// caller must do nothing.
    #[must_use]
    pub(crate) fn claim(mut self) -> bool {
        if *self.0.peek() {
            return false;
        }
        self.0.set(true);
        true
    }

    /// Release the latch once the action settles.
    pub(crate) fn release(mut self) {
        self.0.set(false);
    }

    /// Whether an action is in flight — for disabling the control that started it.
    pub(crate) fn is_busy(self) -> bool {
        *self.0.read()
    }
}

/// Create a [`Busy`] latch scoped to the calling component.
pub(crate) fn use_busy() -> Busy {
    Busy(use_signal(|| false))
}

/// The `Ok`/`Err` message shown under a form after a mutation settles.
pub(crate) type Outcome = Option<Result<String, String>>;

/// Create an [`Outcome`] slot scoped to the calling component.
pub(crate) fn use_outcome() -> Signal<Outcome> {
    use_signal(|| None)
}
