//! A tab strip with real tablist semantics: roving tabindex and arrow-key navigation.

use crate::skin::{use_skin, Flag, Part, Variant};
use dioxus::prelude::*;

/// One tab: the value selecting it writes back, the already-translated word for it, and the
/// optional count and trailing placement some strips need.
#[derive(Clone, PartialEq, Debug)]
pub struct TabItem<T> {
    pub value: T,
    pub label: String,
    pub count: Option<String>,
    pub apart: bool,
}

impl<T> TabItem<T> {
    pub fn new(value: T, label: impl Into<String>) -> Self {
        Self {
            value,
            label: label.into(),
            count: None,
            apart: false,
        }
    }

    /// A count beside the label, already formatted — the kit has no locale.
    #[must_use]
    pub fn count(mut self, count: impl Into<String>) -> Self {
        self.count = Some(count.into());
        self
    }

    /// Push it to the trailing edge, set off from the tabs before it.
    #[must_use]
    pub fn apart(mut self) -> Self {
        self.apart = true;
        self
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
    /// Keep the strip on one row that scrolls sideways, with a fade at the trailing edge, for
    /// a set too long to wrap gracefully.
    #[props(default = false)]
    scroll: bool,
) -> Element {
    let skin = use_skin();
    let index = items.iter().position(|item| item.value == selected);
    let keyed = items.clone();
    let shell = skin.class(Part::TabsScroll, &[]);

    let strip = rsx! {
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
            // Deliberately unkeyed. The strip is positional — a caller varies its *length*
            // (Account drops the panels a reader has no capability for), never its order — so
            // positional diffing is already correct. This keyed on `item.label` until a duplicate
            // key elsewhere in the app aborted the desktop build inside `dioxus-core`'s keyed
            // diff: a human-readable label is not an identity, and `T` carries no bound that
            // would yield one, so the safe key is no key.
            for item in items {
                button {
                    class: skin.class(
                        Part::Tab,
                        &[
                            Variant::flag(item.value == selected, Flag::Active),
                            Variant::flag(item.apart, Flag::Apart),
                        ],
                    ),
                    r#type: "button",
                    role: "tab",
                    "aria-selected": if item.value == selected { "true" } else { "false" },
                    // Roving tabindex: only the current tab is a Tab stop.
                    tabindex: if item.value == selected { "0" } else { "-1" },
                    onclick: move |_| on_select.call(item.value.clone()),
                    "{item.label}"
                    if let Some(count) = item.count {
                        span { class: skin.class(Part::TabCount, &[]), "{count}" }
                    }
                }
            }
        }
    };

    if scroll {
        rsx! {
            div { class: shell, {strip} }
        }
    } else {
        strip
    }
}

#[cfg(test)]
mod tests {
    /// The kit's tabs grew a count and a scrolling shell so the watchlist's `.ik-wl-tabcount`
    /// and Account's `.ik-subnav` could stop being a second and third tab idiom. A modifier the
    /// stylesheet never defines is the failure this kit exists to prevent — it compiles,
    /// renders, and draws the count as a bare number in the label's own type.
    #[test]
    fn the_tab_extras_are_drawn() {
        let css = include_str!("../styles/inkstone.css");
        for class in [".ik-tab-count", ".ik-tabs-scroll", ".ik-tab.apart"] {
            assert!(
                css.contains(class),
                "`{class}` is emitted by `TabBar` but no rule defines it"
            );
        }
    }
}
