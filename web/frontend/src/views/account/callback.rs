//! The OAuth landing page for an external tracker's consent redirect.
//!
//! The provider redirects here after the reader approves (or declines). That full-page round
//! trip wipes the SPA's in-memory session, so this waits for the boot-time silent refresh in
//! [`crate::components::Shell`] to restore an access token before calling the
//! bearer-authenticated link endpoint — otherwise the exchange would always 401 on a cold
//! return from the provider.

use crate::api;
use crate::state::use_session;
use crate::Route;
use dioxus::prelude::*;

/// The provider this callback route is registered for. The sync service's `redirect_uri` is
/// configured per provider; only `AniList` is registered today.
const PROVIDER: &str = "anilist";

#[component]
pub(crate) fn AnilistCallback(code: String) -> Element {
    let session = use_session();
    let api = api::use_api();
    let nav = use_navigator();
    let mut outcome = use_signal(|| Option::<Result<(), String>>::None);

    use_effect(move || {
        // Wait for the boot refresh to settle, and fire the exchange exactly once.
        if !*session.ready.read() || outcome.peek().is_some() {
            return;
        }
        let code = code.clone();
        let client = api.client();
        spawn(async move {
            let result = match session.token_value() {
                None => Err(
                    "Sign in, then connect the provider again from Account → Sync & integrations."
                        .to_owned(),
                ),
                Some(_) if code.trim().is_empty() => {
                    Err("The provider did not return an authorization code.".to_owned())
                }
                Some(_) => client
                    .sync_callback()
                    .provider(PROVIDER)
                    .code(code)
                    .send()
                    .await
                    .map(|_| ())
                    .map_err(api::friendly_error),
            };
            let succeeded = result.is_ok();
            outcome.set(Some(result));
            if succeeded {
                nav.push(Route::Account {});
            }
        });
    });

    // Bind before the `rsx!` so the signal borrow is released at the end of this statement
    // rather than living until the function's temporaries drop.
    let failure = match &*outcome.read() {
        Some(Err(message)) => Some(message.clone()),
        _ => None,
    };

    match failure {
        Some(message) => rsx! {
            div { class: "ik-empty",
                p { "Couldn't connect the provider: {message}" }
                Link { to: Route::Account {}, class: "ik-btn primary", "Back to Account" }
            }
        },
        None => rsx! {
            div { class: "ik-empty", "Connecting…" }
        },
    }
}
