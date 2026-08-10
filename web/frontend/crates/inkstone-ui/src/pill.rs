//! Status decorations: the pill, the filter chip and the state dot.

use crate::skin::{use_skin, Flag, Part, Variant};
use crate::tone::Tone;
use dioxus::prelude::*;

/// A small uppercase status label.
///
/// Decorative by default: the word inside carries the meaning, so nothing is announced twice.
/// Pass `announce` for a pill that is the *only* place a state appears, and it becomes a live
/// `status` region.

#[component]
pub fn Pill(
    #[props(default)] tone: Tone,
    #[props(default)] title: Option<String>,
    #[props(default = false)] announce: bool,
    #[props(default)] class: String,
    #[props(default)] style: Option<String>,
    children: Element,
) -> Element {
    let class = use_skin().class_with(Part::Pill, &[Variant::Tone(tone)], &class);
    rsx! {
        span {
            class,
            style: style.unwrap_or_default(),
            title: title.clone().unwrap_or_default(),
            role: if announce { "status" } else { "" },
            {children}
        }
    }
}

/// A filter chip: a pill-shaped toggle that is part of a set.
#[component]
pub fn Chip(
    label: String,
    active: bool,
    on_toggle: EventHandler<()>,
    /// Amber styling for a chip that narrows to something exceptional (failures, blocked).
    #[props(default = false)]
    warn: bool,
    #[props(default = false)] disabled: bool,
) -> Element {
    let class = use_skin().class(
        Part::Chip,
        &[
            Variant::flag(active, Flag::Active),
            Variant::flag(warn, Flag::Warn),
        ],
    );
    rsx! {
        button {
            class,
            r#type: "button",
            disabled,
            "aria-pressed": if active { "true" } else { "false" },
            onclick: move |_| on_toggle.call(()),
            "{label}"
        }
    }
}

/// A coloured dot for an inline state, with the state's name as its accessible label.
#[component]
pub fn StatusDot(label: String, #[props(default)] tone: Tone) -> Element {
    let class = use_skin().class(Part::StatusDot, &[Variant::Tone(tone)]);
    rsx! {
        span { class, role: "img", "aria-label": "{label}", title: "{label}" }
    }
}
