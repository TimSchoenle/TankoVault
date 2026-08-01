//! The OAuth landing page for an external tracker's consent redirect.
//!
//! The full-page round trip wipes the SPA's in-memory session, so this waits for the boot-time
//! silent refresh in [`crate::components::Shell`] to restore a token before calling the
//! bearer-authenticated link endpoint — otherwise the exchange always 401s on a cold return.

use crate::api;
use crate::components::EmptyBox;
use crate::i18n::use_i18n;
use crate::state::use_session;
use crate::Route;
use dioxus::prelude::*;

/// Provider this callback route is registered for; only `AniList` today.
const PROVIDER: &str = "anilist";

#[component]
pub(crate) fn AnilistCallback(code: String) -> Element {
    let session = use_session();
    let i18n = use_i18n();
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
                None => Err(i18n.t("account.callback.signedOut")),
                Some(_) if code.trim().is_empty() => Err(i18n.t("account.callback.noCode")),
                Some(_) => client
                    .sync_callback()
                    .provider(PROVIDER)
                    .code(code)
                    .send()
                    .await
                    .map(|_| ())
                    .map_err(|e| api::friendly_error(i18n, e)),
            };
            let succeeded = result.is_ok();
            outcome.set(Some(result));
            if succeeded {
                nav.push(Route::Account {});
            }
        });
    });

    // Bind before `rsx!` so the borrow drops here, not at function end.
    let failure = match &*outcome.read() {
        Some(Err(message)) => Some(message.clone()),
        _ => None,
    };

    match failure {
        Some(message) => rsx! {
            div { class: "ik-empty",
                p { {i18n.args("account.callback.failed", &[("message", &message)])} }
                Link { to: Route::Account {}, class: "ik-btn primary",
                    {i18n.t("account.callback.back")}
                }
            }
        },
        None => rsx! {
            EmptyBox { message: i18n.t("account.callback.connecting") }
        },
    }
}
