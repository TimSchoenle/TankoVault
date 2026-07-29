//! Card, section and list chrome shared across the console, the account panels and the series
//! screens.
//!
//! These lived in `views/console/shell.rs` as `pub(super)` items, which made them unreachable
//! from `views/account/` and `views/series/` — so those trees re-derived card and confirm
//! chrome from scratch, and `views/console/{stats,solver}.rs` reached *sideways* into a sibling
//! view to borrow one. `views/` may depend on `components/`; it must never depend on itself.

use crate::components::EmptyBox;
use crate::icons::{Ic, Icon};
use dioxus::prelude::*;

/// A mono uppercase section label, optionally with something right-aligned beside it.
#[component]
pub(crate) fn Section(
    label: String,
    /// Optional right-aligned status beside the label (a validity note, a result pill).
    #[props(default)]
    trailing: Option<Element>,
    children: Element,
) -> Element {
    rsx! {
        div {
            div { class: "ik-flex", style: "align-items:baseline;gap:8px;margin-bottom:9px;",
                span { class: "ik-sec-lbl", "{label}" }
                span { style: "margin-left:auto;display:flex;align-items:center;gap:8px;", {trailing} }
            }
            {children}
        }
    }
}

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

/// The empty inspector: shown until a row is chosen.
#[component]
pub(crate) fn NoSelection(message: String) -> Element {
    rsx! {
        div { class: "ik-cons-pane",
            EmptyBox { message: "{message}" }
        }
    }
}

/// The sidebar-card chrome an account or series panel sits in: an icon + title header, a body.
///
/// `title` arrives already resolved — a panel has its [`crate::i18n::Translator`] to hand and
/// this keeps the chrome free of any opinion about where the words came from.
#[component]
pub(crate) fn PanelCard(icon: Icon, title: String, children: Element) -> Element {
    rsx! {
        div { class: "ik-sidebar-card", style: "max-width:560px;",
            div { class: "ik-flex", style: "margin-bottom:12px;",
                Ic { icon, size: 18 }
                strong { "{title}" }
            }
            {children}
        }
    }
}
