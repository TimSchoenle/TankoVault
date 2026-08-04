//! The small-viewport navigation: a fixed five-tab bar and the **More** sheet behind its last
//! tab (layout handoff §4.1).
//!
//! Below 820px the rail used to become a wrapping strip of eleven capability-gated links, the
//! brand lockup and the user box — roughly a third of a 390px viewport before any content. The
//! bar replaces it with five 44px targets; everything the five cannot carry moves into the
//! sheet, which is also where the per-reader gating lives so the bar itself never reflows.

use crate::components::nav::{tab_destinations, Destination, NOTICES_ROUTE};
use crate::components::UnreadBadge;
use crate::i18n::{use_i18n, Translator, LOCALES};
use crate::icons::{Ic, Icon};
use crate::state::capabilities::use_capabilities;
use crate::state::use_session;
use crate::wire::types::Feature;
use crate::Route;
use dioxus::prelude::*;

#[component]
pub(crate) fn BottomTabs() -> Element {
    let i18n = use_i18n();
    let caps = use_capabilities();
    let route: Route = use_route();
    let unread = *use_context::<UnreadBadge>().0.read();
    let mut sheet = use_signal(|| false);

    // A route change has to close the sheet: every row in it navigates, and a sheet left open
    // over the screen it just opened covers the thing the reader asked for.
    use_effect(use_reactive!(|route| {
        // The route is the dependency, not an input — any change to it closes the sheet.
        drop(route);
        if *sheet.peek() {
            sheet.set(false);
        }
    }));

    let tabs = tab_destinations(i18n, &caps, unread);
    rsx! {
        nav { class: "ik-tabbar", "aria-label": i18n.t("nav.railLabel"),
            for tab in tabs {
                {tab_link(&tab, &route)}
            }
            button {
                class: if *sheet.read() { "ik-tab-item active" } else { "ik-tab-item" },
                r#type: "button",
                "aria-haspopup": "dialog",
                "aria-expanded": if *sheet.read() { "true" } else { "false" },
                onclick: move |_| {
                    let next = !*sheet.peek();
                    sheet.set(next);
                },
                Ic { icon: Icon::MoreHoriz, size: 21 }
                span { class: "ik-tab-label", {i18n.t("nav.more")} }
            }
        }
        if *sheet.read() {
            MoreSheet { on_close: move |()| sheet.set(false) }
        }
    }
}

/// One routed tab. Active by exact route rather than by [`crate::components::nav`]'s
/// `same_screen`, because the bar has no Discover-parents-Series relationship to express: a
/// series page is reached from any tab.
fn tab_link(tab: &Destination, current: &Route) -> Element {
    let active = std::mem::discriminant(&tab.route) == std::mem::discriminant(current);
    let badge = tab.badge;
    rsx! {
        Link {
            key: "{tab.short}",
            to: tab.route.clone(),
            class: if active { "ik-tab-item active" } else { "ik-tab-item" },
            "aria-current": if active { "page" } else { "false" },
            Ic { icon: tab.icon, size: 21 }
            if badge > 0 {
                span { class: "ik-tab-badge", "aria-hidden": "true", "{badge}" }
            }
            span { class: "ik-tab-label", "{tab.short}" }
        }
    }
}

/// Everything the five fixed tabs cannot carry: the screens that did not make the bar, the
/// language knob the top bar hides at this width, and the licence notices.
#[component]
fn MoreSheet(on_close: EventHandler<()>) -> Element {
    let i18n = use_i18n();
    let caps = use_capabilities();
    let session = use_session();
    let mut languages = use_signal(|| false);
    let current = i18n.language();

    rsx! {
        button {
            class: "ik-sheet-backdrop",
            "aria-label": i18n.t("common.close"),
            onclick: move |_| on_close.call(()),
        }
        div { class: "ik-sheet", role: "dialog", "aria-modal": "true", "aria-label": i18n.t("nav.more"),
            div { class: "ik-sheet-grip" }

            if caps.has_feature(Feature::CatalogueSearch) {
                {sheet_link(Route::Search { q: String::new() }, Icon::Search, i18n.t("nav.search"))}
            }
            if caps.is_staff() {
                {sheet_link(Route::Console {}, Icon::Console, i18n.t("nav.console"))}
            }
            if session.is_authenticated() {
                {sheet_link(Route::Account {}, Icon::Account, i18n.t("nav.account"))}
                {sheet_link(Route::Account {}, Icon::Tune, i18n.t("account.tab.appearance"))}
            } else {
                {sheet_link(Route::Login {}, Icon::Account, i18n.t("common.signIn"))}
            }

            button {
                class: "ik-sheet-row",
                r#type: "button",
                "aria-expanded": if *languages.read() { "true" } else { "false" },
                onclick: move |_| {
                    let next = !*languages.peek();
                    languages.set(next);
                },
                Ic { icon: Icon::Language, size: 19 }
                {i18n.t("nav.language")}
                span { class: "val",
                    {LOCALES.iter().find(|l| l.code == current).map_or("", |l| l.endonym)}
                }
            }
            if *languages.read() {
                {language_rows(i18n, &current)}
            }

            div { class: "ik-sheet-head", {i18n.t("footer.openSource")} }
            a {
                class: "ik-sheet-row",
                href: NOTICES_ROUTE,
                target: "_blank",
                rel: "noopener noreferrer",
                Ic { icon: Icon::Code, size: 19 }
                {i18n.t("nav.notices")}
                span { class: "val", Ic { icon: Icon::OpenInNew, size: 14 } }
            }
        }
    }
}

/// A routed row in the sheet.
fn sheet_link(to: Route, icon: Icon, label: String) -> Element {
    rsx! {
        Link { to, class: "ik-sheet-row",
            Ic { icon, size: 19 }
            "{label}"
        }
    }
}

/// The shipped catalogues, each named in its own language.
fn language_rows(i18n: Translator, current: &str) -> Element {
    let current = current.to_owned();
    rsx! {
        for locale in LOCALES.iter() {
            button {
                key: "{locale.code}",
                class: "ik-sheet-row",
                r#type: "button",
                role: "menuitemradio",
                "aria-checked": if locale.code == current { "true" } else { "false" },
                style: "padding-left:37px;",
                onclick: move |_| i18n.set_language(locale.code),
                "{locale.endonym}"
                if locale.code == current {
                    span { class: "val", Ic { icon: Icon::Check, size: 15 } }
                }
            }
        }
    }
}
