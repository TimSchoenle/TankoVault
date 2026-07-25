//! The shared auto-refresh control: a status pill plus pause/resume and manual refresh.

use crate::views::console::RefreshTick;
use dioxus::prelude::*;

/// Live-refresh status pill plus pause/resume and manual-refresh controls.
#[component]
pub(super) fn LiveControls(tick: RefreshTick, auto: Signal<bool>) -> Element {
    let is_auto = *auto.read();
    let pill_class = if is_auto { "ik-live on" } else { "ik-live" };
    rsx! {
        div { class: "ik-flex",
            span { class: "{pill_class}",
                span { class: "ik-live-dot" }
                if is_auto { "Live · 4s" } else { "Paused" }
            }
            button {
                class: "ik-btn",
                onclick: move |_| {
                    let mut a = auto;
                    let cur = *a.peek();
                    a.set(!cur);
                },
                if is_auto { "Pause" } else { "Resume" }
            }
            button {
                class: "ik-btn",
                onclick: move |_| tick.bump(),
                "Refresh"
            }
        }
    }
}
