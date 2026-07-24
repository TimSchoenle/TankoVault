//! Typed API client (design §17.4). Wrapper over the generated `tankovault-api-client`.
//!
//! targets the API service under the same origin (`/v1/...`) via `Client`.

use dioxus::prelude::*;
use progenitor_client::{ClientInfo, Error as ApiOpError};
use serde::Serialize;
use tankovault_api_client::Client;

pub type ApiResult<T> = Result<T, String>;

/// App-wide base client (same-origin). `use_api` clones its base URL and attaches the
/// caller's bearer token for authenticated requests.
#[derive(Clone)]
pub struct ApiClient {
    base: Client,
}

pub fn use_api() -> Client {
    let base = use_context::<ApiClient>();
    let session = crate::state::use_session();

    let mut builder = reqwest::ClientBuilder::new();
    if let Some(token) = session.token_value() {
        let mut headers = reqwest::header::HeaderMap::new();
        let auth = format!("Bearer {token}");
        headers.insert(
            reqwest::header::AUTHORIZATION,
            reqwest::header::HeaderValue::from_str(&auth).unwrap(),
        );
        builder = builder.default_headers(headers);
    }

    let http_client = builder.build().unwrap();
    Client::new_with_client(base.base.baseurl(), http_client)
}

pub fn provide_api() {
    use_context_provider(|| ApiClient {
        base: Client::new(&api_base_url()),
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
    http.get(url)
        .send()
        .await
        .map_err(|e| format!("Network error: {e}"))?
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
