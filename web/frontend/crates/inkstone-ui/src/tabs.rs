//! A tab strip with real tablist semantics: roving tabindex and arrow-key navigation.

use crate::skin::{use_skin, Flag, Part, Variant};
use dioxus::prelude::*;

/// One tab: the value selecting it writes back, and the already-translated word for it.
#[derive(Clone, PartialEq, Debug)]
pub struct TabItem<T> {
    pub value: T,
    pub label: String,
}

impl<T> TabItem<T> {
    pub fn new(value: T, label: impl Into<String>) -> Self {
        Self {
            value,
            label: label.into(),
        }
    }
}

/// A tab strip.
///
/// Controlled: the caller owns the selection, because it is usually a URL parameter, and a
/// signal here would hold a second copy of it. Generic over the selection type so the caller
/// keeps its enum rather than matching on strings.
#[component]
pub fn TabBar<T: Clone + PartialEq + 'static>(
    items: Vec<TabItem<T>>,
    selected: T,
    on_select: EventHandler<T>,
    /// Names the strip for a screen reader, which otherwise announces an unlabelled tablist.
    #[props(default)]
    label: Option<String>,
    /// Flush against an inspector's edge rather than a page's.
    #[props(default = false)]
    flush: bool,
) -> Element {
    let skin = use_skin();
    let index = items.iter().position(|item| item.value == selected);
    let keyed = items.clone();

    rsx! {
        div {
            class: skin.class(Part::Tabs, &[Variant::flag(flush, Flag::Flush)]),
            role: "tablist",
            "aria-label": label.clone().unwrap_or_default(),
            onkeydown: move |event| {
                let Some(at) = index else { return };
                let last = keyed.len().saturating_sub(1);
                let next = match event.key() {
                    Key::ArrowLeft => if at == 0 { last } else { at - 1 },
                    Key::ArrowRight => if at == last { 0 } else { at + 1 },
                    Key::Home => 0,
                    Key::End => last,
                    _ => return,
                };
                // Suppress the browser's own scroll-by-arrow-key behaviour.
                event.prevent_default();
                on_select.call(keyed[next].value.clone());
            },
            for item in items {
                button {
                    key: "{item.label}",
                    class: skin.class(Part::Tab, &[Variant::flag(item.value == selected, Flag::Active)]),
                    r#type: "button",
                    role: "tab",
                    "aria-selected": if item.value == selected { "true" } else { "false" },
                    // Roving tabindex: only the current tab is a Tab stop.
                    tabindex: if item.value == selected { "0" } else { "-1" },
                    onclick: move |_| on_select.call(item.value.clone()),
                    "{item.label}"
                }
            }
        }
    }
}
