//! Frontend service: serves the compiled Dioxus WASM SPA and reverse-proxies `/v1/*` (REST +
//! SSE) to the `api` service from one origin, via the shared [`HttpStack`] runtime.
//!
//! Two shared-stack concerns are deliberately *not* adopted here — see [`stack_security`] and
//! the rate-limiting note in [`main`].

use std::net::SocketAddr;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use axum::Router;
use axum::body::Body;
use axum::extract::{ConnectInfo, Request, State};
use axum::http::{HeaderMap, HeaderName, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{any, get};
use serde::Deserialize;
use tankovault_config::{MetricsConfig, SecurityConfig, TelemetryConfig};
use tankovault_service::{CancellationToken, Health, HttpStack, MetricsRegistry};
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
    /// Prometheus metrics, with the same `TANKOVAULT_METRICS__*` surface as every service —
    /// including the isolated scrape port, so the scrape never shares the public listener.
    #[serde(default)]
    metrics: MetricsConfig,
}

/// Non-privileged: the `scratch` image runs as a numeric nonroot user, which can't bind
/// reserved port 80; the compose stack maps host `3000` here instead.
fn default_bind() -> String {
    "0.0.0.0:3000".to_owned()
}

#[derive(Debug, Clone, Deserialize)]
struct FrontendConfig {
    /// Directory the built SPA bundle is served from. Baked into the image at `/srv/www`.
    #[serde(default = "FrontendConfig::default_static_dir")]
    static_dir: String,
    /// The generated third-party licence notices, served at `/third-party-notices`.
    ///
    /// Points at the image's own copy (`/THIRD-PARTY-NOTICES`, beside `/LICENSE`) rather than a
    /// second one inside the bundle: it is 300-odd KB, and two copies is the arrangement where
    /// one of them goes stale. Configurable because the path only exists inside the image — a
    /// developer running `dx serve` and the tests point it at their own checkout.
    #[serde(default = "FrontendConfig::default_notices_path")]
    notices_path: String,
    /// Base origin the `/v1/*` proxy targets, e.g. `http://api:8080`. No trailing slash.
    #[serde(default = "FrontendConfig::default_api_upstream")]
    api_upstream: String,
    /// Largest request body accepted on this hop.
    ///
    /// Enforced twice: the shared stack's `DefaultBodyLimit` rejects it before buffering (see
    /// [`stack_security`]), and the proxy handler passes the same number to `to_bytes` so the
    /// two cannot drift.
    #[serde(default = "FrontendConfig::default_max_body_bytes")]
    max_body_bytes: usize,
    /// Connection-establishment timeout for the upstream — not a whole-request timeout, since
    /// an SSE stream stays open indefinitely.
    #[serde(default = "FrontendConfig::default_connect_timeout_secs")]
    connect_timeout_secs: u64,
}

impl FrontendConfig {
    fn default_static_dir() -> String {
        "/srv/www".to_owned()
    }
    fn default_notices_path() -> String {
        "/THIRD-PARTY-NOTICES".to_owned()
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
            notices_path: Self::default_notices_path(),
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
    // Before anything else: this may be Docker's HEALTHCHECK invoking the binary rather than
    // the server. `scratch` has no shell/wget, so the binary probing itself is the only probe.
    if tankovault_service::healthcheck::requested() {
        let cfg: Config = tankovault_config::load()?;
        tankovault_service::run_healthcheck_and_exit(&cfg.bind_addr);
    }

    let boot = tankovault_config::load_watched::<Config>()?;
    // Both are process-global and installed once, which is why `telemetry.*` and `metrics.*`
    // are the two blocks a configuration reload cannot apply.
    tankovault_service::init_tracing(&boot.value.telemetry)?;
    let metrics =
        MetricsRegistry::install(&boot.value.metrics, &boot.value.telemetry.service_name)?;
    let shutdown = tankovault_service::install_shutdown();
    // The scrape lives on its own listener (`TANKOVAULT_METRICS__LISTEN`), so the public
    // port the browser reaches never serves it. Outside the reloadable runtime so a reload
    // does not rebind it.
    tankovault_service::spawn_metrics_server(metrics.clone(), shutdown.clone());

    tankovault_service::run_reloading(boot, &shutdown, |cfg, generation| {
        serve_once(cfg, metrics.clone(), generation)
    })
    .await
}

/// Build and run everything a configuration change rebuilds: the upstream client, the proxy
/// state, the router and the listener.
///
/// Returns when `shutdown` is cancelled — by the OS signal, or by the supervisor because the
/// configuration changed and this runtime is being replaced.
async fn serve_once(
    cfg: Arc<Config>,
    metrics: MetricsRegistry,
    shutdown: CancellationToken,
) -> anyhow::Result<()> {
    // Decompression off so the response body forwards byte-for-byte alongside its
    // `Content-Encoding` header, rather than being silently decoded and shipped mismatched.
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

    // No rate limiter here: one page load fetches the shell plus every hashed asset, so any
    // bucket tight enough to matter throttles a legit cold load. The API applies the limits
    // that protect state, and sees the real client via the X-Forwarded-For this hop appends.
    let health = upstream_health(&state);
    let app = build_app(
        &cfg.frontend.static_dir,
        &cfg.frontend.notices_path,
        state,
        &stack_security(&cfg.frontend),
        &metrics,
        health,
    );
    tankovault_service::serve(&cfg.bind_addr, app, shutdown).await?;
    Ok(())
}

/// The shared [`HttpStack`]'s hardening config, derived rather than operator-supplied.
///
/// `security_headers` must stay off: the shared middleware's `default-src 'none'` CSP is right
/// for a JSON API but blocks the WASM bundle from booting; this tier sends its own policy
/// ([`content_security_policy`]) on the app shell instead. `cors` is meaningless since the SPA
/// and API share an origin through this proxy. `max_body_bytes` is mapped from
/// `TANKOVAULT_FRONTEND__MAX_BODY_BYTES` so this cap and the proxy's buffering guard can't drift.
fn stack_security(frontend: &FrontendConfig) -> SecurityConfig {
    SecurityConfig {
        security_headers: false,
        max_body_bytes: frontend.max_body_bytes,
        ..SecurityConfig::default()
    }
}

/// Readiness for this tier: is the `api` upstream reachable?
///
/// Previously nothing checked it, so a frontend whose upstream was gone still reported
/// healthy. `/health` (liveness) stays independent, since restarting this process can't fix
/// an unreachable API.
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
    notices_path: &str,
    state: AppState,
    security: &SecurityConfig,
    metrics: &MetricsRegistry,
    health: Health,
) -> Router {
    HttpStack::new(security, metrics.clone())
        .apply(build_router(static_dir, notices_path, state))
        .merge(tankovault_service::ops_router(health, metrics.clone()))
}

/// Content-Security-Policy for the SPA shell.
///
/// The access token lives only in memory; this CSP is the ceiling on where an injected script
/// could send it. No `'unsafe-eval'`: `'wasm-unsafe-eval'` covers WebAssembly instantiation, and
/// nothing here calls Dioxus's `document::eval` (banned — its web impl is `new Function(…)`, see
/// `web/frontend/src/browser.rs`). The `'sha256-…'` entries admit the shell's inline boot
/// scripts, hashed from the served shell at startup by [`inline_script_hashes`]; if that hash
/// ever stops matching what's served, the browser silently refuses the inline scripts with no
/// error surfaced.
///
/// `connect-src 'self'` is the exfiltration ceiling (widen only for a split-origin deployment);
/// `img-src` allows any `https:`/`data:` host for provider-sourced cover art.
fn content_security_policy(static_dir: &str) -> String {
    let shell = Path::new(static_dir).join("index.html");
    let hashes = match std::fs::read_to_string(&shell) {
        Ok(html) => inline_script_hashes(&html),
        Err(error) => {
            // Not fatal: only the shell's inline scripts are refused (theme/search shortcut
            // lost); `/v1/*`, ops probes and every hashed asset still work.
            tracing::warn!(
                shell = %shell.display(),
                %error,
                "could not read the app shell; its inline scripts will be blocked by the CSP"
            );
            Vec::new()
        }
    };
    let mut scripts = String::new();
    for hash in &hashes {
        scripts.push_str(" '");
        scripts.push_str(hash);
        scripts.push('\'');
    }
    format!(
        "default-src 'self'; \
         script-src 'self' 'wasm-unsafe-eval'{scripts}; \
         style-src 'self' 'unsafe-inline'; \
         connect-src 'self'; \
         img-src 'self' https: data:; \
         font-src 'self' data:; \
         object-src 'none'; \
         base-uri 'none'; \
         form-action 'self'; \
         frame-ancestors 'none'"
    )
}

/// The `sha256-…` source expressions for every inline `<script>` in `html`.
///
/// Hashed from the served shell at startup rather than baked in as a constant, so the hash
/// can't drift from what's actually served — see [`normalize_newlines`] for why line endings
/// don't affect the result.
///
/// A deliberately partial scan, not a full HTML parse: this reads one file generated by `dx`
/// from a shell in this repo. Elements carrying `src` are skipped — covered by `'self'`.
fn inline_script_hashes(html: &str) -> Vec<String> {
    const OPEN: &str = "<script";

    let mut hashes = Vec::new();
    let mut rest = html;
    while let Some(offset) = rest.find(OPEN) {
        let after_name = &rest[offset + OPEN.len()..];
        // `<scriptfoo>` is not a script element; only a name/attribute boundary follows the tag.
        if after_name.starts_with(|c: char| c.is_alphanumeric() || c == '-') {
            rest = after_name;
            continue;
        }
        let (Some(open_end), Some(close)) = (after_name.find('>'), after_name.find("</script>"))
        else {
            break;
        };
        // Unterminated opening tag (`</script>` before its `>`); refusing to hash avoids a panic.
        if open_end >= close {
            break;
        }
        // A `>` inside an attribute value would mis-split here; not worth a parser for a shell
        // `dx` already validated.
        let (attributes, body) = (&after_name[..open_end], &after_name[open_end + 1..close]);
        if !has_src_attribute(attributes) {
            hashes.push(sha256_source(body));
        }
        rest = &after_name[close..];
    }
    hashes
}

fn has_src_attribute(attributes: &str) -> bool {
    const NAME: &str = "src";

    let mut searched = 0;
    while let Some(offset) = attributes[searched..].find(NAME) {
        let at = searched + offset;
        // Indexed into the whole span so the preceding character is checked correctly
        // (`srcsrc=` isn't `src`).
        let preceded_by_boundary = attributes[..at]
            .chars()
            .next_back()
            .is_none_or(char::is_whitespace);
        searched = at + NAME.len();
        if preceded_by_boundary && attributes[searched..].trim_start().starts_with('=') {
            return true;
        }
    }
    false
}

/// One CSP `sha256-…` source expression: the base64 SHA-256 of the script text, exactly as
/// the browser computes it over the element's contents (not the file's raw bytes — see
/// [`normalize_newlines`]).
fn sha256_source(script: &str) -> String {
    use base64::Engine as _;
    use sha2::Digest as _;

    let digest = sha2::Sha256::digest(normalize_newlines(script).as_bytes());
    format!(
        "sha256-{}",
        base64::engine::general_purpose::STANDARD.encode(digest)
    )
}

/// Apply the HTML parser's input-stream preprocessing: `\r\n` and a lone `\r` both become `\n`.
///
/// Load-bearing: a CSP hash covers the script element's *text content* as the parser produces
/// it, not the wire bytes ([WHATWG HTML §13.2.3.5]); hashing raw CRLF bytes computes a hash no
/// browser ever does, so a CRLF checkout silently has its inline scripts refused with a
/// correct-looking CSP header — the only symptom is in the browser console.
///
/// [WHATWG HTML §13.2.3.5]: https://html.spec.whatwg.org/multipage/parsing.html#preprocessing-the-input-stream
fn normalize_newlines(script: &str) -> std::borrow::Cow<'_, str> {
    if !script.contains('\r') {
        return std::borrow::Cow::Borrowed(script);
    }
    let mut out = String::with_capacity(script.len());
    let mut chars = script.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\r' {
            // CRLF collapses to one LF; a lone CR becomes one too.
            if chars.peek() == Some(&'\n') {
                chars.next();
            }
            out.push('\n');
        } else {
            out.push(c);
        }
    }
    std::borrow::Cow::Owned(out)
}

/// Where the SPA links its third-party licence notices, and the only URL they are published at.
///
/// `web/frontend/src/components/nav.rs` repeats this literal — it is a separate workspace, so
/// there is no compile-time relationship between the two — and `xtask repo-lint` is what holds
/// them equal.
const NOTICES_ROUTE: &str = "/third-party-notices";

/// Assemble the router: the health probe, the licence notices, the `/v1/*` proxy, and the
/// static bundle (with SPA fallback and hardening headers) catching everything else.
fn build_router(static_dir: &str, notices_path: &str, state: AppState) -> Router {
    // SPA fallback: any path with no matching file resolves to the app shell so client-side
    // routing (`/series/…`, `/account/…`) works on a hard refresh or a deep link.
    let index = format!("{}/index.html", static_dir.trim_end_matches('/'));
    let bundle = ServeDir::new(static_dir).fallback(ServeFile::new(index));

    // Built once from the shell on disk, not per response, since the hashes cover a file that
    // can't change without a redeploy. The fallback below is unreachable in practice (the
    // policy is assembled from ASCII only), but a hashless CSP still beats refusing to serve.
    let csp = HeaderValue::from_str(&content_security_policy(static_dir))
        .unwrap_or_else(|_| HeaderValue::from_static("default-src 'self'"));

    // Baseline hardening on the app shell only: `if_not_present` never clobbers a value a
    // served file already carries, and `/v1/*` responses keep the API's own headers untouched.
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
            csp,
        ))
        // The app shell must never be cached: it names the hashed bundle, so a stale copy
        // pins the client to a retired build. Hashed assets get their own immutable caching
        // via `ServeDir`'s ETag/Last-Modified handling.
        .layer(SetResponseHeaderLayer::if_not_present(
            header::CACHE_CONTROL,
            HeaderValue::from_static("no-cache"),
        ))
        .service(bundle);
    // Compression isn't layered here separately: `HttpStack`'s single `CompressionLayer`
    // already covers the WASM bundle and proxied JSON, and skips a response already carrying
    // `Content-Encoding` or `text/event-stream` — which is what keeps SSE flushing frame by
    // frame.

    // A route of its own rather than a file dropped into the bundle, for two reasons that both
    // end in a reader not seeing the notices: an extensionless file is served
    // `application/octet-stream`, which downloads rather than displays under `nosniff`; and any
    // path the bundle does not contain resolves to the app shell, so a typo'd or renamed file
    // would answer 200 with the SPA and nothing would look wrong.
    let notices = ServiceBuilder::new()
        .layer(SetResponseHeaderLayer::overriding(
            header::CONTENT_TYPE,
            HeaderValue::from_static("text/plain; charset=utf-8"),
        ))
        .layer(SetResponseHeaderLayer::if_not_present(
            header::X_CONTENT_TYPE_OPTIONS,
            HeaderValue::from_static("nosniff"),
        ))
        .service(ServeFile::new(notices_path));

    Router::new()
        .route("/healthz", get(healthz))
        .route_service(NOTICES_ROUTE, notices)
        .route("/v1/{*rest}", any(proxy))
        .fallback_service(static_service)
        .with_state(state)
}

/// Legacy liveness alias.
///
/// The real probe is `GET /health` from [`tankovault_service::ops_router`]. Kept only because
/// the retired nginx image published this path and an external monitor may still poll it.
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

    // Preserve the full path and query verbatim: the SSE stream's credential rides in the query
    // string (`EventSource` can't set a header) as a single-use, 30-second ticket rather than an
    // access token, since this hop and every proxy in front of it logs the URI it forwards.
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
/// `X-Forwarded-For` is appended to any inbound value (mirroring nginx's
/// `$proxy_add_x_forwarded_for`), so the right-most entry is the peer this proxy actually
/// accepted the connection from.
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

    /// A stub `api` upstream: `/v1/echo` reflects the forwarded client IP for assertions,
    /// `/v1/status/{code}` returns an arbitrary status, `/health` backs the readiness probe.
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

    /// Write a minimal SPA bundle (shell + one hashed asset + a stand-in notices file) to a
    /// unique temp directory.
    fn write_bundle() -> PathBuf {
        static COUNTER: AtomicU32 = AtomicU32::new(0);
        let unique = format!(
            "{}-{}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::Relaxed)
        );
        let dir = std::env::temp_dir().join(format!("tankovault-frontend-test-{unique}"));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("index.html"), SHELL).unwrap();
        std::fs::write(dir.join("app.js"), "APPJS").unwrap();
        // Beside the bundle here only because a test needs somewhere to put it; in the image it
        // sits at `/THIRD-PARTY-NOTICES`, outside the served directory.
        std::fs::write(dir.join(NOTICES_FILE), NOTICES).unwrap();
        dir
    }

    /// The stand-in notices document and the name [`write_bundle`] writes it under.
    const NOTICES: &str = "THIRD-PARTY NOTICES\nApache License 2.0\n";
    const NOTICES_FILE: &str = "THIRD-PARTY-NOTICES";

    /// The stand-in app shell: an inline boot script (which the CSP has to admit by hash) and
    /// an external one (which `'self'` already covers), matching the real shell's shape.
    const SHELL: &str = "<html><head><script>window.tv=1;</script>\
                         <script type=\"module\" src=\"/app.js\"></script></head>\
                         <body>INDEX</body></html>";

    /// Stand up the real frontend application — the shared stack included — against a stub
    /// upstream on an ephemeral port.
    ///
    /// Goes through [`build_app`], not [`build_router`]: the middleware previously found
    /// missing (request id, ops probes) lives in the stack, so testing only the inner router
    /// would miss a regression.
    async fn spawn_frontend(static_dir: &str, upstream: SocketAddr) -> SocketAddr {
        spawn_frontend_with_notices(
            static_dir,
            &format!("{static_dir}/{NOTICES_FILE}"),
            upstream,
        )
        .await
    }

    /// As [`spawn_frontend`], with the notices document somewhere of the caller's choosing —
    /// including nowhere.
    async fn spawn_frontend_with_notices(
        static_dir: &str,
        notices_path: &str,
        upstream: SocketAddr,
    ) -> SocketAddr {
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
            notices_path,
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

    /// The contract every other service exposes; this tier previously answered `/healthz` only.
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

    /// With the upstream gone, readiness must fail while liveness must not — a restart can't
    /// bring the API back.
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

    /// This tier originates every correlation chain: a bare `TraceLayer` minted no request id,
    /// so a frontend → api hop couldn't be correlated in logs.
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

    /// The shared stack's security middleware sends the API's `default-src 'none'` CSP, which
    /// blocks the WASM bundle; [`stack_security`] turns it off so this tier's own policy
    /// survives.
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
        // Nothing here needs eval(); admitting it would hand an injected script the one
        // primitive this policy denies.
        assert!(
            !csp.contains("'unsafe-eval'"),
            "the SPA's CSP re-enabled eval(): {csp}"
        );
    }

    /// The shell's inline scripts run before the WASM bundle (theme pre-paint, search
    /// shortcut); `script-src 'self'` doesn't cover them, so without their hashes the browser
    /// refuses both.
    #[tokio::test]
    async fn the_shells_inline_scripts_are_admitted_by_hash() {
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
            csp.contains(&format!("'{}'", sha256_source("window.tv=1;"))),
            "the shell's inline boot script is not admitted: {csp}"
        );
    }

    /// Pinned against a hash computed outside this codebase (`sha256sum | base64`):
    /// `sha256-bhHHL3z2…` is the value MDN documents for `alert(1)`, since a browser silently
    /// refusing a script it should run is otherwise invisible server-side.
    #[test]
    fn inline_scripts_hash_the_way_a_browser_does() {
        assert_eq!(
            sha256_source("alert(1)"),
            "sha256-bhHHL3z2vDgxUt0W3dWQOrprscmda2Y5pLsLg4GF+pI="
        );
        // Covers the element's text verbatim, indentation and newlines included.
        let html = "<head>\n    <script>\n      (function () {\n        var root = \
                    document.documentElement;\n      })();\n    </script>\n  </head>";
        assert_eq!(
            inline_script_hashes(html),
            vec!["sha256-TeLaQhPwM0rq27ouKvfSy/FNGtKQLRss1WIhFG20024=".to_owned()]
        );
    }

    /// A CRLF shell must produce the same hashes as an LF one, since the parser normalises
    /// newlines before tokenization and the browser always hashes the LF form. Hashing raw
    /// bytes used to ship a CSP that silently refused the shell's boot scripts on any Windows
    /// checkout.
    ///
    /// The expectation is the same literal as the LF case, deliberately: the point is that the
    /// two are indistinguishable once hashed.
    #[test]
    fn a_crlf_shell_hashes_the_same_as_an_lf_one() {
        let lf = "<head>\n    <script>\n      (function () {\n        var root = \
                  document.documentElement;\n      })();\n    </script>\n  </head>";
        let crlf = lf.replace('\n', "\r\n");

        assert_eq!(
            inline_script_hashes(&crlf),
            vec!["sha256-TeLaQhPwM0rq27ouKvfSy/FNGtKQLRss1WIhFG20024=".to_owned()]
        );
        assert_eq!(inline_script_hashes(&crlf), inline_script_hashes(lf));

        // A lone CR is normalised too, so a classic-Mac-ending file isn't a third hash.
        assert_eq!(sha256_source("a\rb"), sha256_source("a\nb"));
        assert_eq!(sha256_source("a\r\nb"), sha256_source("a\nb"));
    }

    #[test]
    fn a_script_with_a_src_is_left_to_self() {
        let hashes = inline_script_hashes(
            "<script src=\"/app.js\"></script>\
             <script type=\"module\" async src=\"/x.js\"></script>\
             <script>inline()</script>",
        );
        assert_eq!(hashes, vec![sha256_source("inline()")]);
    }

    /// A `src`-shaped attribute name is not a `src` attribute, and neither is `srcset`; both
    /// used to be enough to make a genuinely inline script go unhashed.
    #[test]
    fn only_a_whole_src_attribute_counts() {
        assert!(has_src_attribute(" src=\"/app.js\""));
        assert!(has_src_attribute(" type=\"module\" src = \"/app.js\""));
        assert!(!has_src_attribute(" data-src-hint=\"x\""));
        assert!(!has_src_attribute(" srcset=\"x\""));
        assert!(!has_src_attribute(" type=\"module\""));
        assert!(!has_src_attribute(" srcsrc=\"x\""));
    }

    /// An unreadable shell must not take the whole tier down — the API proxy and every hashed
    /// asset still work, only the inline scripts are refused.
    #[test]
    fn a_missing_shell_degrades_to_a_hashless_policy() {
        let csp = content_security_policy("./no-such-directory");
        assert!(
            csp.contains("script-src 'self' 'wasm-unsafe-eval';"),
            "{csp}"
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

    /// The obligation the whole notices artefact exists for: a reader whose browser downloads
    /// and runs the WASM bundle has received a binary distribution, and almost every licence in
    /// it requires its text to travel along. Serving them only inside the image would satisfy
    /// whoever pulls the image and nobody who actually runs the code.
    ///
    /// `text/plain` matters as much as the 200: the file is extensionless, so left to content
    /// sniffing it is `application/octet-stream`, which a browser downloads instead of showing.
    #[tokio::test]
    async fn the_licence_notices_are_served_to_the_reader_as_text() {
        let upstream = spawn_stub_upstream().await;
        let dir = write_bundle();
        let front = spawn_frontend(dir.to_str().unwrap(), upstream).await;

        let response = reqwest::get(format!("http://{front}{NOTICES_ROUTE}"))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response.headers().get("content-type").unwrap(),
            "text/plain; charset=utf-8"
        );
        assert_eq!(
            response.headers().get("x-content-type-options").unwrap(),
            "nosniff"
        );
        assert_eq!(response.text().await.unwrap(), NOTICES);
    }

    /// Pins why the notices are a route and not a file in the served bundle: every unmatched
    /// path resolves to the app shell, so a missing or renamed notices file would answer `200`
    /// with a page of HTML claiming to be the licence texts, and no gate would see it.
    #[tokio::test]
    async fn missing_notices_are_a_404_rather_than_the_app_shell() {
        let upstream = spawn_stub_upstream().await;
        let dir = write_bundle();
        let front =
            spawn_frontend_with_notices(dir.to_str().unwrap(), "./no-such-notices-file", upstream)
                .await;

        let response = reqwest::get(format!("http://{front}{NOTICES_ROUTE}"))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
        assert_ne!(response.text().await.unwrap(), SHELL);
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
        assert_eq!(response.text().await.unwrap(), SHELL);
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
