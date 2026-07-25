//! The shared auto-refresh control: a status pill plus pause/resume and manual refresh.

use crate::i18n::use_i18n;
use crate::views::console::RefreshTick;
use dioxus::prelude::*;

/// Live-refresh status pill plus pause/resume and manual-refresh controls.
#[component]
pub(super) fn LiveControls(tick: RefreshTick, auto: Signal<bool>) -> Element {
    let i18n = use_i18n();
    let is_auto = *auto.read();
    let pill_class = if is_auto { "ik-live on" } else { "ik-live" };
    rsx! {
        div { class: "ik-flex",
            span { class: "{pill_class}",
                span { class: "ik-live-dot" }
                if is_auto {
                    {i18n.args("console.live.on", &[("seconds", &(super::REFRESH_MS / 1000).to_string())])}
                } else {
                    {i18n.t("console.live.paused")}
                }
            }
            button {
                class: "ik-btn",
                onclick: move |_| {
                    let mut a = auto;
                    let cur = *a.peek();
                    a.set(!cur);
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
