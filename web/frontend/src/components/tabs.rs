//! Shared tab strip with ARIA tablist semantics (roving tabindex, arrow-key navigation) used by
//! Account, Notifications and Console screens.

use crate::i18n::use_i18n;
use dioxus::prelude::*;

/// A closed set of tabs: what they are, and the catalogue key wording each one.
pub(crate) trait TabKind: Copy + PartialEq + 'static {
    /// Every tab this kind defines, in strip order.
    fn all() -> &'static [Self]
    where
        Self: Sized;

    /// The catalogue key of this tab's label.
    fn label_key(self) -> &'static str;
}

/// A tab strip with real tab semantics.
///
/// `visible` restricts the strip to a subset — Account hides panels a reader has no capability
/// for, and rendering a tab that opens nothing is worse than omitting it.
#[component]
pub(crate) fn TabBar<T: TabKind + Clone + PartialEq + 'static>(
    selected: Signal<T>,
    #[props(default)] visible: Option<Vec<T>>,
    /// `ik-tabs flush` + the console's top margin, for strips that sit inside an inspector.
    #[props(default = false)]
    flush: bool,
) -> Element {
    let i18n = use_i18n();
    let mut selected = selected;
    let entries = visible.unwrap_or_else(|| T::all().to_vec());
    let current = *selected.read();

    // Resolved once so the key handler can move by position without re-reading the list.
    let index = entries.iter().position(|entry| *entry == current);
    let keyed = entries.clone();

    rsx! {
        div {
            class: if flush { "ik-tabs flush" } else { "ik-tabs" },
            style: if flush { "margin-top:14px;" } else { "" },
            role: "tablist",
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
                // Suppress the browser's own scroll-by-arrow-key behavior.
                event.prevent_default();
                selected.set(keyed[next]);
            },
            for entry in entries {
                button {
                    key: "{entry.label_key()}",
                    class: if entry == current { "ik-tab active" } else { "ik-tab" },
                    r#type: "button",
                    role: "tab",
                    "aria-selected": if entry == current { "true" } else { "false" },
                    // Roving tabindex: only the current tab is a Tab stop.
                    tabindex: if entry == current { "0" } else { "-1" },
                    onclick: move |_| selected.set(entry),
                    {i18n.t(entry.label_key())}
                }
            }
        }
    }
}
