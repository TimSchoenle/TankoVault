//! # frontend service
//!
//! Serves the compiled Dioxus WASM single-page app and reverse-proxies `/v1/*` (REST + SSE)
//! to the `api` service from a single origin, replacing the previous nginx image. Like every
//! other backend binary it is a fully static musl build shipped on a bare `scratch` image
//! (see `deploy/docker/Dockerfile.frontend`).
//!
//! ## Why one origin
//!
//! The WASM client issues same-origin `/v1/...` requests (`web/frontend/src/api/mod.rs`) and opens
//! the live-notification stream (`/v1/me/stream`) via the browser `EventSource` API. One origin is
//! also what makes the refresh cookie's `__Host-` prefix workable: the prefix requires `Path=/`,
//! and everything that path now reaches is served from here (see `auth::session::refresh_cookie`
//! in `services/api` for the review). Serving
//! the SPA and proxying `/v1/*` from the same origin is what makes those calls resolve without
//! a cross-origin hop — no CORS — and the proxy streams responses unbuffered so Server-Sent
//! Events flush to the browser the instant the API emits them.
//!
//! ## Feature parity with the retired nginx config
//!
//! - `/v1/*` — streaming reverse proxy to the API, forwarding `X-Forwarded-For` / `X-Real-IP`
//!   / `X-Forwarded-Proto` so the API's rate limiter and audit trail see the real client, with
//!   no request timeout on the proxied leg so long-lived SSE streams stay open.
//! - everything else — the static bundle with SPA fallback to `index.html`, carrying the
//!   baseline hardening headers (`X-Content-Type-Options`, `Referrer-Policy`, `X-Frame-Options`)
//!   on the app shell only, so proxied API responses keep their own headers.
//!
//! ## Why this service uses the shared runtime
//!
//! It did not, and that was a defect: it served `/healthz` instead of the `/health` + `/ready`
//! contract every other service exposes, exported no metrics, and mounted a bare `TraceLayer`
//! instead of [`HttpStack`] — so the one tier that *originates* every request emitted no
//! `x-request-id`, and a request could not be correlated across the frontend → api hop that
//! the rest of the stack is built to trace. It now uses [`HttpStack`], [`ops_router`] and the
//! isolated metrics listener like everything else, and its readiness probe reports the `api`
//! upstream, which is the only dependency it has.
//!
//! Two shared-stack concerns are deliberately *not* adopted here; see [`stack_security`] and
//! the rate-limiting note in [`main`].

use std::net::SocketAddr;
use std::time::Duration;

use axum::Router;
use axum::body::Body;
use axum::extract::{ConnectInfo, Request, State};
use axum::http::{HeaderMap, HeaderName, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{any, get};
use serde::Deserialize;
use tankovault_config::{MetricsConfig, SecurityConfig, TelemetryConfig};
use tankovault_service::{Health, HttpStack, MetricsRegistry};
use tower::ServiceBuilder;
use tower_http::services::{ServeDir, ServeFile};
use tower_http::set_header::SetResponseHeaderLayer;

#[derive(Debug, Deserialize)]
struct Config {
    #[serde(default = "default_bind")]
    bind_addr: String,
    telemetry: TelemetryConfig,
    #[serde(default)]
    frontend: FrontendConfig,
    /// Prometheus metrics, with the same `TANKOVAULT_METRICS__*` surface as every other
    /// service — including the isolated scrape port (`0.0.0.0:9090` by default), so the
    /// scrape never shares the public listener the browser talks to.
    #[serde(default)]
    metrics: MetricsConfig,
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
    /// Largest request body accepted on this hop.
    ///
    /// Enforced twice, deliberately: the shared stack's `DefaultBodyLimit` rejects it before
    /// a byte is buffered (see [`stack_security`]), and the proxy handler passes the same
    /// number to `to_bytes` so the buffering guard cannot drift from the advertised cap.
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
    // Before config, telemetry or anything else: this process may have been invoked by
    // Docker's HEALTHCHECK rather than as the server. `scratch` images have no shell and no
    // wget, so the binary probing itself is the only probe available. See
    // `tankovault_service::healthcheck`.
    if tankovault_service::healthcheck::requested() {
        let cfg: Config = tankovault_config::load()?;
        tankovault_service::run_healthcheck_and_exit(&cfg.bind_addr);
    }

    let cfg: Config = tankovault_config::load()?;
    tankovault_service::init_tracing(&cfg.telemetry)?;
    let metrics = MetricsRegistry::install(&cfg.metrics)?;
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

    // The scrape lives on its own listener (`TANKOVAULT_METRICS__LISTEN`), so the public
    // port the browser reaches never serves it.
    tankovault_service::spawn_metrics_server(metrics.clone(), shutdown.clone());

    // No rate limiter is mounted, deliberately. One page load fetches the shell plus every
    // hashed asset, so any bucket tight enough to matter would throttle a legitimate cold
    // load; the API behind this proxy applies the limits that actually protect state, and
    // it sees the real client because this hop appends `X-Forwarded-For`.
    let health = upstream_health(&state);
    let app = build_app(
        &cfg.frontend.static_dir,
        state,
        &stack_security(&cfg.frontend),
        &metrics,
        health,
    );
    tankovault_service::serve(&cfg.bind_addr, app, shutdown).await?;
    Ok(())
}

/// The shared [`HttpStack`]'s hardening config, **derived rather than operator-supplied**.
///
/// `TANKOVAULT_SECURITY__*` is deliberately not read here, because on this tier two of those
/// knobs cannot be honoured and a third would duplicate one that already exists:
///
/// - `security_headers` **must stay off**. The shared middleware sends the API's header set,
///   whose `Content-Security-Policy: default-src 'none'` is right for a JSON API and fatal for
///   an HTML document — it blocks the WASM bundle, so the app does not boot at all. This tier
///   sends its own policy ([`CSP`]) on the app shell instead, and leaves proxied `/v1/*`
///   responses carrying the API's.
/// - `cors` is meaningless here: the SPA and the API it calls share an origin *because* of
///   this proxy, so there is no cross-origin hop to allow or refuse.
/// - `max_body_bytes` is mapped from `TANKOVAULT_FRONTEND__MAX_BODY_BYTES` so the layer's cap
///   and the proxy's own buffering guard cannot drift apart.
///
/// Exposing them as configuration anyway would only offer settings that silently do nothing,
/// which is the failure mode `TANKOVAULT_TELEMETRY__OTLP_ENDPOINT` was removed for.
fn stack_security(frontend: &FrontendConfig) -> SecurityConfig {
    SecurityConfig {
        security_headers: false,
        max_body_bytes: frontend.max_body_bytes,
        ..SecurityConfig::default()
    }
}

/// Readiness for this tier: is the `api` upstream reachable?
///
/// It is the only dependency the frontend has, and previously nothing checked it — so a
/// frontend whose upstream was gone still reported itself healthy and kept serving an app
/// that could not load a single page of data. `/health` (liveness) stays independent of it,
/// because restarting this process cannot fix an unreachable API.
fn upstream_health(state: &AppState) -> Health {
    let client = state.client.clone();
    let url = format!("{}/health", state.upstream);
    Health::builder()
        .check_fn("api", move || {
            let client = client.clone();
            let url = url.clone();
            async move {
                match client.get(&url).send().await {
                    Ok(response) if response.status().is_success() => Ok(()),
                    Ok(response) => Err(format!("upstream answered {}", response.status())),
                    Err(error) => Err(error.to_string()),
                }
            }
        })
        .build()
}

/// Assemble the served application: the app router inside the shared middleware stack, plus
/// the ops probes merged *outside* it — so a body cap or a timeout can never make a healthy
/// replica look unhealthy to its orchestrator.
fn build_app(
    static_dir: &str,
    state: AppState,
    security: &SecurityConfig,
    metrics: &MetricsRegistry,
    health: Health,
) -> Router {
    HttpStack::new(security, metrics.clone())
        .apply(build_router(static_dir, state))
        .merge(tankovault_service::ops_router(health, metrics.clone()))
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
        .service(bundle);
    // Compression is *not* layered here any more: `HttpStack` applies one `CompressionLayer`
    // over the whole app, which covers the WASM bundle (1-3 MB, the largest single cost of a
    // cold load) and the proxied JSON alike. Keeping a second one on this branch would only
    // add a no-op wrapper — tower-http skips a response that already carries
    // `Content-Encoding`, and skips `text/event-stream` outright, which is what keeps the SSE
    // relay flushing frame by frame.

    Router::new()
        .route("/healthz", get(healthz))
        .route("/v1/{*rest}", any(proxy))
        .fallback_service(static_service)
        .with_state(state)
}

/// Legacy liveness alias.
///
/// The real probe is `GET /health` from [`tankovault_service::ops_router`], matching every
/// other service. This path is kept only because the retired nginx image published it and an
/// external monitor may still be pointed at it; it is not referenced by the compose stack or
/// the container healthcheck, and can be removed once nothing calls it.
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

    // Preserve the full path and query verbatim: the SSE stream's credential rides in its query
    // string, because `EventSource` cannot set a header. It is a single-use, 30-second ticket
    // rather than an access token (SEC-8) precisely because this hop — and every reverse proxy in
    // front of it — records the URI it forwards.
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
    /// arbitrary status so pass-through of non-200s can be checked; `/health` is what the
    /// frontend's own readiness probe calls.
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
            .route("/v1/status/{code}", get(status))
            .route("/health", get(async || "ok"));
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

    /// Stand up the real frontend application — the shared stack included — against a stub
    /// upstream on an ephemeral port.
    ///
    /// Deliberately goes through [`build_app`] rather than [`build_router`]: the middleware
    /// the audit found missing (request id, ops probes) lives in the stack, so a test that
    /// only assembled the inner router would keep passing if it were removed again.
    async fn spawn_frontend(static_dir: &str, upstream: SocketAddr) -> SocketAddr {
        let frontend = FrontendConfig {
            max_body_bytes: 1024 * 1024,
            ..FrontendConfig::default()
        };
        let state = AppState {
            client: reqwest::Client::builder()
                .gzip(false)
                .brotli(false)
                .build()
                .unwrap(),
            upstream: format!("http://{upstream}"),
            max_body_bytes: frontend.max_body_bytes,
        };
        let health = upstream_health(&state);
        let app = build_app(
            static_dir,
            state,
            &stack_security(&frontend),
            // No recorder: installing the process-wide Prometheus recorder twice fails, and
            // these tests share a process.
            &MetricsRegistry::disabled(),
            health,
        );
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

    /// The contract every other service exposes. This tier answered `/healthz` only, so any
    /// orchestrator config templated on `/health` failed against it alone.
    #[tokio::test]
    async fn health_and_ready_match_the_shared_contract() {
        let upstream = spawn_stub_upstream().await;
        let dir = write_bundle();
        let front = spawn_frontend(dir.to_str().unwrap(), upstream).await;

        let health = reqwest::get(format!("http://{front}/health"))
            .await
            .unwrap();
        assert_eq!(health.status(), StatusCode::OK);

        // Readiness reports the api upstream, which the stub is serving.
        let ready = reqwest::get(format!("http://{front}/ready")).await.unwrap();
        assert_eq!(ready.status(), StatusCode::OK);
        assert!(ready.text().await.unwrap().contains("\"api\""));
    }

    /// With the upstream gone, readiness must fail while liveness must not — restarting this
    /// process cannot bring the API back, and a restart loop would only deepen the outage.
    #[tokio::test]
    async fn readiness_fails_when_the_api_upstream_is_unreachable() {
        // Bind then drop, so the port is known to have been free and is now unbound.
        let dead = {
            let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
            listener.local_addr().unwrap()
        };
        let dir = write_bundle();
        let front = spawn_frontend(dir.to_str().unwrap(), dead).await;

        let ready = reqwest::get(format!("http://{front}/ready")).await.unwrap();
        assert_eq!(ready.status(), StatusCode::SERVICE_UNAVAILABLE);

        let health = reqwest::get(format!("http://{front}/health"))
            .await
            .unwrap();
        assert_eq!(health.status(), StatusCode::OK);
    }

    /// This tier originates every correlation chain. With a bare `TraceLayer` it minted no
    /// request id at all, so a frontend → api hop could not be correlated in the logs.
    #[tokio::test]
    async fn responses_carry_a_request_id() {
        let upstream = spawn_stub_upstream().await;
        let dir = write_bundle();
        let front = spawn_frontend(dir.to_str().unwrap(), upstream).await;

        for path in ["/app.js", "/v1/echo"] {
            let response = reqwest::get(format!("http://{front}{path}")).await.unwrap();
            assert!(
                response.headers().contains_key("x-request-id"),
                "{path} carried no x-request-id"
            );
        }
    }

    /// The shared stack's security-header middleware sends the API's
    /// `Content-Security-Policy: default-src 'none'`, which blocks the WASM bundle and stops
    /// the SPA booting. [`stack_security`] turns it off so this tier's own policy survives.
    #[tokio::test]
    async fn the_spa_keeps_its_own_content_security_policy() {
        let upstream = spawn_stub_upstream().await;
        let dir = write_bundle();
        let front = spawn_frontend(dir.to_str().unwrap(), upstream).await;

        let response = reqwest::get(format!("http://{front}/")).await.unwrap();
        let csp = response
            .headers()
            .get("content-security-policy")
            .and_then(|v| v.to_str().ok())
            .unwrap_or_default()
            .to_owned();
        assert!(
            csp.contains("wasm-unsafe-eval"),
            "the API's CSP clobbered the SPA's: {csp}"
        );
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
