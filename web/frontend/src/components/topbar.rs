//! The top command bar: instant search and the notifications bell.

use crate::components::UnreadBadge;
use crate::i18n::use_i18n;
use crate::icons::{Ic, Icon};
use crate::state::use_session;
use crate::Route;
use dioxus::prelude::*;

#[component]
pub(crate) fn TopBar() -> Element {
    let nav = use_navigator();
    let session = use_session();
    let i18n = use_i18n();
    let unread = *use_context::<UnreadBadge>().0.read();
    let mut query = use_signal(String::new);

    let signed_in = session.is_authenticated();

    rsx! {
        header { class: "ik-topbar",
            div { class: "ik-search",
                span { class: "lead", Ic { icon: Icon::Search, size: 16 } }
                input {
                    // `index.html` binds ⌘K / Ctrl+K to focus this field by id.
                    id: "tv-search",
                    class: "ik-input",
                    r#type: "search",
                    placeholder: i18n.t("topbar.searchPlaceholder"),
                    "aria-label": i18n.t("topbar.searchPlaceholder"),
                    value: "{query}",
                    oninput: move |e| query.set(e.value()),
                    onkeydown: move |e| {
                        if e.key() == Key::Enter {
                            let q = query.read().trim().to_owned();
                            if !q.is_empty() {
                                nav.push(Route::Search { q });
                            }
                        }
                    },
                }
                span { class: "kbd", "⌘K" }
            }
            div { class: "ik-topbar-actions",
                if signed_in {
                    Link {
                        to: Route::Notifications {},
                        class: "ik-bell",
                        title: i18n.t("nav.notifications"),
                        "aria-label": if unread > 0 {
                            i18n.plural("topbar.unreadCount", unread, &[])
                        } else {
                            i18n.t("nav.notifications")
                        },
                        Ic { icon: Icon::Notifications, size: 18 }
                        if unread > 0 {
                            // Polite, not assertive: an SSE-pushed count should not interrupt the reader.
                            span { class: "dot", "aria-live": "polite", "{unread}" }
                        }
                    }
                }
            }
        }
    }
}
