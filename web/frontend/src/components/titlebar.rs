//! The app-drawn window header, for the desktop build only.
//!
//! The OS caption is switched off in `main` (`with_decorations(false)`), so this is the whole
//! title bar: it names the window, drags it, and carries the minimise/maximise/close controls.
//! It sits above the router rather than inside it, so it is present on the first-run connection
//! screen and the sign-in screen too — which is also why the settings button lives here. That is
//! the one control a reader needs when the *server* is the thing that is wrong, and everything
//! behind the router needs a working server to render.
//!
//! **What this costs, so nobody rediscovers it as a bug.** Windows 11's snap-layouts flyout
//! appears when the pointer rests on a *real* maximise button, which the shell decides by asking
//! the window to hit-test its caption (`WM_NCHITTEST` → `HTMAXBUTTON`). `tao` does not expose
//! that hook, so the flyout does not appear here. Every other route to the same feature still
//! works: `Win`+arrow, dragging to a screen edge, and the window menu.
//!
//! The window menu itself is reached by right-clicking the drag area or pressing `Alt`+`Space`,
//! both of which the OS still handles because the window keeps its system menu — losing them is
//! the accessibility trap in a custom title bar, and the reason this one is a strip of real
//! buttons with labels rather than a picture of one.

use crate::i18n::use_i18n;
use crate::icons::{Ic, Icon};
use dioxus::prelude::*;

/// The window header: drag area, title, settings, and the window controls.
#[component]
pub(crate) fn TitleBar(on_settings: EventHandler<()>) -> Element {
    let i18n = use_i18n();
    // Not `use_route`: this bar renders *above* the router, so there is no route context to ask
    // — calling it here aborted the process on boot. `crate::title` publishes the screen's name
    // instead; before the first routed screen mounts (the connection screen) it is empty, and
    // the product name is the right thing to show.
    let title = crate::platform::WINDOW_HEADING.read().clone();
    let title = if title.is_empty() {
        i18n.t("title.app")
    } else {
        title
    };
    // Re-read on every render rather than cached: the window can be maximised by the OS — a
    // double-click, `Win`+`Up`, a drag to the top edge — and a cached flag would leave this
    // showing the wrong glyph until something else happened to re-render.
    let maximised = crate::platform::window().is_some_and(|window| window.is_maximized());

    rsx! {
        header { class: "ik-titlebar",
            // The drag surface. `drag_window` hands the move to the OS, which is what keeps
            // snapping, multi-monitor DPI and the drag-to-edge gestures working — a hand-rolled
            // move loop gets none of that.
            div {
                class: "ik-titlebar-grab",
                onmousedown: move |_| {
                    if let Some(window) = crate::platform::window() {
                        window.drag();
                    }
                },
                ondoubleclick: move |_| toggle_maximise(),
                div { class: "ik-brand-tile sm", Ic { icon: Icon::MenuBook, size: 15 } }
                span { class: "ik-titlebar-name", "{title}" }
            }

            button {
                class: "ik-titlebar-btn",
                r#type: "button",
                "aria-label": i18n.t("settings.title"),
                title: i18n.t("settings.title"),
                onclick: move |_| on_settings.call(()),
                // Sliders, not the gear the rest of the app uses for settings. The gear's path
                // is a ring of arcs, and at the 15px a title-bar control gets they collapse into
                // a smudge; three strokes and three dots survive the size.
                Ic { icon: Icon::Tune, size: 15 }
            }

            div { class: "ik-titlebar-controls",
                button {
                    class: "ik-titlebar-btn",
                    r#type: "button",
                    "aria-label": i18n.t("window.minimise"),
                    title: i18n.t("window.minimise"),
                    onclick: move |_| {
                        if let Some(window) = crate::platform::window() {
                            window.set_minimized(true);
                        }
                    },
                    Ic { icon: Icon::Remove, size: 15 }
                }
                button {
                    class: "ik-titlebar-btn",
                    r#type: "button",
                    "aria-label": if maximised { i18n.t("window.restore") } else { i18n.t("window.maximise") },
                    title: if maximised { i18n.t("window.restore") } else { i18n.t("window.maximise") },
                    onclick: move |_| toggle_maximise(),
                    Ic {
                        icon: if maximised { Icon::WindowRestore } else { Icon::WindowMaximise },
                        size: 13,
                    }
                }
                button {
                    class: "ik-titlebar-btn danger",
                    r#type: "button",
                    "aria-label": i18n.t("common.close"),
                    title: i18n.t("common.close"),
                    onclick: move |_| {
                        if let Some(window) = crate::platform::window() {
                            window.close();
                        }
                    },
                    Ic { icon: Icon::Close, size: 15 }
                }
            }
        }
    }
}

fn toggle_maximise() {
    if let Some(window) = crate::platform::window() {
        window.set_maximized(!window.is_maximized());
    }
}
