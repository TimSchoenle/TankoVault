//! Routed screens (design §17.2). Each is a Dioxus component named to match a `Route`
//! variant in `main.rs`.

mod account;
mod auth;
mod console;
mod discover;
mod home;
mod notifications;
mod password;
mod series;
mod watchlist;

pub(crate) use account::{Account, AnilistCallback};
pub(crate) use auth::{Login, VerifyEmail};
pub(crate) use console::Console;
pub(crate) use discover::{Discover, Search};
pub(crate) use home::Home;
pub(crate) use notifications::Notifications;
pub(crate) use password::{ForgotPassword, ResetPassword};
pub(crate) use series::Series;
pub(crate) use watchlist::Watchlist;

use dioxus::prelude::*;

/// Catch-all 404 (design §17.3: error states name what failed).
#[component]
pub(crate) fn NotFound(segments: Vec<String>) -> Element {
    let path = segments.join("/");
    rsx! {
        h1 { class: "ik-page-title", "Lost the thread" }
        div { class: "ik-empty",
            p { "There's nothing at /{path}." }
            Link { to: crate::Route::Home {}, class: "ik-btn primary", "Back to Home" }
        }
    }
}
