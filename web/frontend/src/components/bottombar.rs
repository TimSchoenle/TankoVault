//! The small-viewport navigation: a fixed five-tab bar and the **More** sheet behind its last
//! tab (layout handoff §4.1).
//!
//! Below 820px the rail used to become a wrapping strip of eleven capability-gated links, the
//! brand lockup and the user box — roughly a third of a 390px viewport before any content. The
//! bar replaces it with up to five 44px targets; everything they cannot carry moves into the
//! sheet, which is where the per-reader gating lives so the bar does not reflow around a
//! permission. Signing out is the one thing that does shorten it — see `nav::tab_destinations`.

use crate::components::nav::{reader_destinations_visible, tab_destinations, Destination};
use crate::components::UnreadBadge;
use crate::i18n::{use_i18n, Translator, LOCALES};
use crate::icons::{Ic, Icon};
use crate::models::LegalKind;
use crate::state::capabilities::use_capabilities;
use crate::state::legal::{legal_title, use_legal_index};
use crate::state::use_session;
use crate::views::AccountPanel;
use crate::wire::types::Feature;
use crate::Route;
use dioxus::prelude::*;
use std::rc::Rc;

#[component]
pub(crate) fn BottomTabs() -> Element {
    let i18n = use_i18n();
    let caps = use_capabilities();
    let session = use_session();
    let route: Route = use_route();
    let unread = *use_context::<UnreadBadge>().0.read();
    let mut sheet = use_signal(|| false);
    // The sheet is a modal, so dismissing it has to put the keyboard back where it came from —
    // and the element it came from is this button. A mounted handle rather than a DOM id
    // because the desktop build has no `web-sys`; see `components::focus`.
    let mut more_tab = use_signal(|| Option::<Rc<MountedData>>::None);

    // A route change has to close the sheet: every row in it navigates, and a sheet left open
    // over the screen it just opened covers the thing the reader asked for.
    use_effect(use_reactive!(|route| {
        // The route is the dependency, not an input — any change to it closes the sheet.
        drop(route);
        if *sheet.peek() {
            sheet.set(false);
        }
    }));

    let personal = reader_destinations_visible(session.is_authenticated(), session.is_settled());
    let tabs = tab_destinations(i18n, &caps, unread, personal);
    rsx! {
        nav { class: "ik-tabbar", "aria-label": i18n.t("nav.tabbarLabel"),
            for tab in tabs {
                {tab_link(&tab, &route)}
            }
            button {
                class: if *sheet.read() { "ik-tab-item active" } else { "ik-tab-item" },
                r#type: "button",
                "aria-haspopup": "dialog",
                "aria-expanded": if *sheet.read() { "true" } else { "false" },
                onmounted: move |event| more_tab.set(Some(event.data())),
                onclick: move |_| {
                    let next = !*sheet.peek();
                    sheet.set(next);
                },
                Ic { icon: Icon::MoreHoriz, size: 21 }
                span { class: "ik-tab-label", {i18n.t("nav.more")} }
            }
        }
        if *sheet.read() {
            MoreSheet {
                on_close: move |()| {
                    sheet.set(false);
                    // The sheet held the focus, and it is about to stop existing. Only the
                    // dismissals come through here — a row that navigates closes the sheet
                    // through the route effect above instead, and that reader wants the screen
                    // they asked for, not this button.
                    if let Some(element) = more_tab.peek().clone() {
                        spawn(async move {
                            let _ = element.set_focus(true).await;
                        });
                    }
                },
            }
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
                span { class: "ik-tab-badge", "aria-hidden": "true", {crate::util::compact_count(badge)} }
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
    let personal = reader_destinations_visible(session.is_authenticated(), session.is_settled());

    rsx! {
        button {
            class: "ik-sheet-backdrop",
            "aria-label": i18n.t("common.close"),
            onclick: move |_| on_close.call(()),
        }
        div {
            class: "ik-sheet",
            role: "dialog",
            "aria-modal": "true",
            "aria-label": i18n.t("nav.more"),
            // Focusable so `Escape` reaches the sheet with nothing else focused, and so the
            // first `Tab` lands on the first row rather than back in the page behind — the
            // step-up dialog's treatment, which this claimed with `aria-modal` and never had.
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
            div { class: "ik-sheet-grip" }

            if caps.has_feature(Feature::CatalogueSearch) {
                {sheet_link(Route::Search { query: crate::views::SearchQuery::default() }, Icon::Search, &i18n.t("nav.search"))}
            }
            // In the sheet rather than the bar, which is fixed at five: this is per-reader
            // gating, which is what the sheet is for. Without the row `/for-you` had no
            // small-viewport entry point but the CTA below two lists at the foot of Home.
            if personal && caps.has_feature(Feature::CatalogueRecommendations) {
                {sheet_link(Route::Recommendations {}, Icon::AutoAwesome, &i18n.t("nav.recommendations"))}
            }
            if caps.is_staff() {
                {sheet_link(Route::Console {}, Icon::Console, &i18n.t("nav.console"))}
            }
            if session.is_authenticated() {
                {sheet_link(Route::Account {}, Icon::Account, &i18n.t("nav.account"))}
                {sheet_link(Route::AccountSection { panel: AccountPanel::Appearance }, Icon::Tune, &i18n.t("account.tab.appearance"))}
            } else {
                {sheet_link(Route::Login {}, Icon::Account, &i18n.t("common.signIn"))}
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

            // The footer cannot share the bottom edge with a fixed tab bar, so its Legal column
            // lives here at this width — same source, same "configured or absent" behaviour.
            {legal_block(i18n)}

            div { class: "ik-sheet-head", {i18n.t("footer.openSource")} }
            // An in-app route now, so no origin to resolve it against and nothing to withhold
            // on the desktop build before a server is chosen — see `footer::OpenSourceColumn`.
            {sheet_link(Route::Licenses {}, Icon::Code, &i18n.t("nav.notices"))}
        }
    }
}

/// The documents this deployment publishes, as sheet rows.
///
/// Renders nothing at all when there are none — which is also what a failed index fetch looks
/// like, and is the correct degradation either way: a heading over an empty list is worse than
/// no heading.
fn legal_block(i18n: Translator) -> Element {
    let entries = use_legal_index();
    let entries = entries.read().clone();
    if entries.is_empty() {
        return rsx! {};
    }
    rsx! {
        div { class: "ik-sheet-head", {i18n.t("footer.legal")} }
        for entry in entries {
            match entry.kind {
                LegalKind::External => {
                    let href = entry.url.clone().unwrap_or_default();
                    rsx! {
                        a {
                            key: "{entry.slug}",
                            class: "ik-sheet-row",
                            href: "{href}",
                            target: "_blank",
                            rel: "noopener noreferrer",
                            Ic { icon: Icon::Gavel, size: 19 }
                            {legal_title(i18n, &entry.slug, entry.title.as_deref())}
                            span { class: "val", Ic { icon: Icon::OpenInNew, size: 14 } }
                        }
                    }
                }
                LegalKind::Inline => rsx! {
                    Link {
                        key: "{entry.slug}",
                        to: Route::Legal { slug: entry.slug.clone() },
                        class: "ik-sheet-row",
                        Ic { icon: Icon::Gavel, size: 19 }
                        {legal_title(i18n, &entry.slug, entry.title.as_deref())}
                    }
                },
            }
        }
    }
}

/// A routed row in the sheet.
fn sheet_link(to: Route, icon: Icon, label: &str) -> Element {
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
