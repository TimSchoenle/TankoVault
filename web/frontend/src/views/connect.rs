//! First run on the desktop build: which `TankoVault` server is this copy for?
//!
//! The web SPA is served *by* the API it talks to, so `location.origin` answers this and the
//! question never comes up. A desktop binary is shipped by nobody in particular and has to be
//! told, once — that is all this screen is.
//!
//! It runs *instead of* the router rather than as a route, because every screen behind the
//! router issues requests, and there is nowhere to send them until this is answered.

use super::auth::AuthBrand;
use crate::components::{Field, PanelCard};
use crate::i18n::use_i18n;
use crate::icons::Icon;
use dioxus::prelude::*;
use tankovault_api_client::Client;

/// Ask for a server, prove it answers, and store it.
///
/// `on_connected` is called with the accepted origin; the caller re-renders into the app.
#[component]
pub(crate) fn ConnectServer(on_connected: EventHandler<String>) -> Element {
    let i18n = use_i18n();
    let mut entered = use_signal(String::new);
    let mut error = use_signal(|| Option::<String>::None);
    let mut probing = use_signal(|| false);

    let mut connect = move |()| {
        if *probing.peek() {
            return;
        }
        let candidate = match normalise(&entered.peek().clone()) {
            Ok(origin) => origin,
            Err(key) => {
                error.set(Some(i18n.t(key)));
                return;
            }
        };
        error.set(None);
        probing.set(true);
        spawn(async move {
            match probe(&candidate).await {
                Ok(()) => {
                    crate::platform::set_server_origin(Some(&candidate));
                    on_connected.call(candidate);
                }
                Err(key) => {
                    error.set(Some(i18n.t(key)));
                    probing.set(false);
                }
            }
        });
    };

    rsx! {
        div { class: "ik-auth",
            AuthBrand {}
            h1 { {i18n.t("connect.heading")} }
            p { class: "ik-muted", {i18n.t("connect.subtitle")} }

            if let Some(message) = error.read().clone() {
                div { class: "ik-error", style: "padding:12px;margin:14px 0;text-align:left;",
                    "{message}"
                }
            }

            Field {
                id: "tv-connect-origin",
                label: i18n.t("connect.field.server"),
                kind: "url",
                placeholder: "https://tankovault.example.com",
                hint: i18n.t("connect.field.hint"),
                value: entered(),
                on_input: move |value| entered.set(value),
                on_enter: connect,
            }

            button {
                class: "ik-btn primary",
                style: "width:100%;",
                r#type: "button",
                disabled: probing(),
                onclick: move |_| connect(()),
                if probing() {
                    {i18n.t("connect.connecting")}
                } else {
                    {i18n.t("connect.action")}
                }
            }

            if let Some(path) = crate::platform::settings_path() {
                p { class: "ik-muted", style: "font-size:12px;margin-top:18px;word-break:break-all;",
                    {i18n.args("connect.storedAt", &[("path", &path.display().to_string())])}
                }
            }
        }
    }
}

/// Change the server after first run, from the appearance panel.
///
/// Signing out is not optional here and is not a courtesy: the access token in memory was minted
/// by the *old* server and means nothing to the new one, so keeping it would send a stranger's
/// deployment a credential and then show the reader a wall of 401s it could not explain.
#[component]
pub(crate) fn ServerCard() -> Element {
    let i18n = use_i18n();
    let api = crate::api::use_api();
    let session = crate::state::use_session();
    let current = crate::platform::server_origin().unwrap_or_default();
    let mut entered = use_signal(|| current.clone());
    let mut error = use_signal(|| Option::<String>::None);
    let mut probing = use_signal(|| false);

    let mut change = move |()| {
        if *probing.peek() {
            return;
        }
        let candidate = match normalise(&entered.peek().clone()) {
            Ok(origin) => origin,
            Err(key) => {
                error.set(Some(i18n.t(key)));
                return;
            }
        };
        error.set(None);
        probing.set(true);
        spawn(async move {
            match probe(&candidate).await {
                Ok(()) => {
                    crate::platform::set_server_origin(Some(&candidate));
                    api.set_base(&candidate);
                    session.clear();
                    probing.set(false);
                }
                Err(key) => {
                    error.set(Some(i18n.t(key)));
                    probing.set(false);
                }
            }
        });
    };

    rsx! {
        PanelCard { icon: Icon::CloudSync, title: i18n.t("connect.card.title"),
            p { class: "ik-muted", style: "font-size:13px;margin-top:0;",
                {i18n.t("connect.card.intro")}
            }
            if let Some(message) = error.read().clone() {
                div { class: "ik-error", style: "padding:10px;margin-bottom:12px;", "{message}" }
            }
            Field {
                id: "tv-settings-origin",
                label: i18n.t("connect.field.server"),
                kind: "url",
                value: entered(),
                on_input: move |value| entered.set(value),
                on_enter: change,
            }
            button {
                class: "ik-btn",
                r#type: "button",
                disabled: probing() || *entered.read() == current,
                onclick: move |_| change(()),
                if probing() {
                    {i18n.t("connect.connecting")}
                } else {
                    {i18n.t("connect.card.action")}
                }
            }
        }
    }
}

/// Accept what a person would actually type and reject what cannot carry a bearer token safely.
///
/// A bare host gets `https://`, never `http://`: the access token rides an `Authorization`
/// header on every request, and defaulting to plaintext would put it on the wire because
/// somebody left a scheme off. Plain HTTP stays *reachable* — a self-hoster developing against
/// their own box needs it — but only when typed in full and only for a loopback host, which is
/// the same line the browser draws for a secure context.
///
/// # Errors
/// A **catalogue key**, not a sentence.
fn normalise(entered: &str) -> Result<String, &'static str> {
    let trimmed = entered.trim();
    if trimmed.is_empty() {
        return Err("connect.error.empty");
    }
    let with_scheme = if trimmed.contains("://") {
        trimmed.to_owned()
    } else {
        format!("https://{trimmed}")
    };
    let Some((scheme, rest)) = with_scheme.split_once("://") else {
        return Err("connect.error.malformed");
    };
    // Trailing slashes come off *after* the split, not before: stripping them from the whole
    // string first eats the `//` of the separator, and `https://` then round-trips into
    // `https://https:` — a value that would be sent as a base URL. See the test.
    let rest = rest.trim_end_matches('/');
    if rest.is_empty() {
        return Err("connect.error.malformed");
    }
    let origin = format!("{scheme}://{rest}");
    match scheme {
        "https" => Ok(origin),
        "http" if is_loopback(rest) => Ok(origin),
        "http" => Err("connect.error.insecure"),
        _ => Err("connect.error.malformed"),
    }
}

/// Whether the authority names this machine, so plain HTTP never leaves it.
///
/// The host is taken up to the first `:`, `/` or `?`, and matched whole — `localhost.example.com`
/// is somebody else's server and must not be treated as loopback.
fn is_loopback(rest: &str) -> bool {
    let authority = rest.split(['/', '?', '#']).next().unwrap_or_default();
    let host = match authority.split_once(']') {
        // A bracketed IPv6 literal keeps its brackets; whatever follows `]` is the port.
        Some((bracketed, _)) => format!("{bracketed}]"),
        None => authority
            .split_once(':')
            .map_or(authority, |(host, _)| host)
            .to_owned(),
    };
    matches!(host.as_str(), "localhost" | "127.0.0.1" | "[::1]")
}

/// Ask the candidate for its public legal index — the cheapest unauthenticated call this build
/// makes — and accept the server only if the answer is the shape this build expects.
///
/// Through the generated client on purpose: a URL that answers but is not a `TankoVault` of a
/// compatible version fails *here*, named, instead of turning into a screen full of empty lists
/// after the reader has signed in.
///
/// # Errors
/// A **catalogue key**, not a sentence.
async fn probe(origin: &str) -> Result<(), &'static str> {
    use progenitor_client::Error;

    match Client::new(origin).legal_index().send().await {
        Ok(_) => Ok(()),
        // It answered, but not with what this build asked for: a different product, a reverse
        // proxy's error page, or an API too old or too new to talk to.
        Err(
            Error::ErrorResponse(_)
            | Error::UnexpectedResponse(_)
            | Error::InvalidResponsePayload(..),
        ) => Err("connect.error.notTankovault"),
        Err(_) => Err("connect.error.unreachable"),
    }
}

#[cfg(test)]
mod tests {
    use super::normalise;

    /// A host typed without a scheme must become `https`, and plain `http` must be refused for
    /// anything but this machine.
    ///
    /// The bug this pins would be a silent one: the access token is sent as an `Authorization`
    /// header on every request, so a server accepted over plain HTTP puts it on the wire in
    /// clear text for the lifetime of the session — and nothing in the UI would say so.
    #[test]
    fn a_server_is_only_accepted_over_https_or_on_this_machine() {
        assert_eq!(
            normalise("tankovault.example.com").as_deref(),
            Ok("https://tankovault.example.com")
        );
        assert_eq!(
            normalise("  https://tankovault.example.com/  ").as_deref(),
            Ok("https://tankovault.example.com")
        );
        assert_eq!(
            normalise("http://localhost:8080").as_deref(),
            Ok("http://localhost:8080")
        );
        assert_eq!(
            normalise("http://127.0.0.1:8080").as_deref(),
            Ok("http://127.0.0.1:8080")
        );

        assert_eq!(
            normalise("http://tankovault.example.com"),
            Err("connect.error.insecure")
        );
        // Not loopback: the suffix is somebody else's domain.
        assert_eq!(
            normalise("http://localhost.example.com"),
            Err("connect.error.insecure")
        );
        assert_eq!(normalise("   "), Err("connect.error.empty"));
        assert_eq!(
            normalise("ftp://example.com"),
            Err("connect.error.malformed")
        );
        assert_eq!(normalise("https://"), Err("connect.error.malformed"));
    }
}
