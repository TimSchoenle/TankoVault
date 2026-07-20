//! Routed screens (design §17.2). Each is a Dioxus component named to match a `Route`
//! variant in `main.rs`.

mod auth;
mod console;
mod discover;
mod notifications;
mod reading;
mod series;
mod watchlist;

pub use auth::Login;
pub use console::Console;
pub use discover::{Discover, Search};
pub use notifications::Notifications;
pub use reading::Reading;
pub use series::Series;
pub use watchlist::Watchlist;

use dioxus::prelude::*;

/// Catch-all 404 (design §17.3: error states name what failed).
#[component]
pub fn NotFound(segments: Vec<String>) -> Element {
    let path = segments.join("/");
    rsx! {
        h1 { class: "ik-page-title", "Lost the thread" }
        div { class: "ik-empty",
            p { "There's nothing at /{path}." }
            Link { to: crate::Route::Discover {}, class: "ik-btn primary", "Back to Discover" }
        }
    }
}
