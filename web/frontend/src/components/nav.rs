//! The left rail: brand lockup, grouped destinations and the user footer.

use crate::components::{UnreadBadge, Wordmark};
use crate::i18n::{use_i18n, Translator};
use crate::icons::{Ic, Icon};
use crate::state::branding::use_branding;
use crate::state::capabilities::{use_capabilities, CapabilitySet};
use crate::state::use_session;
use crate::util::initial;
use crate::views::{DiscoverQuery, NotificationsQuery, WatchlistQuery};
use crate::wire::types::Feature;
use crate::Route;
use dioxus::prelude::*;
use inkstone_ui::{button_class, Size, Tone};
/// The line under the rail's lockup: the operator's own tagline, or the catalogue's.
///
/// Same rule as the footer's — see `components::footer` — and the same one string, because two
/// taglines for one deployment is a difference nobody chose.
fn rail_tagline(i18n: Translator) -> String {
    use_branding()
        .read()
        .tagline
        .clone()
        .unwrap_or_else(|| i18n.t("nav.tagline"))
}

#[component]
pub(crate) fn Rail() -> Element {
    let i18n = use_i18n();
    let route: Route = use_route();
    let unread = *use_context::<UnreadBadge>().0.read();
    let caps = use_capabilities();
    let session = use_session();

    // Each entry needs both: the reader is allowed, and the deployment offers the feature.
    let personal = reader_destinations_visible(session.is_authenticated(), session.is_settled());
    let show_search = caps.has_feature(Feature::CatalogueSearch);
    let show_discover = caps.has_feature(Feature::CatalogueBrowse);
    let show_watchlist = personal && caps.has_feature(Feature::TrackingWatchlist);
    let show_notifications = personal && caps.has_feature(Feature::NotificationsInApp);
    let show_console = caps.is_staff();
    // Signed-out readers have no taste profile, so the destination would be an auth wall.
    let show_recommendations = personal && caps.has_feature(Feature::CatalogueRecommendations);

    rsx! {
        nav { class: "ik-rail", "aria-label": i18n.t("nav.railLabel"),
            div { class: "ik-brand",
                div { class: "ik-brand-tile", Ic { icon: Icon::MenuBook, size: 22 } }
                div {
                    // The deployment's name, not a message: it is configuration, so it is
                    // deliberately not in the catalogue.
                    Wordmark { class: "ik-wordmark" }
                    div { class: "ik-brand-tag", {rail_tagline(i18n)} }
                }
            }

            NavGroup { label: i18n.t("nav.group.main") }
            NavLink { to: Route::Home {}, label: i18n.t("nav.home"), icon: Icon::Home, current: route.clone() }
            if show_discover {
                NavLink { to: Route::Discover { query: DiscoverQuery::default() }, label: i18n.t("nav.discover"), icon: Icon::Explore, current: route.clone() }
            }
            if show_search {
                NavLink { to: Route::Search { query: crate::views::SearchQuery::default() }, label: i18n.t("nav.search"), icon: Icon::Search, current: route.clone() }
            }
            if show_recommendations {
                NavLink { to: Route::Recommendations {}, label: i18n.t("nav.recommendations"), icon: Icon::AutoAwesome, current: route.clone() }
            }

            if show_watchlist || show_notifications {
                NavGroup { label: i18n.t("nav.group.library") }
            }
            if show_watchlist {
                NavLink { to: Route::Watchlist { query: WatchlistQuery::default() }, label: i18n.t("nav.watchlist"), icon: Icon::Watchlist, current: route.clone() }
            }
            if show_notifications {
                NavLink {
                    to: Route::Notifications { query: NotificationsQuery::default() },
                    label: i18n.t("nav.notifications"),
                    icon: Icon::Notifications,
                    current: route.clone(),
                    badge: unread,
                }
            }

            if show_console {
                NavGroup { label: i18n.t("nav.group.operator") }
                NavLink { to: Route::Console {}, label: i18n.t("nav.console"), icon: Icon::Console, current: route.clone() }
            }

            if personal {
                NavGroup { label: i18n.t("nav.group.account") }
                NavLink { to: Route::Account {}, label: i18n.t("nav.account"), icon: Icon::Account, current: route.clone() }
            }

            div { class: "ik-rail-spacer" }
            UserFooter {}
        }
    }
}

/// Whether the reader-scoped destinations — watchlist, notifications, recommendations, account —
/// belong in the chrome at all.
///
/// Every one of them is an auth wall without a session, so offering them signed out advertises
/// four screens that can only answer "sign in". They are withdrawn on the *settled* answer, not
/// on the absent token: the token is adopted from the refresh cookie by a network round trip, so
/// gating on `!authenticated` alone would strip the rail and the tab bar on every reload and
/// then put them back a moment later.
pub(crate) const fn reader_destinations_visible(authenticated: bool, settled: bool) -> bool {
    authenticated || !settled
}

/// One routed destination in the bottom tab bar.
///
/// The set lives beside the rail it mirrors rather than in the bar's own module: these are the
/// same navigation seen at two widths, and a destination added to one chrome and forgotten in
/// the other is unreachable for half the readers.
pub(crate) struct Destination {
    pub(crate) route: Route,
    /// The bar's label. German's `Benachrichtigungen` does not fit a fifth of a 390px viewport,
    /// so the bar takes a shorter key rather than an ellipsis.
    pub(crate) short: String,
    pub(crate) icon: Icon,
    /// The unread count to badge, or zero.
    pub(crate) badge: i64,
}

/// The routed destinations the bottom tab bar draws, in bar order.
///
/// Feature-gated, not permission-gated: the deployment-level switches are the same for every
/// reader of a given instance, so the bar never reflows *per reader*. Everything that does vary
/// by who is holding it — the console above all — lives in the **More** sheet instead.
///
/// Signing in or out is the one exception, and deliberately so: `personal` drops the two
/// reader-scoped tabs for a reader we know has no session, because a tab bar whose middle two
/// slots both lead to a sign-in gate is worse than a shorter bar. See
/// [`reader_destinations_visible`] for why "no session" is not simply "no token yet".
pub(crate) fn tab_destinations(
    i18n: crate::i18n::Translator,
    caps: &CapabilitySet,
    unread: i64,
    personal: bool,
) -> Vec<Destination> {
    let mut out = vec![Destination {
        route: Route::Home {},
        short: i18n.t("nav.home"),
        icon: Icon::Home,
        badge: 0,
    }];
    if caps.has_feature(Feature::CatalogueBrowse) {
        out.push(Destination {
            route: Route::Discover {
                query: DiscoverQuery::default(),
            },
            short: i18n.t("nav.discover"),
            icon: Icon::Explore,
            badge: 0,
        });
    }
    if personal && caps.has_feature(Feature::TrackingWatchlist) {
        out.push(Destination {
            route: Route::Watchlist {
                query: WatchlistQuery::default(),
            },
            short: i18n.t("nav.watchlist"),
            icon: Icon::Watchlist,
            badge: 0,
        });
    }
    if personal && caps.has_feature(Feature::NotificationsInApp) {
        out.push(Destination {
            route: Route::Notifications {
                query: NotificationsQuery::default(),
            },
            short: i18n.t("nav.alerts"),
            icon: Icon::Notifications,
            badge: unread,
        });
    }
    out
}

// The rail's own notices link retired with the footer, which carries it in its Open source
// column on every screen and in the More sheet below 820px. The reason it was in the rail at
// all — a signed-out reader has received the same bundle and is owed the same notices — is
// unchanged and still met: the footer renders signed out. The literals that used to live here
// moved with the link, to `views/licenses.rs`, which is the only thing that reaches for them now.

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
    let active = same_screen(&to, &current);
    let class = if active {
        "ik-nav-link active"
    } else {
        "ik-nav-link"
    };
    rsx! {
        Link {
            to: to.clone(),
            class: "{class}",
            "aria-current": if active { "page" } else { "false" },
            Ic { icon, size: 18 }
            span { class: "label", "{label}" }
            if badge > 0 {
                // Compacted, not raw: the badge is a fixed pill and a four-figure inbox used to
                // widen it past the label it sits beside. `title` keeps the exact number
                // reachable for anyone who wants it.
                span { class: "ik-nav-badge", title: "{badge}", {crate::util::compact_count(badge)} }
            }
        }
    }
}

/// Whether two routes belong to the same top-level rail destination, so a detail screen keeps
/// its parent entry lit.
fn same_screen(a: &Route, b: &Route) -> bool {
    use std::mem::discriminant;
    // The comparison below is by discriminant, so every addressable sub-route folds onto the
    // entry that leads to it or the rail goes dark under the reader's feet: `/console/providers`
    // and `/account/security` are their own variants, not the `Console {}` and `Account {}` the
    // rail links to. A section route added without a line here is the same bug again.
    let normalise = |route: &Route| match route {
        // Series detail lives under Discover in the rail's mental model.
        Route::Series { .. } => Route::Discover {
            query: DiscoverQuery::default(),
        },
        Route::ConsoleSection { .. } => Route::Console {},
        Route::AccountSection { .. } => Route::Account {},
        other => other.clone(),
    };
    discriminant(&normalise(a)) == discriminant(&normalise(b))
}

/// Avatar + identity + settings gear when signed in; a "Sign in" button once we know there is
/// no session. Sign-out lives on the Account screen.
#[component]
fn UserFooter() -> Element {
    let session = use_session();
    let caps = use_capabilities();
    let i18n = use_i18n();
    if !session.is_authenticated() {
        // Nothing at all until the boot refresh settles: a "Sign in" button that turns into the
        // reader's own name a moment later is the sign-in flash `Session::ready` exists to stop.
        if !session.is_settled() {
            return rsx! {};
        }
        return rsx! {
            div { style: "padding:8px;",
                Link { to: Route::Login {}, class: button_class(Tone::Primary, Size::Md, true), {i18n.t("common.signIn")} }
            }
        };
    }

    let name = session
        .username()
        .unwrap_or_else(|| i18n.t("common.readerFallback"));
    // Derived from the reader's actual capabilities, not a stored role.
    let tier = i18n.t(caps.label_key());
    rsx! {
        div { class: "ik-userbox",
            div { class: "ik-avatar", "{initial(&name)}" }
            div { class: "who",
                div { class: "name", "{name}" }
                div { class: "sub",
                    span { class: "ik-status-dot" }
                    "{tier}"
                }
            }
            Link { to: Route::Account {}, class: "gear", title: i18n.t("nav.accountSettings"),
                Ic { icon: Icon::Settings, size: 18 }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{reader_destinations_visible, same_screen, Route};
    use crate::views::{AccountPanel, ConsoleEntity, ConsoleQuery, DiscoverQuery};

    /// Both halves of the rule, because either one alone is a defect.
    ///
    /// Without the `settled` term the rail and the tab bar are stripped for the length of the
    /// boot-time silent refresh on every reload, and put back once it lands. Without the rule at
    /// all — the behaviour this replaced — a signed-out reader is offered four destinations that
    /// can only answer "sign in".
    #[test]
    fn reader_destinations_survive_boot_and_go_on_a_settled_sign_out() {
        assert!(
            reader_destinations_visible(false, false),
            "an unsettled session means `we have not looked yet`, not `signed out`"
        );
        assert!(reader_destinations_visible(true, true));
        assert!(reader_destinations_visible(true, false));
        assert!(
            !reader_destinations_visible(false, true),
            "a settled sign-out is the one state that withdraws them"
        );
    }

    /// An addressable sub-route keeps its parent rail entry lit.
    ///
    /// `same_screen` compares `std::mem::discriminant`s, and `/console` replaces itself with
    /// `/console/:entity` on arrival — so the Console entry went dark the instant the operator
    /// landed on the console, and the rail claimed they were nowhere. `/account/:panel` arrived
    /// later with the same shape and would have repeated it.
    #[test]
    fn an_addressable_sub_route_keeps_its_parent_entry_lit() {
        let console = Route::ConsoleSection {
            entity: ConsoleEntity::Overview,
            query: ConsoleQuery::fresh(),
        };
        assert!(same_screen(&Route::Console {}, &console));

        let panel = Route::AccountSection {
            panel: AccountPanel::Appearance,
        };
        assert!(same_screen(&Route::Account {}, &panel));

        // Still discriminating: folding must not make every entry light at once.
        assert!(!same_screen(&Route::Account {}, &console));
        assert!(!same_screen(
            &Route::Discover {
                query: DiscoverQuery::default(),
            },
            &panel
        ));
    }
}
