//! The modal dialog.
//!
//! Rendered inline rather than through a portal: this kit has to work in a wry webview as well
//! as a browser, and the app that owns it forbids `document::eval`, so there is no DOM escape
//! hatch to move a node with. A scrim with its own stacking context does the same job.

use crate::skin::{use_skin, Flag, Part, Variant};
use dioxus::prelude::*;

/// How wide the dialog is.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum ModalSize {
    /// A question with one answer — a confirmation, a code.
    #[default]
    Compact,
    /// A form.
    Wide,
}

impl ModalSize {
    fn variant(self) -> Variant {
        Variant::flag(matches!(self, Self::Wide), Flag::Wide)
    }
}

/// A modal dialog.
///
/// Takes focus on mount and closes on `Escape`. `dismiss_on_backdrop` defaults to *false* on
/// purpose: a dialog that is answered by typing — a code, a password — loses a half-entered
/// answer *and* the action waiting behind it to one stray click on the scrim. Turn it on for
/// dialogs that only present.
#[component]
pub fn Modal(
    /// Names the dialog for assistive technology and titles it visually.
    title: String,
    on_close: EventHandler<()>,
    #[props(default)] size: ModalSize,
    /// A glyph beside the title.
    #[props(default)]
    icon: Option<Element>,
    /// Sentence under the title explaining what is being asked.
    #[props(default)]
    intro: Option<String>,
    /// The action row along the bottom.
    #[props(default)]
    footer: Option<Element>,
    #[props(default = false)] dismiss_on_backdrop: bool,
    #[props(default)] class: String,
    children: Element,
) -> Element {
    let skin = use_skin();
    let class = skin.class_with(Part::Modal, &[size.variant()], &class);
    rsx! {
        div {
            class: skin.class(Part::ModalScrim, &[]),
            onclick: move |_| {
                if dismiss_on_backdrop {
                    on_close.call(());
                }
            },
            div {
                class,
                role: "dialog",
                "aria-modal": "true",
                "aria-labelledby": "ik-modal-title",
                // Focusable so `Escape` reaches the dialog before anything inside has been
                // clicked into.
                tabindex: "-1",
                onmounted: move |event| {
                    let element = event.data();
                    spawn(async move {
                        let _ = element.set_focus(true).await;
                    });
                },
                onkeydown: move |event| {
                    if event.key() == Key::Escape {
                        on_close.call(());
                    }
                },
                // Without this a click anywhere in the dialog body bubbles to the scrim and
                // dismisses the thing being filled in.
                onclick: move |event| event.stop_propagation(),

                div { class: skin.class(Part::ModalHead, &[]),
                    {icon}
                    h2 { id: "ik-modal-title", "{title}" }
                }
                if let Some(intro) = intro {
                    p { class: skin.class(Part::ModalIntro, &[]), "{intro}" }
                }
                div { class: skin.class(Part::ModalBody, &[]), {children} }
                if let Some(footer) = footer {
                    div { class: skin.class(Part::ModalFoot, &[]), {footer} }
                }
            }
        }
    }
}
