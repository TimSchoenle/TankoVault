//! # frontend service
//!
//! Serves the compiled Dioxus WASM single-page app and reverse-proxies `/v1/*` (REST + SSE)
//! to the `api` service from a single origin, replacing the previous nginx image. Like every
//! other backend binary it is a fully static musl build shipped on a bare `scratch` image
//! (see `deploy/docker/Dockerfile.frontend`).
//!
//! ## Why one origin
//!
//! The WASM client issues same-origin `/v1/...` requests (`web/frontend/src/api.rs`) and opens
//! the live-notification stream (`/v1/me/stream`) via the browser `EventSource` API. Serving
//! the SPA and proxying `/v1/*` from the same origin is what makes those calls resolve without
//! a cross-origin hop — no CORS — and the proxy streams responses unbuffered so Server-Sent
//! Events flush to the browser the instant the API emits them.
//!
//! ## Feature parity with the retired nginx config
//!
//! - `GET /healthz` — a plain-text liveness endpoint.
//! - `/v1/*` — streaming reverse proxy to the API, forwarding `X-Forwarded-For` / `X-Real-IP`
//!   / `X-Forwarded-Proto` so the API's rate limiter and audit trail see the real client, with
//!   no request timeout on the proxied leg so long-lived SSE streams stay open.
//! - everything else — the static bundle with SPA fallback to `index.html`, carrying the
//!   baseline hardening headers (`X-Content-Type-Options`, `Referrer-Policy`, `X-Frame-Options`)
//!   on the app shell only, so proxied API responses keep their own headers.

use std::net::SocketAddr;
use std::time::Duration;

use axum::Router;
use axum::body::Body;
use axum::extract::{ConnectInfo, Request, State};
use axum::http::{HeaderMap, HeaderName, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{any, get};
use serde::Deserialize;
use tankovault_config::TelemetryConfig;
use tower::ServiceBuilder;
use tower_http::compression::CompressionLayer;
use tower_http::services::{ServeDir, ServeFile};
use tower_http::set_header::SetResponseHeaderLayer;
use tower_http::trace::TraceLayer;

#[derive(Debug, Deserialize)]
struct Config {
    #[serde(default = "default_bind")]
    bind_addr: String,
    telemetry: TelemetryConfig,
    #[serde(default)]
    frontend: FrontendConfig,
}

/// Non-privileged by design: the `scratch` image runs as a numeric nonroot user, which cannot
/// bind the reserved port 80 the nginx image used. The compose stack maps host `3000` here.
fn default_bind() -> String {
    "0.0.0.0:3000".to_owned()
}

#[derive(Debug, Clone, Deserialize)]
struct FrontendConfig {
    /// Directory the built SPA bundle is served from. Baked into the image at `/srv/www`.
    #[serde(default = "FrontendConfig::default_static_dir")]
    static_dir: String,
    /// Base origin the `/v1/*` proxy targets, e.g. `http://api:8080`. No trailing slash.
    #[serde(default = "FrontendConfig::default_api_upstream")]
    api_upstream: String,
    /// Largest proxied request body buffered before forwarding. The API enforces its own
    /// (smaller) cap; this is only a memory guard on this hop.
    #[serde(default = "FrontendConfig::default_max_body_bytes")]
    max_body_bytes: usize,
    /// Connection-establishment timeout for the upstream. Deliberately *not* a whole-request
    /// timeout: an SSE stream is a single request that stays open indefinitely.
    #[serde(default = "FrontendConfig::default_connect_timeout_secs")]
    connect_timeout_secs: u64,
}

impl FrontendConfig {
    fn default_static_dir() -> String {
        "/srv/www".to_owned()
    }
    fn default_api_upstream() -> String {
        "http://api:8080".to_owned()
    }
    fn default_max_body_bytes() -> usize {
        10 * 1024 * 1024
    }
    fn default_connect_timeout_secs() -> u64 {
        10
    }
}

impl Default for FrontendConfig {
    fn default() -> Self {
        Self {
            static_dir: Self::default_static_dir(),
            api_upstream: Self::default_api_upstream(),
            max_body_bytes: Self::default_max_body_bytes(),
            connect_timeout_secs: Self::default_connect_timeout_secs(),
        }
    }
}

/// Shared state for the `/v1/*` proxy: the reusable client, the upstream origin, and the body
/// cap. Cheap to clone (the client is internally reference-counted).
#[derive(Clone)]
struct AppState {
    client: reqwest::Client,
    upstream: String,
    max_body_bytes: usize,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cfg: Config = tankovault_config::load()?;
    tankovault_service::init_tracing(&cfg.telemetry)?;
    let shutdown = tankovault_service::install_shutdown();

    // A dedicated client for the proxy. Automatic gzip/brotli decompression is turned off so
    // the response body is forwarded byte-for-byte alongside its `Content-Encoding` header
    // rather than being silently decoded here and shipped with a now-wrong header.
    let client = reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(cfg.frontend.connect_timeout_secs))
        .gzip(false)
        .brotli(false)
        .build()?;

    let state = AppState {
        client,
        upstream: cfg.frontend.api_upstream.trim_end_matches('/').to_owned(),
        max_body_bytes: cfg.frontend.max_body_bytes,
    };

    tracing::info!(
        static_dir = %cfg.frontend.static_dir,
        upstream = %state.upstream,
        bind = %cfg.bind_addr,
        "frontend serving the SPA and proxying /v1/* to the api"
    );

    let app = build_router(&cfg.frontend.static_dir, state);
    tankovault_service::serve(&cfg.bind_addr, app, shutdown).await?;
    Ok(())
}

/// Content-Security-Policy for the SPA shell.
///
/// The access token lives only in memory, so the thing a CSP buys here is a hard ceiling on
/// where an injected script could send it — previously there was none, and any regression
/// that got script into the page (a compromised build artefact, a future
/// `dangerous_inner_html`) could exfiltrate it to an arbitrary origin with nothing to stop it.
///
/// - `script-src 'self' 'wasm-unsafe-eval'` — `wasm-unsafe-eval` is required: WebAssembly
///   instantiation is `eval`-shaped to the CSP engine, and without it the app does not boot.
///   It does **not** re-enable `eval()` for JavaScript.
/// - `connect-src 'self'` — the API is same-origin through this proxy by design, so this is
///   the exfiltration ceiling. A split-origin deployment must widen it.
/// - `img-src` allows any `https:` host and `data:` for remote cover art, which comes from
///   whichever provider a series is sourced from and cannot be enumerated.
const CSP: &str = "default-src 'self'; \
                   script-src 'self' 'wasm-unsafe-eval'; \
                   style-src 'self' 'unsafe-inline'; \
                   connect-src 'self'; \
                   img-src 'self' https: data:; \
                   font-src 'self' data:; \
                   object-src 'none'; \
                   base-uri 'none'; \
                   form-action 'self'; \
                   frame-ancestors 'none'";

/// Assemble the router: the health probe, the `/v1/*` proxy, and the static bundle (with SPA
/// fallback and hardening headers) catching everything else.
fn build_router(static_dir: &str, state: AppState) -> Router {
    // SPA fallback: any path with no matching file resolves to the app shell so client-side
    // routing (`/series/…`, `/account/…`) works on a hard refresh or a deep link.
    let index = format!("{}/index.html", static_dir.trim_end_matches('/'));
    let bundle = ServeDir::new(static_dir).fallback(ServeFile::new(index));

    // Baseline hardening, scoped to the app shell only. `if_not_present` never clobbers a
    // value a served file might already carry, and because this wraps only the static branch
    // the proxied `/v1/*` responses keep the API's own headers untouched.
    let static_service = ServiceBuilder::new()
        .layer(SetResponseHeaderLayer::if_not_present(
            header::X_CONTENT_TYPE_OPTIONS,
            HeaderValue::from_static("nosniff"),
        ))
        .layer(SetResponseHeaderLayer::if_not_present(
            header::REFERRER_POLICY,
            HeaderValue::from_static("no-referrer"),
        ))
        .layer(SetResponseHeaderLayer::if_not_present(
            header::X_FRAME_OPTIONS,
            HeaderValue::from_static("DENY"),
        ))
        .layer(SetResponseHeaderLayer::if_not_present(
            header::CONTENT_SECURITY_POLICY,
            HeaderValue::from_static(CSP),
        ))
        // The app shell must never be cached: it names the hashed bundle, so a stale copy
        // pins the client to a retired build. Hashed assets carry their own immutable
        // caching via `ServeDir`'s ETag/Last-Modified handling.
        .layer(SetResponseHeaderLayer::if_not_present(
            header::CACHE_CONTROL,
            HeaderValue::from_static("no-cache"),
        ))
        // The WASM bundle is 1-3 MB and shipped uncompressed until now — the largest single
        // cost of a cold load, and the API tier has had compression all along.
        .layer(CompressionLayer::new())
        .service(bundle);

    Router::new()
        .route("/healthz", get(healthz))
        .route("/v1/{*rest}", any(proxy))
        .fallback_service(static_service)
        .with_state(state)
        .layer(TraceLayer::new_for_http())
}

/// Container liveness target, matching the retired nginx `/healthz`.
async fn healthz() -> impl IntoResponse {
    ([(header::CONTENT_TYPE, "text/plain")], "ok\n")
}

/// Reverse-proxy a `/v1/*` request to the API and stream the response straight back.
///
/// The request body is buffered (the API's bodies are small JSON payloads), but the *response*
/// is streamed frame-by-frame via [`reqwest::Response::bytes_stream`], so Server-Sent Events
/// reach the browser as the API emits them rather than being held here.
async fn proxy(
    State(state): State<AppState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    req: Request,
) -> Response {
    let (parts, body) = req.into_parts();

    // Preserve the full path and query verbatim (tokens ride in the SSE stream's query string).
    let path_and_query = parts
        .uri
        .path_and_query()
        .map_or_else(|| parts.uri.path(), |pq| pq.as_str());
    let url = format!("{}{path_and_query}", state.upstream);

    let Ok(body_bytes) = axum::body::to_bytes(body, state.max_body_bytes).await else {
        return (StatusCode::PAYLOAD_TOO_LARGE, "request body too large").into_response();
    };

    let mut headers = parts.headers.clone();
    strip_hop_by_hop(&mut headers);
    // reqwest sets Host and Content-Length from the target URL and body respectively.
    headers.remove(header::HOST);
    headers.remove(header::CONTENT_LENGTH);
    set_forwarded_headers(&mut headers, &peer.ip().to_string());

    let upstream = state
        .client
        .request(parts.method, url.as_str())
        .headers(headers)
        .body(body_bytes)
        .send()
        .await;

    let upstream = match upstream {
        Ok(response) => response,
        Err(error) => {
            tracing::warn!(%error, %url, "upstream request failed");
            return (StatusCode::BAD_GATEWAY, "upstream request failed").into_response();
        }
    };

    let status = upstream.status();
    let mut response_headers = upstream.headers().clone();
    strip_hop_by_hop(&mut response_headers);
    // The body is re-framed by this server's HTTP layer; a stale length/encoding would lie.
    response_headers.remove(header::TRANSFER_ENCODING);

    let mut response = Response::new(Body::from_stream(upstream.bytes_stream()));
    *response.status_mut() = status;
    *response.headers_mut() = response_headers;
    response
}

/// Hop-by-hop headers are meaningful only on a single connection and must not be forwarded
/// across a proxy (RFC 9110 §7.6.1). Stripped from both the request and the response.
fn strip_hop_by_hop(headers: &mut HeaderMap) {
    const HOP_BY_HOP: [HeaderName; 7] = [
        header::CONNECTION,
        HeaderName::from_static("keep-alive"),
        header::PROXY_AUTHENTICATE,
        header::PROXY_AUTHORIZATION,
        header::TE,
        header::TRAILER,
        header::UPGRADE,
    ];
    for name in HOP_BY_HOP {
        headers.remove(name);
    }
}

/// Attach the forwarding headers the API relies on to identify the real client.
///
/// `X-Forwarded-For` is *appended* to any inbound value (mirroring nginx's
/// `$proxy_add_x_forwarded_for`), so the peer this proxy actually accepted the connection from
/// is the trustworthy right-most entry.
fn set_forwarded_headers(headers: &mut HeaderMap, client_ip: &str) {
    const X_FORWARDED_FOR: HeaderName = HeaderName::from_static("x-forwarded-for");
    const X_REAL_IP: HeaderName = HeaderName::from_static("x-real-ip");
    const X_FORWARDED_PROTO: HeaderName = HeaderName::from_static("x-forwarded-proto");

    let forwarded_for = match headers.get(&X_FORWARDED_FOR).and_then(|v| v.to_str().ok()) {
        Some(existing) if !existing.is_empty() => format!("{existing}, {client_ip}"),
        _ => client_ip.to_owned(),
    };
    if let Ok(value) = HeaderValue::from_str(&forwarded_for) {
        headers.insert(X_FORWARDED_FOR, value);
    }
    if let Ok(value) = HeaderValue::from_str(client_ip) {
        headers.insert(X_REAL_IP, value);
    }
    headers.insert(X_FORWARDED_PROTO, HeaderValue::from_static("http"));
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU32, Ordering};

    use axum::extract::Path;
    use tokio::net::TcpListener;

    /// A stub `api` upstream: `/v1/echo` reflects the forwarded client IP (both in the body and
    /// a response header) so a test can assert the proxy set it; `/v1/status/{code}` returns an
    /// arbitrary status so pass-through of non-200s can be checked.
    async fn spawn_stub_upstream() -> SocketAddr {
        async fn echo(headers: HeaderMap) -> Response {
            let xff = headers
                .get("x-forwarded-for")
                .and_then(|v| v.to_str().ok())
                .unwrap_or("")
                .to_owned();
            ([("x-echo-xff", xff.clone())], format!("echo:{xff}")).into_response()
        }
        async fn status(Path(code): Path<u16>) -> Response {
            StatusCode::from_u16(code).unwrap().into_response()
        }

        let app = Router::new()
            .route("/v1/echo", get(echo))
            .route("/v1/status/{code}", get(status));
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        addr
    }

    /// Write a minimal SPA bundle (shell + one hashed asset) to a unique temp directory.
    fn write_bundle() -> PathBuf {
        static COUNTER: AtomicU32 = AtomicU32::new(0);
        let unique = format!(
            "{}-{}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::Relaxed)
        );
        let dir = std::env::temp_dir().join(format!("tankovault-frontend-test-{unique}"));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("index.html"), "INDEX").unwrap();
        std::fs::write(dir.join("app.js"), "APPJS").unwrap();
        dir
    }

    /// Stand up the real frontend router against a stub upstream on an ephemeral port.
    async fn spawn_frontend(static_dir: &str, upstream: SocketAddr) -> SocketAddr {
        let state = AppState {
            client: reqwest::Client::builder()
                .gzip(false)
                .brotli(false)
                .build()
                .unwrap(),
            upstream: format!("http://{upstream}"),
            max_body_bytes: 1024 * 1024,
        };
        let app = build_router(static_dir, state);
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(
                listener,
                app.into_make_service_with_connect_info::<SocketAddr>(),
            )
            .await
            .unwrap();
        });
        addr
    }

    #[tokio::test]
    async fn healthz_returns_ok() {
        let upstream = spawn_stub_upstream().await;
        let dir = write_bundle();
        let front = spawn_frontend(dir.to_str().unwrap(), upstream).await;

        let response = reqwest::get(format!("http://{front}/healthz"))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(response.text().await.unwrap(), "ok\n");
    }

    #[tokio::test]
    async fn static_assets_are_served_with_hardening_headers() {
        let upstream = spawn_stub_upstream().await;
        let dir = write_bundle();
        let front = spawn_frontend(dir.to_str().unwrap(), upstream).await;

        let response = reqwest::get(format!("http://{front}/app.js"))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response.headers().get("x-content-type-options").unwrap(),
            "nosniff"
        );
        assert_eq!(response.headers().get("x-frame-options").unwrap(), "DENY");
        assert_eq!(response.text().await.unwrap(), "APPJS");
    }

    #[tokio::test]
    async fn unknown_paths_fall_back_to_the_app_shell() {
        let upstream = spawn_stub_upstream().await;
        let dir = write_bundle();
        let front = spawn_frontend(dir.to_str().unwrap(), upstream).await;

        // A client-side route with no matching file must resolve to index.html, not 404.
        let response = reqwest::get(format!("http://{front}/series/abc/deep"))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(response.text().await.unwrap(), "INDEX");
    }

    #[tokio::test]
    async fn v1_requests_are_proxied_with_the_client_ip_forwarded() {
        let upstream = spawn_stub_upstream().await;
        let dir = write_bundle();
        let front = spawn_frontend(dir.to_str().unwrap(), upstream).await;

        let response = reqwest::get(format!("http://{front}/v1/echo"))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        // The proxy must have injected the loopback peer as X-Forwarded-For for the API.
        let echoed = response
            .headers()
            .get("x-echo-xff")
            .unwrap()
            .to_str()
            .unwrap()
            .to_owned();
        assert!(echoed.contains("127.0.0.1"), "XFF not forwarded: {echoed}");
        assert_eq!(response.text().await.unwrap(), format!("echo:{echoed}"));
    }

    #[tokio::test]
    async fn proxy_passes_upstream_status_through() {
        let upstream = spawn_stub_upstream().await;
        let dir = write_bundle();
        let front = spawn_frontend(dir.to_str().unwrap(), upstream).await;

        let response = reqwest::get(format!("http://{front}/v1/status/404"))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }

    #[test]
    fn forwarded_for_appends_to_an_existing_chain() {
        let mut headers = HeaderMap::new();
        headers.insert(
            HeaderName::from_static("x-forwarded-for"),
            HeaderValue::from_static("203.0.113.7"),
        );
        set_forwarded_headers(&mut headers, "198.51.100.2");
        assert_eq!(
            headers.get("x-forwarded-for").unwrap(),
            "203.0.113.7, 198.51.100.2"
        );
        assert_eq!(headers.get("x-real-ip").unwrap(), "198.51.100.2");
    }

    #[test]
    fn hop_by_hop_headers_are_stripped() {
        let mut headers = HeaderMap::new();
        headers.insert(header::CONNECTION, HeaderValue::from_static("keep-alive"));
        headers.insert(header::UPGRADE, HeaderValue::from_static("websocket"));
        headers.insert(
            header::CONTENT_TYPE,
            HeaderValue::from_static("application/json"),
        );
        strip_hop_by_hop(&mut headers);
        assert!(headers.get(header::CONNECTION).is_none());
        assert!(headers.get(header::UPGRADE).is_none());
        // A non-hop-by-hop header is untouched.
        assert!(headers.get(header::CONTENT_TYPE).is_some());
    }
}
