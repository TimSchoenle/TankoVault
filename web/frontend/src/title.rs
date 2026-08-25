//! What the browser tab is called, per screen.
//!
//! The name is derived from the *route* rather than set by each screen, so a route nobody
//! remembered to annotate still gets its own title instead of keeping the previous page's.
//! Screens that know a name the route cannot spell — a series' own title — publish it through
//! [`PageTitle`].

use crate::i18n::{use_i18n, Translator};
use crate::Route;
use dioxus::prelude::*;

/// A title a screen supplies for itself, tagged with the route it describes.
///
/// The tag is the point. A screen publishes asynchronously — a series title arrives with its
/// fetch — so by the time one lands the reader may already be somewhere else. Comparing the tag
/// against the live route means a late or left-behind title is dropped rather than shown over
/// the screen that replaced it.
#[derive(Clone, Copy)]
pub(crate) struct PageTitle(Signal<Option<(Route, String)>>);

impl PageTitle {
    pub(crate) fn new() -> Self {
        Self(Signal::new(None))
    }

    /// Publish `title` as the tab name for as long as `route` is the one on screen.
    pub(crate) fn set(mut self, route: Route, title: String) {
        self.0.set(Some((route, title)));
    }

    /// The published title, if it was published for `route`.
    fn claimed_for(self, route: &Route) -> Option<String> {
        claimed_for(self.0.read().as_ref(), route).map(str::to_owned)
    }
}

/// The decision [`PageTitle::claimed_for`] makes, lifted out of the signal so it can be tested
/// without a reactive runtime.
fn claimed_for<'a>(published: Option<&'a (Route, String)>, route: &Route) -> Option<&'a str> {
    match published {
        Some((claimed, title)) if claimed == route => Some(title),
        _ => None,
    }
}

/// Keep `document.title` in step with the screen on display. Mount once, inside the router.
pub(crate) fn use_document_title() {
    let i18n = use_i18n();
    let route = use_route::<Route>();
    let published = use_context::<PageTitle>();

    let claimed = published.claimed_for(&route);
    let title = claimed
        .clone()
        .map_or_else(|| route_title(&route, i18n), |name| decorate(&name, i18n));
    use_effect(use_reactive!(|title| crate::platform::set_document_title(
        &title
    )));

    // The undecorated name, for the desktop build's app-drawn title bar. The window keeps the
    // decorated one, because that is what the taskbar and alt-tab show; repeating the brand in a
    // header sitting directly above the rail's own wordmark says nothing twice.
    #[cfg(feature = "desktop")]
    {
        let heading = claimed.unwrap_or_else(|| page_name(&route, i18n));
        use_effect(use_reactive!(|heading| {
            crate::platform::set_window_heading(&heading);
        }));
    }
}

/// The tab name for `route` alone, ignoring anything a screen has published.
fn route_title(route: &Route, i18n: Translator) -> String {
    // The landing page is named for the product, not for a section of it — and matching the
    // `<title>` in `index.html` means the tab doesn't visibly rename itself once WASM boots.
    if matches!(route, Route::Home {}) {
        return i18n.t("title.app");
    }
    decorate(&page_name(route, i18n), i18n)
}

/// The screen's own name, undecorated.
///
/// The browser tab wants it with the brand appended; the small-viewport top bar, which replaces
/// the rail's lit entry as the only "where am I", wants it bare.
pub(crate) fn page_name(route: &Route, i18n: Translator) -> String {
    match route {
        Route::Home {} => i18n.t("nav.home"),
        Route::Discover { .. } => i18n.t("nav.discover"),
        Route::Recommendations {} => i18n.t("nav.recommendations"),
        Route::Series { .. } => i18n.t("title.series"),
        Route::Watchlist { .. } => i18n.t("nav.watchlist"),
        Route::Notifications {} => i18n.t("nav.notifications"),
        Route::Account {} | Route::AnilistCallback { .. } => i18n.t("nav.account"),
        Route::Search { query } if !query.q.trim().is_empty() => {
            i18n.args("title.search", &[("query", query.q.trim())])
        }
        Route::Search { .. } => i18n.t("nav.search"),
        Route::Login {} => i18n.t("common.signIn"),
        Route::VerifyEmail { .. } => i18n.t("verifyEmail.heading"),
        Route::ForgotPassword {} => i18n.t("password.forgot.heading"),
        Route::ResetPassword { .. } => i18n.t("password.reset.heading"),
        Route::Console {} | Route::ConsoleSection { .. } => i18n.t("nav.console"),
        // Named generically until the document lands and publishes its own title through
        // `PageTitle` — the slug is operator configuration, so the route cannot spell it.
        Route::Legal { .. } => i18n.t("title.legal"),
        Route::Licenses {} => i18n.t("licenses.title"),
        Route::NotFound { .. } => i18n.t("notFound.title"),
    }
}

/// `"<screen> — TankoVault"`. The join and the brand live in the catalogue so a translation can
/// reorder them.
fn decorate(page: &str, i18n: Translator) -> String {
    i18n.args("title.template", &[("page", page)])
}

#[cfg(test)]
mod tests {
    use super::*;

    fn series(id: &str) -> Route {
        Route::Series { id: id.to_owned() }
    }

    /// A screen's own title only applies to the screen that published it.
    ///
    /// The series title arrives with its fetch, so a reader who navigates on before it lands
    /// leaves a published title behind. Without the route tag it would be shown over whatever
    /// screen replaced it — the tab reading `Blame! — TankoVault` on the watchlist.
    #[test]
    fn a_published_title_is_ignored_once_its_route_is_gone() {
        let published = (series("abc"), "Blame!".to_owned());

        assert_eq!(
            claimed_for(Some(&published), &series("abc")),
            Some("Blame!")
        );
        assert_eq!(claimed_for(Some(&published), &series("def")), None);
        assert_eq!(
            claimed_for(Some(&published), &Route::Notifications {}),
            None
        );
        assert_eq!(claimed_for(None, &series("abc")), None);
    }
}
