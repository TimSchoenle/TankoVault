//! The app's API access point (design §17.4) — a thin, `Copy` handle over the generated
//! `tankovault-api-client`, provided once at the router root.
//!
//! # Why a handle rather than a client
//!
//! The obvious shape — a hook returning a ready `Client` — bakes the bearer token in at
//! *render* time. That is subtly wrong for this app: on a page reload the SPA boots signed
//! out and adopts a token from the httpOnly refresh cookie a moment later
//! ([`crate::components::Shell`]). Anything holding a client built during that gap keeps
//! sending unauthenticated requests and 401s forever, even though the user is signed in.
//!
//! [`Api`] instead resolves the token on every [`Api::client`] call, so a request always
//! carries whatever the session holds *now*. Reading the token also subscribes the calling
//! reactive scope, which means a `use_resource` that builds its client in the synchronous
//! prologue automatically re-runs when the token changes — the boot-time refresh, a silent
//! renewal, a sign-in and a sign-out all refetch without any per-view wiring.
//!
//! [`Api`] is `Copy` (via [`CopyValue`]), so views capture it into any number of closures
//! without the clone-per-handler noise that a reference-counted client would force.

mod error;

pub(crate) use error::{error_status, friendly_error};

use crate::state::Session;
use dioxus::prelude::*;
use tankovault_api_client::Client;

/// A `Copy` handle that mints authenticated clients against the current session.
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
/// [`Api::client`] runs on virtually every render across dozens of components; rebuilding a
/// client and its header map each time is pure waste. A built `reqwest::Client` is an `Arc`
/// internally and the browser owns the connection pool behind it, so handing out clones is
/// cheap and every clone shares the same keep-alive connections. We rebuild only when the
/// token actually changes — sign-in, silent refresh, sign-out.
struct HttpCache {
    token: Option<String>,
    client: reqwest::Client,
}

impl Api {
    /// A client carrying the session's **current** access token.
    ///
    /// Call this wherever the request is actually issued rather than caching the result:
    /// in a render body, in a `use_resource` prologue, or inside a spawned handler. Reading
    /// the token subscribes the calling reactive scope (if there is one), which is what makes
    /// dependent resources refetch across a sign-in or a silent refresh.
    pub(crate) fn client(&self) -> Client {
        let token = self.session.token.read();
        self.client_for(token.as_deref())
    }

    /// The absolute origin every request is sent to. The SSE subscription needs it directly,
    /// because `EventSource` is built by the browser rather than by the generated client.
    pub(crate) fn base_url(&self) -> String {
        self.base.read().clone()
    }

    /// A client carrying an explicit token — for the rare call that must pin one.
    fn client_for(&self, token: Option<&str>) -> Client {
        let http = {
            // `CopyValue` is `Copy`, so a local rebind is all that's needed to write through
            // a shared `&self` — no interior-mutability ceremony, and no `&mut self` leaking
            // into every call site.
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
    let mut builder = reqwest::ClientBuilder::new();
    if let Some(token) = token {
        if let Ok(value) = reqwest::header::HeaderValue::from_str(&format!("Bearer {token}")) {
            let mut headers = reqwest::header::HeaderMap::new();
            headers.insert(reqwest::header::AUTHORIZATION, value);
            builder = builder.default_headers(headers);
        }
    }
    // The WASM builder only stores headers, so `build` cannot fail here.
    builder
        .build()
        .expect("wasm reqwest client build is infallible")
}

/// Absolute base URL for API calls.
///
/// The SPA is served from the same origin as the API (design §19), so we target that
/// origin's `/v1/...` paths. Unlike the browser's own `fetch`, reqwest parses the request URL
/// up front and rejects a relative path such as `/v1/auth/login` with a builder error, so it
/// needs the concrete origin. Falls back to an empty base outside a browser.
fn origin() -> String {
    web_sys::window()
        .and_then(|window| window.location().origin().ok())
        .unwrap_or_default()
}

/// URL of the per-user SSE notification stream.
///
/// The token rides in the query string because the browser's `EventSource` cannot set an
/// `Authorization` header.
pub(crate) fn stream_url(token: &str) -> String {
    format!("/v1/me/stream?token={token}")
}
