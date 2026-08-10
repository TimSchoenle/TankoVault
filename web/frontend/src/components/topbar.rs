//! The top command bar: instant search, the language control and the notifications bell.

use crate::components::{use_focus_targets, UnreadBadge};
use crate::i18n::{use_i18n, Translator, LOCALES};
use crate::icons::{Ic, Icon};
use crate::state::use_session;
use crate::Route;
use dioxus::prelude::*;

#[component]
pub(crate) fn TopBar() -> Element {
    let nav = use_navigator();
    let session = use_session();
    let i18n = use_i18n();
    let route: Route = use_route();
    let unread = *use_context::<UnreadBadge>().0.read();
    let mut query = use_signal(String::new);
    let mut focus_targets = use_focus_targets();

    let signed_in = session.is_authenticated();

    rsx! {
        header { class: "ik-topbar",
            // The band is full-bleed — its blur and hairline span the window — while this row
            // is capped at the page's `--measure`, so the bell stays over the list it counts.
            div { class: "ik-measure",
                // Small viewports only: the rail is gone, so the bar has to say where you are.
                div { class: "ik-topbar-page",
                    div { class: "ik-brand-tile sm", Ic { icon: Icon::MenuBook, size: 17 } }
                    span { class: "nm", {crate::title::page_name(&route, i18n)} }
                }
                div { class: "ik-search",
                    span { class: "lead", Ic { icon: Icon::Search, size: 16 } }
                    input {
                        // `index.html` binds ⌘K / Ctrl+K to focus this field by id — a web-only
                        // boot script. The console's jump button goes through the handle below,
                        // which works on both builds.
                        id: "tv-search",
                        onmounted: move |event| focus_targets.search.set(Some(event.data())),
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
                                    nav.push(Route::Search {
                                        query: crate::views::SearchQuery {
                                            q,
                                            ..crate::views::SearchQuery::default()
                                        },
                                    });
                                }
                            }
                        },
                    }
                    span { class: "kbd", "⌘K" }
                }
                div { class: "ik-topbar-actions",
                    // The ⌘K field does not fit beside a page title, so on small viewports the
                    // field collapses to this and the full search screen does the rest.
                    Link {
                        to: Route::Search { query: crate::views::SearchQuery::default() },
                        class: "ik-bell compact",
                        title: i18n.t("nav.search"),
                        "aria-label": i18n.t("nav.search"),
                        Ic { icon: Icon::Search, size: 18 }
                    }
                    LanguageButton {}
                    if signed_in {
                        Link {
                            to: Route::Notifications {},
                            // The bell leaves the bar below 820px — `/notifications` is a tab there.
                            class: "ik-bell wide-only",
                            title: i18n.t("nav.notifications"),
                            "aria-label": if unread > 0 {
                                i18n.plural("topbar.unreadCount", unread, &[])
                            } else {
                                i18n.t("nav.notifications")
                            },
                            Ic { icon: Icon::Notifications, size: 18 }
                            if unread > 0 {
                                // Polite, not assertive: an SSE-pushed count should not interrupt the reader.
                                //
                                // Compacted: the dot is a fixed circle on the bell's corner, and a
                                // literal four-figure count grew it past the icon and pushed the
                                // bar's actions out of alignment. The `aria-label` above still
                                // announces the exact number.
                                span { class: "dot", "aria-live": "polite", {crate::util::compact_count(unread)} }
                            }
                        }
                    }
                }
            }
        }
    }
}

/// The language control, in the slot the retired external-sync pill freed (handoff §4.3).
///
/// In the top bar rather than the footer or Account for one reason: it is reachable from every
/// screen *including signed-out ones*, and it is the one preference a reader may need before
/// they can read anything. Account → Appearance stays the full surface; below 820px this
/// collapses into the **More** sheet, where the bar has room for two icon buttons and no more.
#[component]
fn LanguageButton() -> Element {
    let i18n = use_i18n();
    let mut open = use_signal(|| false);
    let current = i18n.language();

    rsx! {
        div { class: "ik-langwrap wide-only",
            button {
                class: "ik-langbtn",
                r#type: "button",
                "aria-haspopup": "true",
                "aria-expanded": if *open.read() { "true" } else { "false" },
                "aria-label": i18n.t("nav.language"),
                onclick: move |_| {
                    let next = !*open.peek();
                    open.set(next);
                },
                Ic { icon: Icon::Language, size: 16 }
                span { class: "code", "{current.to_uppercase()}" }
                Ic { icon: Icon::ChevronDown, size: 13 }
            }
            if *open.read() {
                // Catches the click that closes the menu without dimming the page.
                button {
                    class: "ik-menu-backdrop",
                    "aria-label": i18n.t("common.close"),
                    onclick: move |_| open.set(false),
                }
                div { class: "ik-langmenu", role: "menu",
                    {locale_rows(i18n, &current, open)}
                }
            }
        }
    }
}

/// One row per shipped catalogue, named in its own language — a reader looking for theirs
/// recognises "Deutsch", not "German".
fn locale_rows(i18n: Translator, current: &str, mut open: Signal<bool>) -> Element {
    let current = current.to_owned();
    rsx! {
        for locale in LOCALES.iter() {
            button {
                key: "{locale.code}",
                class: "ik-wl-menu-item",
                r#type: "button",
                role: "menuitemradio",
                "aria-checked": if locale.code == current { "true" } else { "false" },
                onclick: move |_| {
                    i18n.set_language(locale.code);
                    open.set(false);
                },
                "{locale.endonym}"
                if locale.code == current {
                    span { style: "margin-left:auto;", Ic { icon: Icon::Check, size: 14 } }
                }
            }
        }
    }
}
