//! The console's live-stream control: a connection-state pill plus detach/attach and a manual
//! refresh.
//!
//! The pill reports the *connection*, not a timer. An operator staring at a queue that has
//! stopped moving has to be able to tell whether it is quiet or whether the stream is gone.

use crate::i18n::use_i18n;
use crate::views::console::live::LiveState;
use crate::views::console::RefreshTick;
use dioxus::prelude::*;

/// Live-stream status pill plus detach/attach and manual-refresh controls.
#[component]
pub(super) fn LiveControls(
    tick: RefreshTick,
    auto: Signal<bool>,
    state: Signal<LiveState>,
) -> Element {
    let i18n = use_i18n();
    let is_auto = *auto.read();
    let connection = *state.read();
    let pill_class = if connection.is_current() {
        "ik-live on"
    } else {
        "ik-live"
    };
    rsx! {
        div { class: "ik-flex",
            span {
                class: "{pill_class}",
                // The one place the console reports its own staleness, so it is announced
                // rather than left to the colour of a dot.
                "aria-live": "polite",
                span { class: "ik-live-dot" }
                {i18n.t(connection.label_key())}
            }
            button {
                class: "ik-btn",
                onclick: move |_| {
                    let mut a = auto;
                    let next = !*a.peek();
                    a.set(next);
                    // Persisted: an operator who detaches to read a queue should not have it
                    // start moving again on the next reload.
                    crate::state::prefs::set_console_live(next);
                },
                if is_auto {
                    {i18n.t("console.live.pause")}
                } else {
                    {i18n.t("console.live.resume")}
                }
            }
            button {
                class: "ik-btn",
                onclick: move |_| tick.bump(),
                {i18n.t("console.live.refresh")}
            }
        }
    }
}
