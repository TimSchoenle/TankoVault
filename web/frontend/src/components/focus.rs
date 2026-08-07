//! Handles to the two text fields that something on another screen focuses.
//!
//! Both used to be reached by DOM id through `web-sys`, which the desktop build has no
//! equivalent for. A mounted-element handle is the renderer's own answer and works on either,
//! so the shortcut behaves the same in both — and neither side hard-codes a string the other
//! could rename.

use dioxus::prelude::*;
use std::rc::Rc;

/// Provided once at the app root; each field registers itself on mount.
///
/// `None` until the owning screen is on display, which is the normal state — the console's jump
/// button and the watchlist's `/` are both rendered beside the field they focus, but the top bar
/// is not mounted on every route.
#[derive(Clone, Copy)]
pub(crate) struct FocusTargets {
    /// The top bar's search box.
    pub(crate) search: Signal<Option<Rc<MountedData>>>,
    /// The watchlist toolbar's filter box.
    pub(crate) filter: Signal<Option<Rc<MountedData>>>,
}

impl FocusTargets {
    pub(crate) fn new() -> Self {
        Self {
            search: Signal::new(None),
            filter: Signal::new(None),
        }
    }
}

pub(crate) fn use_focus_targets() -> FocusTargets {
    use_context()
}

/// Focus `target` and select what is already in it, so the next keystroke replaces the previous
/// query rather than appending to it.
///
/// Selecting is best-effort and web-only — see [`crate::platform::select_focused_text`]. It runs
/// only once focus has actually landed, because it acts on whatever is focused: after a failed
/// `set_focus` that would be some unrelated field.
pub(crate) fn focus_and_select(target: Signal<Option<Rc<MountedData>>>) {
    let Some(element) = target.peek().clone() else {
        return;
    };
    spawn(async move {
        if element.set_focus(true).await.is_ok() {
            crate::platform::select_focused_text();
        }
    });
}
