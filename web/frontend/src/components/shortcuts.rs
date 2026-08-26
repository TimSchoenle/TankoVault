//! The `?` reference: every keyboard binding the app has, one table per screen.
//!
//! The rows are handed in by the screen that owns the bindings rather than written here, so this
//! stays a renderer — a screen's keydown handler and the table describing it read the same list.

use crate::i18n::use_i18n;
use dioxus::prelude::*;
use inkstone_ui::{Button, Modal, ModalSize, Section, Table, TableColumn};
use std::rc::Rc;

/// One binding, as the overlay prints it.
#[derive(Clone, PartialEq, Eq, Debug)]
pub(crate) struct ShortcutRow {
    /// The chords that run it. More than one means *either*, not both together.
    pub(crate) chords: Vec<String>,
    pub(crate) description: String,
}

/// One screen's bindings.
#[derive(Clone, PartialEq, Eq, Debug)]
pub(crate) struct ShortcutGroup {
    /// What the screen is called, as its own table's heading.
    pub(crate) screen: String,
    pub(crate) rows: Vec<ShortcutRow>,
}

/// The shortcut reference: a dialog, so `Escape` closes it and focus is inside it while it is up.
#[component]
pub(crate) fn ShortcutsOverlay(
    groups: Vec<ShortcutGroup>,
    /// Focused again on close, so `?` then `Escape` puts the keyboard back where it started.
    /// A mounted-element handle rather than a DOM id, because the desktop build has no
    /// `web-sys` to look one up with (see [`crate::components::FocusTargets`]).
    #[props(default)]
    return_focus: Option<Signal<Option<Rc<MountedData>>>>,
    on_close: EventHandler<()>,
) -> Element {
    let i18n = use_i18n();
    let close = use_callback(move |()| {
        if let Some(element) = return_focus.and_then(|target| target.peek().clone()) {
            spawn(async move {
                // Best-effort: a refused call (the opener unmounted under the dialog) is not
                // worth surfacing over a dialog that is closing anyway.
                let _ = element.set_focus(true).await;
            });
        }
        on_close.call(());
    });

    rsx! {
        Modal {
            title: i18n.t("shortcuts.title"),
            size: ModalSize::Wide,
            intro: Some(i18n.t("shortcuts.intro")),
            // It only presents, so a click on the scrim is a way out rather than a lost answer.
            dismiss_on_backdrop: true,
            on_close: move |()| close.call(()),
            footer: Some(rsx! {
                Button { on_click: move |_| close.call(()), {i18n.t("common.close")} }
            }),
            for group in groups {
                {
                    let ShortcutGroup { screen, rows } = group;
                    rsx! {
                        Section { label: screen.clone(),
                            Table {
                                caption: screen,
                                columns: vec![
                                    TableColumn::new(i18n.t("shortcuts.col.keys")).width("34%"),
                                    TableColumn::new(i18n.t("shortcuts.col.action")),
                                ],
                                for ShortcutRow { chords, description } in rows {
                                    tr {
                                        td {
                                            for chord in chords {
                                                kbd { class: "ik-mono ik-kbd", "{chord}" }
                                            }
                                        }
                                        td { "{description}" }
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    /// The overlay's own wording is the one part of it no screen supplies, so nothing else would
    /// notice a missing key — it would render the key itself as a column heading.
    #[test]
    fn the_overlays_own_wording_is_in_the_catalogue() {
        for key in [
            "shortcuts.title",
            "shortcuts.intro",
            "shortcuts.col.keys",
            "shortcuts.col.action",
            "common.close",
        ] {
            assert!(crate::i18n::has_key(key), "{key} is not in the catalogue");
        }
    }
}
