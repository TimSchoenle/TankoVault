//! The app's API access point (design §17.4) — a thin, `Copy` handle over the generated
//! `tankovault-api-client`, provided once at the router root.

mod error;
#[cfg(feature = "desktop")]
pub(crate) mod session_store;

pub(crate) use error::{error_status, friendly_error, problem_detail};

use crate::state::Session;
use dioxus::prelude::*;
use tankovault_api_client::Client;

/// A `Copy` handle that mints authenticated clients against the current session.
///
/// Resolves the token fresh on every [`Api::client`] call rather than caching a built `Client`:
/// on boot the SPA starts signed out and adopts a token from the refresh cookie moments later
/// ([`crate::components::Shell`]), so a cached client from that gap would 401 forever.
#[derive(Clone, Copy)]
pub(crate) struct Api {
    /// Absolute origin the API is served from — see [`origin`].
    base: CopyValue<String>,
    /// The live session; read on every [`Api::client`] call for the current token.
    session: Session,
    /// The memoised HTTP client and the token it carries.
    http: CopyValue<HttpCache>,
}

/// The single cached `reqwest` client, plus the token it was built with.
///
/// Rebuilt only when the token changes; `Client` clones are cheap, rebuilding isn't.
struct HttpCache {
    token: Option<String>,
    client: reqwest::Client,
}

impl Api {
    /// A client carrying the session's **current** access token.
    ///
    /// Call at the point the request is issued, not cached — see the struct-level doc.
    pub(crate) fn client(&self) -> Client {
        let token = self.session.token.read();
        self.client_for(token.as_deref())
    }

    /// The absolute origin every request is sent to. The SSE subscription needs it directly,
    /// because `EventSource` is built by the browser rather than by the generated client.
    pub(crate) fn base_url(&self) -> String {
        self.base.read().clone()
    }

    /// A client that also presents a step-up grant, for a call the API guards with one.
    ///
    /// Built fresh rather than memoised, unlike [`Self::client`]: an elevation is short-lived and
    /// is presented by a handful of calls, so caching it would mean holding an expiring
    /// credential in a slot the *unelevated* path also reads from — and a stale one there would
    /// attach an elevation to every request in the app.
    pub(crate) fn elevated_client(&self, step_up: &str) -> Client {
        let token = self.session.token.read();
        let http = build_http_client_with(token.as_deref(), Some(step_up));
        Client::new_with_client(&self.base.read(), http)
    }

    /// A client carrying an explicit token — for the rare call that must pin one.
    fn client_for(&self, token: Option<&str>) -> Client {
        let http = {
            // `CopyValue` is `Copy`; rebinding it locally is enough to write through `&self`.
            let mut slot = self.http;
            let mut cache = slot.write();
            if cache.token.as_deref() != token {
                cache.client = build_http_client(token);
                cache.token = token.map(str::to_owned);
            }
            cache.client.clone()
        };
        Client::new_with_client(&self.base.read(), http)
    }

    /// Point this handle at `origin`, once the reader has chosen a server.
    ///
    /// Desktop only: the web build is served *by* its API and reads the origin off the document,
    /// so there is nothing to choose and nothing to re-point.
    #[cfg(feature = "desktop")]
    pub(crate) fn set_base(&self, origin: &str) {
        let mut base = self.base;
        base.set(origin.to_owned());
    }
}

/// Provide the API handle. Call once, inside the component that already provided the
/// [`Session`] this reads from.
pub(crate) fn provide_api() {
    let session = crate::state::use_session();
    use_context_provider(|| Api {
        base: CopyValue::new(origin()),
        session,
        http: CopyValue::new(HttpCache {
            token: None,
            client: build_http_client(None),
        }),
    });
}

/// The API handle for any descendant component.
pub(crate) fn use_api() -> Api {
    use_context::<Api>()
}

/// Build a `reqwest` client that attaches `token` (if any) as a `Bearer` `Authorization`
/// header on every request. A malformed token yields an unauthenticated client — the server
/// then answers 401 — rather than panicking and taking the whole SPA down.
fn build_http_client(token: Option<&str>) -> reqwest::Client {
    build_http_client_with(token, None)
}

/// The header a step-up grant is presented in. Must match `tankovault_api::STEP_UP_HEADER`;
/// this crate is a separate workspace and shares no types with the API, so `openapi.json` and
/// this constant are the only connectors — the same arrangement every other route detail here
/// lives under.
const STEP_UP_HEADER: &str = "x-step-up";

/// [`build_http_client`], optionally also attaching a step-up grant.
fn build_http_client_with(token: Option<&str>, step_up: Option<&str>) -> reqwest::Client {
    let mut builder = reqwest::ClientBuilder::new();
    let mut headers = reqwest::header::HeaderMap::new();
    if let Some(token) = token {
        if let Ok(value) = reqwest::header::HeaderValue::from_str(&format!("Bearer {token}")) {
            headers.insert(reqwest::header::AUTHORIZATION, value);
        }
    }
    if let Some(step_up) = step_up {
        if let Ok(value) = reqwest::header::HeaderValue::from_str(step_up) {
            if let Ok(name) = reqwest::header::HeaderName::from_bytes(STEP_UP_HEADER.as_bytes()) {
                headers.insert(name, value);
            }
        }
    }
    if !headers.is_empty() {
        builder = builder.default_headers(headers);
    }
    // Web needs none of this: requests go through the window's own `fetch`, so the browser's
    // cookie store handles the refresh cookie and is already scoped, persisted and protected.
    #[cfg(feature = "desktop")]
    {
        builder = builder.cookie_provider(session_store::session_jar());
    }
    builder
        .build()
        .expect("the client is configured with headers and a cookie jar, neither of which fails")
}

/// Forget the persisted refresh credential, because there is no longer a session it belongs to.
///
/// Called from [`crate::state::Session::clear`] — the one place in the app that means "this
/// session is over", whether that was a sign-out, a `401` from refresh, a deleted account or a
/// re-pointed server. Hooking each of those instead would be four chances to miss one, and a
/// missed one leaves a live credential in the OS store after the reader believes they signed out.
#[cfg(feature = "desktop")]
pub(crate) fn forget_session() {
    session_store::session_jar().forget();
}

/// Absolute base URL for API calls (design §19: same origin as the API on web, the configured
/// server on desktop). See [`crate::platform::origin`].
fn origin() -> String {
    crate::platform::origin()
}

/// URL of the per-user SSE notification stream, for a ticket from `POST /v1/me/stream-ticket`.
///
/// The credential rides in the query string because `EventSource` cannot set an `Authorization`
/// header, so it's a single-use, 30-second ticket rather than the access token — a query string
/// ends up in access logs and browser history, so it must be worthless once read back.
///
/// Hand-built since `EventSource` bypasses the generated client, so no compiler checks this
/// against the API; see [`tests::the_stream_url_uses_the_parameter_the_published_document_declares`].
pub(crate) fn stream_url(ticket: &str) -> String {
    format!("/v1/me/stream?ticket={ticket}")
}

/// URL of the operator console's SSE stream, for a ticket from the same mint.
///
/// Same credential and the same reason for it as [`stream_url`]; a different stream because the
/// payloads and their cadences are different, and because what it may carry depends on the
/// caller's permissions rather than on their identity.
pub(crate) fn admin_stream_url(ticket: &str) -> String {
    format!("/v1/admin/stream?ticket={ticket}")
}

#[cfg(test)]
mod tests {
    /// Pins the bug where this sent `?token=` while the API read `access_token=`, silently
    /// breaking live notifications since a failed stream degrades silently by design.
    ///
    /// Checked against the committed `openapi.json`, the same artefact the API client generates from.
    #[test]
    fn the_stream_url_uses_the_parameter_the_published_document_declares() {
        const SPEC: &str = include_str!("../../../../openapi.json");
        let spec: serde_json::Value = serde_json::from_str(SPEC).expect("openapi.json parses");

        let parameters = spec["paths"]["/v1/me/stream"]["get"]["parameters"]
            .as_array()
            .expect("the stream declares query parameters")
            .clone();
        let name = parameters
            .iter()
            .find(|p| p["in"] == "query")
            .and_then(|p| p["name"].as_str())
            .expect("a query parameter")
            .to_owned();

        let url = super::stream_url("TICKET");
        assert!(
            url.contains(&format!("{name}=TICKET")),
            "the API reads `{name}`, this crate sends `{url}`"
        );

        // The mint endpoint must exist too, or `live::run` has nothing to call.
        assert!(
            spec["paths"]["/v1/me/stream-ticket"]["post"].is_object(),
            "the published document must offer the endpoint that mints the ticket"
        );
    }

    /// The console stream is hand-built for the same reason and pinned the same way — and it
    /// redeems a ticket from the *me* mint, so a rename on either side has to show up here.
    #[test]
    fn the_console_stream_url_uses_the_parameter_the_published_document_declares() {
        const SPEC: &str = include_str!("../../../../openapi.json");
        let spec: serde_json::Value = serde_json::from_str(SPEC).expect("openapi.json parses");

        let name = spec["paths"]["/v1/admin/stream"]["get"]["parameters"]
            .as_array()
            .expect("the console stream declares query parameters")
            .iter()
            .find(|p| p["in"] == "query")
            .and_then(|p| p["name"].as_str())
            .expect("a query parameter")
            .to_owned();

        let url = super::admin_stream_url("TICKET");
        assert!(
            url.contains(&format!("{name}=TICKET")),
            "the API reads `{name}`, this crate sends `{url}`"
        );
    }
}
