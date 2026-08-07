//! Routed screens (design §17.2). Each is a Dioxus component named to match a `Route`
//! variant in `main.rs`.

mod account;
mod auth;
// Not a route: it renders instead of the router until a server is chosen. Desktop only — the
// web build is served by the API it talks to and has nothing to ask.
#[cfg(feature = "desktop")]
pub(crate) mod connect;
mod console;
mod discover;
mod home;
mod legal;
mod notifications;
mod password;
mod recommendations;
mod search;
mod series;
mod watchlist;

pub(crate) use account::{Account, AnilistCallback};
pub(crate) use auth::{Login, VerifyEmail};
#[cfg(feature = "desktop")]
pub(crate) use connect::ConnectServer;
pub(crate) use console::{Console, ConsoleEntity, ConsoleQuery, ConsoleSection};
pub(crate) use discover::Discover;
pub(crate) use home::Home;
pub(crate) use legal::Legal;
pub(crate) use notifications::Notifications;
pub(crate) use password::{ForgotPassword, ResetPassword};
pub(crate) use recommendations::Recommendations;
pub(crate) use search::Search;
pub(crate) use series::Series;
pub(crate) use watchlist::{Watchlist, WatchlistQuery};

use dioxus::prelude::*;

/// Catch-all 404 (design §17.3: error states name what failed).
#[component]
pub(crate) fn NotFound(segments: Vec<String>) -> Element {
    let i18n = crate::i18n::use_i18n();
    let path = segments.join("/");
    rsx! {
        h1 { class: "ik-page-title", {i18n.t("notFound.title")} }
        div { class: "ik-empty",
            p { {i18n.args("notFound.body", &[("path", &format!("/{path}"))])} }
            Link { to: crate::Route::Home {}, class: "ik-btn primary", {i18n.t("notFound.back")} }
        }
    }
}
