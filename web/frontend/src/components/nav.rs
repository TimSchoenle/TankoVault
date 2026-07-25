//! The left rail: brand lockup, grouped destinations and the user footer.

use crate::components::UnreadBadge;
use crate::icons::{Ic, Icon};
use crate::state::use_session;
use crate::util::initial;
use crate::Route;
use dioxus::prelude::*;

#[component]
pub(crate) fn Rail() -> Element {
    let session = use_session();
    let route: Route = use_route();
    let unread = *use_context::<UnreadBadge>().0.read();
    let is_operator = session.role.read().is_operator();

    rsx! {
        nav { class: "ik-rail",
            div { class: "ik-brand",
                div { class: "ik-brand-tile", Ic { icon: Icon::MenuBook, size: 22 } }
                div {
                    div { class: "ik-wordmark",
                        "Tankō"
                        span { class: "acc", "Vault" }
                    }
                    div { class: "ik-brand-tag", "SOURCE · TRACK · SYNC" }
                }
            }

            NavGroup { label: "MAIN" }
            NavLink { to: Route::Home {}, label: "Home", icon: Icon::Home, current: route.clone() }
            NavLink { to: Route::Discover {}, label: "Discover", icon: Icon::Explore, current: route.clone() }
            NavLink { to: Route::Search { q: String::new() }, label: "Search", icon: Icon::Search, current: route.clone() }

            NavGroup { label: "LIBRARY" }
            NavLink { to: Route::Watchlist {}, label: "Watchlist", icon: Icon::Watchlist, current: route.clone() }
            NavLink {
                to: Route::Notifications {},
                label: "Notifications",
                icon: Icon::Notifications,
                current: route.clone(),
                badge: unread,
            }

            if is_operator {
                NavGroup { label: "OPERATOR" }
                NavLink { to: Route::Console {}, label: "Console", icon: Icon::Console, current: route.clone() }
            }

            NavGroup { label: "ACCOUNT" }
            NavLink { to: Route::Account {}, label: "Account", icon: Icon::Account, current: route.clone() }

            div { class: "ik-rail-spacer" }
            UserFooter {}
        }
    }
}

/// A kicker heading that groups rail destinations.
#[component]
fn NavGroup(label: String) -> Element {
    rsx! {
        div { class: "ik-navgroup",
            div { class: "ik-navgroup-label", "{label}" }
        }
    }
}

/// A rail entry with an icon, label, the animated active bar, and an optional count badge.
#[component]
fn NavLink(
    to: Route,
    label: String,
    icon: Icon,
    current: Route,
    #[props(default = 0)] badge: i64,
) -> Element {
    let class = if same_screen(&to, &current) {
        "ik-nav-link active"
    } else {
        "ik-nav-link"
    };
    rsx! {
        Link { to: to.clone(), class: "{class}",
            Ic { icon, size: 18 }
            span { class: "label", "{label}" }
            if badge > 0 {
                span { class: "ik-nav-badge", "{badge}" }
            }
        }
    }
}

/// Whether two routes belong to the same top-level rail destination, so a detail screen keeps
/// its parent entry lit.
fn same_screen(a: &Route, b: &Route) -> bool {
    use std::mem::discriminant;
    // Series detail lives under Discover in the rail's mental model.
    let normalise = |route: &Route| match route {
        Route::Series { .. } => Route::Discover {},
        other => other.clone(),
    };
    discriminant(&normalise(a)) == discriminant(&normalise(b))
}

/// Avatar + identity + settings gear when signed in; a "Sign in" button otherwise.
/// Sign-out lives on the Account screen.
#[component]
fn UserFooter() -> Element {
    let session = use_session();
    if !session.is_authenticated() {
        return rsx! {
            div { style: "padding:8px;",
                Link { to: Route::Login {}, class: "ik-btn primary block", "Sign in" }
            }
        };
    }

    let name = session.username().unwrap_or_else(|| "reader".to_owned());
    let role = session.role.read().label();
    rsx! {
        div { class: "ik-userbox",
            div { class: "ik-avatar", "{initial(&name)}" }
            div { class: "who",
                div { class: "name", "{name}" }
                div { class: "sub",
                    span { class: "ik-status-dot" }
                    "{role}"
                }
            }
            Link { to: Route::Account {}, class: "gear", title: "Account settings",
                Ic { icon: Icon::Settings, size: 18 }
            }
        }
    }
}
