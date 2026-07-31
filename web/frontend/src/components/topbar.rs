//! The top command bar: instant search, the `AniList` link pill and the notifications bell.

use crate::api;
use crate::components::UnreadBadge;
use crate::i18n::use_i18n;
use crate::icons::{Ic, Icon};
use crate::state::use_session;
use crate::Route;
use dioxus::prelude::*;

/// The provider the header pill reports on. Only one external tracker is registered today;
/// the Account panel is the data-driven surface for the rest.
const HEADER_SYNC_PROVIDER: &str = "anilist";

#[component]
pub(crate) fn TopBar() -> Element {
    let nav = use_navigator();
    let session = use_session();
    let i18n = use_i18n();
    let api = api::use_api();
    let unread = *use_context::<UnreadBadge>().0.read();
    let mut query = use_signal(String::new);

    let signed_in = session.is_authenticated();

    // AniList link state for the pill. This reads the endpoint's real `linked` flag: it used
    // to treat *any* successful response as "synced", because the endpoint was untyped and
    // the flag was invisible — so the pill claimed a connection that did not exist. The
    // response body is now part of the generated client, so the claim is checkable.
    let status = use_resource(move || {
        let client = api.client();
        async move {
            if !session.is_authenticated() {
                return None;
            }
            client
                .sync_status()
                .provider(HEADER_SYNC_PROVIDER)
                .send()
                .await
                .ok()
                .map(|response| response.into_inner().linked)
        }
    });
    let linked = matches!(&*status.read_unchecked(), Some(Some(true)));

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
                        to: Route::Account {},
                        class: if linked { "ik-pill jade" } else { "ik-pill" },
                        style: "display:inline-flex;align-items:center;gap:6px;text-decoration:none;",
                        Ic { icon: if linked { Icon::CloudDone } else { Icon::CloudOff }, size: 13 }
                        if linked {
                            {i18n.t("topbar.anilistSynced")}
                        } else {
                            {i18n.t("topbar.anilistConnect")}
                        }
                    }
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
                            // Polite, not assertive: a chapter landing while the reader is
                            // mid-sentence elsewhere should not interrupt them. The count is
                            // pushed by the SSE stream, so without a live region the change
                            // is silent.
                            span { class: "dot", "aria-live": "polite", "{unread}" }
                        }
                    }
                }
            }
        }
    }
}
