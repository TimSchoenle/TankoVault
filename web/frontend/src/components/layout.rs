//! Owns the card, section and list chrome shared across the console, the account panels and the
//! series screens.

use crate::components::EmptyBox;
use crate::icons::{Ic, Icon};
use dioxus::prelude::*;
use inkstone_ui::{Align, Gap, Hint, Justify, Row};

pub(crate) use inkstone_ui::Section;

/// The list pane's pinned footer: how many rows there are, and the keys that move between them.
#[component]
pub(crate) fn ListFooter(count: String, #[props(default = true)] keys: bool) -> Element {
    let i18n = crate::i18n::use_i18n();
    rsx! {
        div { class: "ik-cons-foot",
            span { "{count}" }
            if keys {
                span { class: "hint", {i18n.t("console.listKeys")} }
            }
        }
    }
}

/// The empty inspector: shown when the list beside it has nothing to open.
#[component]
pub(crate) fn NoSelection(message: String) -> Element {
    rsx! {
        div { class: "ik-cons-pane",
            EmptyBox { message: "{message}" }
        }
    }
}

/// One console panel: a heading, an optional intro under it, and optional controls beside it.
#[component]
pub(crate) fn ConsolePanel(
    title: String,
    #[props(default)] intro: Option<String>,
    #[props(default)] actions: Option<Element>,
    children: Element,
) -> Element {
    rsx! {
        section { class: "ik-mb-lg",
            if let Some(actions) = actions {
                Row { justify: Justify::Between, align: Align::Center, wrap: true,
                    h3 { "{title}" }
                    Row { gap: Gap::Sm, wrap: true, {actions} }
                }
            } else {
                h3 { "{title}" }
            }
            if let Some(intro) = intro {
                Hint { below: Gap::Md, measure: true, "{intro}" }
            }
            {children}
        }
    }
}

/// The panel chrome an account or series card sits in: an icon-and-title header and a body.
#[component]
pub(crate) fn PanelCard(icon: Icon, title: String, children: Element) -> Element {
    rsx! {
        inkstone_ui::Card {
            title,
            class: "ik-panel-form",
            icon: rsx! {
                Ic { icon, size: 18 }
            },
            {children}
        }
    }
}
