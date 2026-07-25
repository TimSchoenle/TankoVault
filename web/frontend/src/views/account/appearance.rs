//! Appearance panel (`DESIGN_SPEC` §8) — theme, accent, density and cover style.
//!
//! Every knob is real: each writes a `data-*` attribute on `<html>` that swaps a block of CSS
//! custom properties, and persists a `tv-*` key that `index.html` re-applies before the next
//! first paint. Selecting a default clears the override so the stylesheet's own value wins.

use super::PanelCard;
use crate::icons::Icon;
use crate::state::prefs::{Knob, ACCENT, COVER, DENSITY, THEME};
use dioxus::prelude::*;

#[component]
pub(crate) fn AppearancePanel() -> Element {
    let theme = use_signal(|| THEME.default.to_owned());
    let accent = use_signal(|| ACCENT.default.to_owned());
    let density = use_signal(|| DENSITY.default.to_owned());
    let cover = use_signal(|| COVER.default.to_owned());

    // Seed each control from what is actually applied, so the panel opens showing the
    // reader's real settings rather than the defaults.
    use_effect(move || {
        THEME.load(theme);
        ACCENT.load(accent);
        DENSITY.load(density);
        COVER.load(cover);
    });

    rsx! {
        PanelCard { icon: Icon::Settings, title: "Appearance",
            p { class: "ik-muted", style: "font-size:13px;margin-top:0;",
                "Tune the reading environment. Your choices are remembered on this device."
            }
            KnobGroup {
                title: "Theme",
                knob: THEME,
                value: theme,
                options: vec![("dark", "Inkstone Dark"), ("light", "Warm Paper")],
            }
            KnobGroup {
                title: "Accent",
                knob: ACCENT,
                value: accent,
                options: vec![
                    ("vermilion", "Vermilion"),
                    ("amber", "Amber"),
                    ("jade", "Jade"),
                    ("azure", "Azure"),
                    ("amethyst", "Amethyst"),
                ],
            }
            KnobGroup {
                title: "Density",
                knob: DENSITY,
                value: density,
                options: vec![("cozy", "Cozy"), ("standard", "Standard"), ("compact", "Compact")],
            }
            KnobGroup {
                title: "Cover style",
                knob: COVER,
                value: cover,
                options: vec![("ink", "Ink"), ("duotone", "Duotone"), ("vivid", "Vivid")],
            }
        }
    }
}

/// One labelled row of mutually-exclusive appearance chips.
#[component]
fn KnobGroup(
    title: &'static str,
    knob: Knob,
    value: Signal<String>,
    options: Vec<(&'static str, &'static str)>,
) -> Element {
    let current = value.read().clone();
    rsx! {
        div { style: "margin-top:16px;",
            div { class: "ik-subhead", style: "margin-bottom:8px;", "{title}" }
            div { class: "ik-chips", style: "margin-bottom:0;",
                for (option , label) in options {
                    button {
                        key: "{option}",
                        class: if current == option { "ik-chip active" } else { "ik-chip" },
                        "aria-pressed": current == option,
                        onclick: move |_| knob.apply(value, option),
                        "{label}"
                    }
                }
            }
        }
    }
}
