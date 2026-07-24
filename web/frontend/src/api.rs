//! Typed API client (design §17.4). Wrapper over the generated `tankovault-api-client`.
//!
//! targets the API service under the same origin (`/v1/...`) via `Client`.

use dioxus::prelude::*;
use gloo_timers::future::TimeoutFuture;
use progenitor_client::{ClientInfo, Error as ApiOpError};
use serde::Serialize;
use std::cell::RefCell;
use std::rc::Rc;
use std::time::Duration;
use tankovault_api_client::Client;

pub type ApiResult<T> = Result<T, String>;

/// Total per-request deadline for the untyped helpers below. Long enough for a cold adapter
/// dry-run or forwarded sync call, short enough that a dead connection surfaces as an error
/// instead of an indefinitely spinning UI. WASM `reqwest` has no client-wide timeout, so the
/// deadline is applied per request (see [`fetch_json`]/[`post_json`]).
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

/// Extra attempts for an idempotent GET after a *transport* failure (never on an HTTP error
/// status). Smooths over transient blips — a dropped socket or a brief offline window —
/// without masking a real server response.
const GET_RETRIES: u32 = 2;

/// App-wide API context (same-origin). Holds the base client for its URL plus a memoised HTTP
/// client, so `use_api` can hand out a ready, authenticated [`Client`] without rebuilding one
/// on every render.
#[derive(Clone)]
pub struct ApiClient {
    base: Client,
    /// Memoised `reqwest` client keyed by the bearer token it carries. `use_api` runs on
    /// virtually every render across dozens of components; rebuilding a fresh client (and its
    /// header map) each time is pure waste. A built `reqwest::Client` is an `Arc` internally, so
    /// cloning it is cheap — we build one per distinct token and hand out clones, rebuilding only
    /// when the token actually changes (sign-in, refresh, sign-out). WASM is single-threaded, so
    /// `Rc<RefCell<_>>` is the right, allocation-light shared cell here.
    http_cache: Rc<RefCell<HttpCache>>,
}

/// The single cached HTTP client and the token it was built for.
struct HttpCache {
    token: Option<String>,
    client: reqwest::Client,
}

/// Build a `reqwest` client that attaches `token` (if any) as a `Bearer` `Authorization` header
/// on every request. A malformed token yields an unauthenticated client (the server then answers
/// 401) rather than panicking and taking the whole SPA down.
fn build_http_client(token: Option<&str>) -> reqwest::Client {
    let mut builder = reqwest::ClientBuilder::new();
    if let Some(token) = token {
        if let Ok(value) = reqwest::header::HeaderValue::from_str(&format!("Bearer {token}")) {
            let mut headers = reqwest::header::HeaderMap::new();
            headers.insert(reqwest::header::AUTHORIZATION, value);
            builder = builder.default_headers(headers);
        }
    }
    // The WASM builder only stores headers, so `build` is infallible here.
    builder
        .build()
        .expect("wasm reqwest client build is infallible")
}

pub fn use_api() -> Client {
    let api = use_context::<ApiClient>();
    let session = crate::state::use_session();
    let token = session.token_value();

    let http = {
        let mut cache = api.http_cache.borrow_mut();
        if cache.token != token {
            cache.client = build_http_client(token.as_deref());
            cache.token = token;
        }
        // Cheap `Arc` clone; the browser owns the underlying connection pool, so every clone
        // shares the same keep-alive connections.
        cache.client.clone()
    };

    Client::new_with_client(api.base.baseurl(), http)
}

pub fn provide_api() {
    use_context_provider(|| ApiClient {
        base: Client::new(&api_base_url()),
        http_cache: Rc::new(RefCell::new(HttpCache {
            token: None,
            client: build_http_client(None),
        })),
    });
}

/// Absolute base URL for API calls. The SPA is served from the same origin as the API
/// (design §19), so we target that origin's `/v1/...` paths. Unlike the browser's own
/// `fetch` (used by the old `gloo-net` client), reqwest parses the request URL up front and
/// rejects a relative path such as `/v1/auth/login` with a "builder error"; handing it the
/// concrete origin (e.g. `https://app.example`) keeps every request same-origin while giving
/// reqwest an absolute URL to parse. Falls back to an empty base if the window/location is
/// unavailable (e.g. non-browser targets).
fn api_base_url() -> String {
    web_sys::window()
        .and_then(|window| window.location().origin().ok())
        .unwrap_or_default()
}

pub fn friendly_error<E: std::fmt::Debug>(err: ApiOpError<E>) -> String {
    match err {
        ApiOpError::ErrorResponse(resp) => {
            let status = resp.status();
            match status.as_u16() {
                401 => "You need to sign in to do that.".to_owned(),
                403 => "You don't have permission to do that.".to_owned(),
                404 => "Not found.".to_owned(),
                409 => "That conflicts with the current state.".to_owned(),
                s if s >= 500 => "The server had a problem. Please retry.".to_owned(),
                _ => format!("Request failed ({status})."),
            }
        }
        _ => format!("Network error: {:?}", err),
    }
}

/// Fetch untyped JSON from a relative path using the pre-configured Client's identity.
pub async fn fetch_json(client: &Client, path: &str) -> ApiResult<serde_json::Value> {
    let http = client.client();
    let url = format!("{}{}", client.baseurl(), path);

    // GET is idempotent: retry only a transport error (timeout, dropped socket) with a short
    // linear backoff. An HTTP error status is a real answer from the server and is returned as
    // soon as it arrives — never retried.
    let mut attempt = 0u32;
    let response = loop {
        match http.get(url.as_str()).timeout(REQUEST_TIMEOUT).send().await {
            Ok(response) => break response,
            Err(_) if attempt < GET_RETRIES => {
                attempt += 1;
                TimeoutFuture::new(200 * attempt).await;
            }
            Err(e) => return Err(format!("Network error: {e}")),
        }
    };

    response
        .error_for_status()
        .map_err(|e| format!("Request failed: {e}"))?
        .json::<serde_json::Value>()
        .await
        .map_err(|e| format!("JSON error: {e}"))
}

/// POST JSON to a relative path and decode an untyped JSON body. Used when the generated
/// client types the success body as `()` (OpenAPI omitted the schema) but the backend still
/// returns a useful payload — e.g. adapter dry-run samples and forwarded sync endpoints.
pub async fn post_json<B: Serialize + ?Sized>(
    client: &Client,
    path: &str,
    body: &B,
) -> ApiResult<serde_json::Value> {
    let http = client.client();
    let url = format!("{}{}", client.baseurl(), path);
    http.post(url)
        .json(body)
        .timeout(REQUEST_TIMEOUT)
        .send()
        .await
        .map_err(|e| format!("Network error: {e}"))?
        .error_for_status()
        .map_err(|e| format!("Request failed: {e}"))?
        .json::<serde_json::Value>()
        .await
        .map_err(|e| format!("JSON error: {e}"))
}

pub fn stream_url(token: &str) -> String {
    format!("/v1/me/stream?token={token}")
}
