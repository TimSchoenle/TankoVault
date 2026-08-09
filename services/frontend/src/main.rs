//! Frontend service: serves the compiled Dioxus WASM SPA and reverse-proxies `/v1/*` (REST +
//! SSE) to the `api` service from one origin, via the shared [`HttpStack`] runtime.
//!
//! Two shared-stack concerns are deliberately *not* adopted here — see [`stack_security`] and
//! the rate-limiting note in [`main`].

use std::convert::Infallible;
use std::future::{Ready, ready};
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::task::{Context, Poll};
use std::time::Duration;

use axum::Router;
use axum::body::{Body, Bytes};
use axum::extract::{ConnectInfo, Request, State};
use axum::http::{HeaderMap, HeaderName, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{any, get};
use csp_shell::presets::cloudflare;
use csp_shell::{Csp, Policy};
use serde::Deserialize;
use tankovault_config::{MetricsConfig, SecurityConfig, TelemetryConfig};
use tankovault_service::{CancellationToken, Health, HttpStack, MetricsRegistry};
use tower::{Service, ServiceBuilder};
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
    /// Opt-ins for running behind Cloudflare. See [`CloudflareConfig`].
    #[serde(default)]
    cloudflare: CloudflareConfig,
}

/// What this tier's Content-Security-Policy concedes to Cloudflare, one flag per product.
///
/// Both default off, and both stay off for a deployment that is not behind Cloudflare: each one
/// admits something the policy otherwise refuses, and an unused concession is only a weakness.
#[derive(Debug, Clone, Default, Deserialize)]
struct CloudflareConfig {
    /// Send a freshly minted `'nonce-…'` in `script-src` on every response.
    ///
    /// Cloudflare's bot products (Bot Fight Mode, JavaScript Detections, the challenge platform)
    /// inject an inline `<script>` into the served HTML at the edge, after this service has
    /// hashed the shell — so `script-src` refuses it and the detection silently never runs.
    /// Cloudflare's documented answer is a nonce: it parses the `Content-Security-Policy`
    /// *response header* and copies the nonce onto what it injects. That is why nothing is
    /// stamped into the shell here — the header is the whole contract, and the shell's own
    /// inline scripts keep running by hash either way.
    ///
    /// The concession is real but narrow: an injected script that can already run could read
    /// this header back off a same-origin fetch and admit further inline script. It cannot
    /// forge one ahead of time (128 CSPRNG bits, minted per response), and it still cannot
    /// reach `'unsafe-eval'` or an off-origin host. Load-bearing for that: the shell is never
    /// served from a stored copy, which is why it is `Cache-Control: no-store` with no validator
    /// and not `no-cache` — see [`FixedResponse::shell`]. A stored shell would pin one nonce
    /// across every reader for the lifetime of the entry, which is `'unsafe-inline'` with extra
    /// steps.
    #[serde(default)]
    script_nonce: bool,
    /// Admit `https://challenges.cloudflare.com` in `script-src` and `frame-src`, for an
    /// embedded Turnstile widget.
    ///
    /// Only for a widget rendered *in* a page this service serves; a managed-challenge
    /// interstitial is a Cloudflare-served document carrying its own policy, and needs nothing
    /// here. The origin is Turnstile's only host and `'self'` can never cover it.
    #[serde(default)]
    turnstile: bool,
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
            cloudflare: CloudflareConfig::default(),
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
    let app = build_app(&cfg.frontend, state, &metrics, health);
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
    frontend: &FrontendConfig,
    state: AppState,
    metrics: &MetricsRegistry,
    health: Health,
) -> Router {
    HttpStack::new(&stack_security(frontend), metrics.clone())
        .apply(build_router(frontend, state))
        .merge(tankovault_service::ops_router(health, metrics.clone()))
}

/// Build the Content-Security-Policy for the SPA shell.
///
/// The access token lives only in memory; this CSP is the ceiling on where an injected script
/// could send it. [`Csp::spa_wasm`] is the policy this service's own hand-rolled one was
/// extracted into: no `'unsafe-eval'` — `'wasm-unsafe-eval'` covers WebAssembly instantiation and
/// nothing here calls Dioxus's `document::eval` (banned; its web impl is `new Function(…)`, see
/// `web/frontend/src/platform/`) — `connect-src 'self'` as the exfiltration ceiling, and any
/// `https:`/`data:` host in `img-src` for provider-sourced cover art.
///
/// The `'sha256-…'` entries admit the shell's inline boot scripts, scanned from the served shell
/// at startup rather than hardcoded: a constant drifts the moment the shell is edited, and the
/// browser then refuses the script with nothing surfaced server-side.
///
/// `connect-src` already covers Cloudflare's `/cdn-cgi/challenge-platform/` beacons and
/// `script-src 'self'` its scripts, since both are served from this origin — the flags in
/// [`CloudflareConfig`] exist for the two things that are *not* reachable from `'self'`.
fn content_security_policy(
    shell: Option<&str>,
    shell_path: &Path,
    config: &CloudflareConfig,
) -> Policy {
    let mut csp = Csp::spa_wasm();
    if let Some(source) = shell {
        let scan = csp_shell::scan_shell(source);
        // The scanner is deliberately not an HTML parser. Anything it accepted but could not
        // read exactly yields a hash no browser computes, so the limit is logged rather than
        // left as a blank page with nothing to attribute it to.
        for warning in &scan.warnings {
            tracing::warn!(
                shell = %shell_path.display(),
                %warning,
                "the app shell hit a documented scanner limit; its hashes may not match"
            );
        }
        csp = csp.with_scan(&scan);
    }
    if config.turnstile {
        csp = cloudflare::turnstile(csp);
    }
    if config.script_nonce {
        csp = cloudflare::script_nonce(csp);
    }
    csp.build()
}

/// Where the app shell lives inside the bundle.
fn shell_path(static_dir: &str) -> PathBuf {
    Path::new(static_dir).join("index.html")
}

/// Read the app shell once, at startup.
///
/// One read, not one per request, and that is the whole point: the bytes the browser receives and
/// the bytes [`content_security_policy`] hashed are then the same generation *by construction*.
/// A per-build hash list and a per-response nonce both depend on that, and nothing else enforces
/// it.
///
/// Fail open, which is this service's decision to make and not the library's: an unreadable shell
/// costs the SPA, while `/v1/*`, the ops probes and every hashed asset keep working.
fn read_shell(path: &Path) -> Option<String> {
    match std::fs::read_to_string(path) {
        Ok(source) => Some(source),
        Err(error) => {
            tracing::error!(
                shell = %path.display(),
                %error,
                "could not read the app shell; the SPA cannot be served"
            );
            None
        }
    }
}

/// Supplies the `Content-Security-Policy` value for each static response.
///
/// Two shapes because a deployment that is not behind Cloudflare has no nonce slot reserved, and
/// its header is then a constant: rendering it once at startup keeps the per-response cost at a
/// `HeaderValue` clone, and only the nonce case pays a render per response.
#[derive(Clone)]
enum CspHeader {
    Constant(HeaderValue),
    PerResponse(Arc<Policy>),
}

impl CspHeader {
    fn new(policy: Policy) -> Self {
        if policy.is_per_response() {
            Self::PerResponse(Arc::new(policy))
        } else {
            Self::Constant(header_value(&policy))
        }
    }
}

impl<T> tower_http::set_header::MakeHeaderValue<T> for CspHeader {
    fn make_header_value(&mut self, _response: &T) -> Option<HeaderValue> {
        Some(match self {
            Self::Constant(value) => value.clone(),
            Self::PerResponse(policy) => header_value(policy),
        })
    }
}

/// Render one response's policy.
fn header_value(policy: &Policy) -> HeaderValue {
    let csp_shell::Headers {
        content_security_policy,
        // The obligation this field carries — a per-response nonce served from cache is pinned
        // across every reader, which is `'unsafe-inline'` with extra steps — is discharged
        // unconditionally by the `Cache-Control: no-cache` layer in [`build_router`], which the
        // shell needs anyway because it names the hashed bundle.
        cache_control: _,
        // Cloudflare reads the nonce out of the response header, so nothing is stamped into the
        // shell and this service never needs the value itself.
        ..
    } = policy.headers();

    // `csp-policy` refuses any term that could carry a separator or a control character into the
    // rendered header, so this cannot fail; a restrictive fallback still beats refusing to serve.
    HeaderValue::from_str(&content_security_policy)
        .unwrap_or_else(|_| HeaderValue::from_static("default-src 'self'"))
}

/// The subdirectory `dx` writes every content-hashed file into.
///
/// Content-hashed means a changed file is a changed URL, which is what makes a one-year
/// `immutable` lifetime safe here and nowhere else in the bundle.
const HASHED_ASSETS: &str = "/assets";

/// A response held in memory, carrying its own `Cache-Control`.
///
/// The bundle's blanket `Cache-Control` layer is `if_not_present`, so stating the value here is
/// what keeps that default off these responses.
#[derive(Clone)]
struct FixedResponse {
    status: StatusCode,
    content_type: &'static str,
    cache_control: &'static str,
    body: Bytes,
}

impl FixedResponse {
    /// The app shell, from the bytes [`read_shell`] took at startup.
    ///
    /// **`no-store` rather than `no-cache`, and no `Last-Modified` or `ETag`.** All three are one
    /// requirement: this document must never be reconstructed from a stored copy. It carries a
    /// per-response CSP nonce and the inline-script hashes of exactly one build, so a body paired
    /// with any other response's header is a document that response's policy refuses.
    ///
    /// `no-cache` does not prevent that — it means *revalidate*, and a successful revalidation
    /// re-uses the stored body under the new response's headers. Served from disk it did exactly
    /// that: image timestamps are clamped to `SOURCE_DATE_EPOCH`, which this repository pins to a
    /// constant `0` on purpose (see `the_build_epoch_is_one_constant` in `xtask`), so every build
    /// answered `Last-Modified: Thu, 01 Jan 1970 00:00:00 GMT` and every conditional request was
    /// a `304` forever. Serving from memory with no validator is what makes the `304` impossible
    /// rather than merely unlikely.
    fn shell(body: Bytes) -> Self {
        Self {
            status: StatusCode::OK,
            content_type: "text/html; charset=utf-8",
            cache_control: "no-store",
            body,
        }
    }

    /// The shell could not be read at startup, so there is no SPA to serve.
    ///
    /// A `503` rather than the `404` a missing file used to produce: the bundle is broken, which
    /// is an operator's problem and not a wrong URL.
    fn unavailable() -> Self {
        Self {
            status: StatusCode::SERVICE_UNAVAILABLE,
            content_type: "text/plain; charset=utf-8",
            cache_control: "no-store",
            body: Bytes::from_static(b"app shell unavailable\n"),
        }
    }

    /// A bundle file that is not there.
    ///
    /// A `404`, never the shell. Answering a `<script type="module">` request with HTML fails in
    /// the browser on the MIME type instead, which reads as a bundler or CSP fault rather than
    /// the missing file it is — the same reasoning that makes the notices a route of their own.
    ///
    /// `no-store` because a rolling deploy legitimately serves this: a client that already has
    /// the new shell asks a replica that has not rolled yet for a file only the new image has.
    /// Caching that answer would outlive the rollout by a year.
    fn missing() -> Self {
        Self {
            status: StatusCode::NOT_FOUND,
            content_type: "text/plain; charset=utf-8",
            cache_control: "no-store",
            body: Bytes::from_static(b"not found\n"),
        }
    }
}

impl Service<Request> for FixedResponse {
    type Response = Response;
    type Error = Infallible;
    type Future = Ready<Result<Response, Infallible>>;

    fn poll_ready(&mut self, _cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Poll::Ready(Ok(()))
    }

    fn call(&mut self, _request: Request) -> Self::Future {
        ready(Ok((
            self.status,
            [
                (
                    header::CONTENT_TYPE,
                    HeaderValue::from_static(self.content_type),
                ),
                (
                    header::CACHE_CONTROL,
                    HeaderValue::from_static(self.cache_control),
                ),
            ],
            self.body.clone(),
        )
            .into_response()))
    }
}

/// Where the SPA links its third-party licence notices, and the only URL they are published at.
///
/// `web/frontend/src/components/nav.rs` repeats this literal — it is a separate workspace, so
/// there is no compile-time relationship between the two — and `xtask repo-lint` is what holds
/// them equal.
const NOTICES_ROUTE: &str = "/third-party-notices";

/// Assemble the router: the health probe, the licence notices, the `/v1/*` proxy, and the
/// static bundle (with SPA fallback and hardening headers) catching everything else.
fn build_router(frontend: &FrontendConfig, state: AppState) -> Router {
    let static_dir = frontend.static_dir.as_str();
    let shell_path = shell_path(static_dir);
    let source = read_shell(&shell_path);

    // Assembled once from the shell read above, since the hashes cover a file that can't change
    // without a redeploy; only the optional nonce is per response.
    let csp = CspHeader::new(content_security_policy(
        source.as_deref(),
        &shell_path,
        &frontend.cloudflare,
    ));

    // The same bytes the policy was derived from. Serving the file instead let a stored copy of
    // one build meet the header of another; see [`FixedResponse::shell`].
    let shell = source.map_or_else(FixedResponse::unavailable, |source| {
        FixedResponse::shell(Bytes::from(source.into_bytes()))
    });

    // `/assets/…` is content-hashed, so a changed file is a changed URL and a year is safe. A
    // miss is a `404` rather than the shell — see [`FixedResponse::missing`]. `if_not_present`
    // rather than `overriding`, so that `404` keeps its own `no-store`.
    let assets = ServiceBuilder::new()
        .layer(SetResponseHeaderLayer::if_not_present(
            header::CACHE_CONTROL,
            HeaderValue::from_static("public, max-age=31536000, immutable"),
        ))
        .service(
            ServeDir::new(Path::new(static_dir).join(HASHED_ASSETS.trim_start_matches('/')))
                .fallback(FixedResponse::missing()),
        );

    // `/` and `/index.html` are routed explicitly so neither ever reaches `ServeDir` and the file
    // on disk: the shell has exactly one representation, and it is the in-memory one. Everything
    // else is a real file if the bundle has one, and a client-side route (`/series/…`,
    // `/account/…`) otherwise, which is what makes a hard refresh or a deep link work.
    let bundle = Router::new()
        .route_service("/", shell.clone())
        .route_service("/index.html", shell.clone())
        .nest_service(HASHED_ASSETS, assets)
        .fallback_service(ServeDir::new(static_dir).fallback(shell));

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
        // The default for anything in the bundle that states nothing of its own: revalidate.
        // The two that do state their own are the ones that matter — the shell is `no-store`
        // ([`FixedResponse::shell`]) and `/assets/…` is immutable — and `if_not_present` is what
        // leaves both alone. This layer previously read `no-cache` and applied it to *everything*
        // under a comment claiming hashed assets were cached immutably; nothing set that, and
        // every asset paid a revalidation round-trip per load.
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
        .service(ServeFile::new(&frontend.notices_path));

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
        std::fs::create_dir_all(dir.join("assets")).unwrap();
        std::fs::write(dir.join("index.html"), SHELL).unwrap();
        std::fs::write(dir.join("app.js"), "APPJS").unwrap();
        std::fs::write(dir.join("assets").join(HASHED_ASSET), "HASHEDJS").unwrap();
        // Beside the bundle here only because a test needs somewhere to put it; in the image it
        // sits at `/THIRD-PARTY-NOTICES`, outside the served directory.
        std::fs::write(dir.join(NOTICES_FILE), NOTICES).unwrap();
        clamp_timestamps(&dir);
        dir
    }

    /// Date every file in the fixture `Thu, 01 Jan 1970`, as the image does.
    ///
    /// Not decoration. Image layer timestamps are clamped from `SOURCE_DATE_EPOCH`, which this
    /// repository pins to a constant `0`, so this is the shell's real mtime in production — and it
    /// is the precondition for the `304` that
    /// [`a_conditional_request_for_the_shell_is_never_answered_304`] exists to prevent. A fixture
    /// dated *now* cannot reproduce the bug: any conditional request against it is legitimately
    /// modified, and the test would pass against the code that shipped it.
    fn clamp_timestamps(dir: &std::path::Path) {
        for name in ["index.html", "app.js"] {
            std::fs::File::options()
                .write(true)
                .open(dir.join(name))
                .unwrap()
                .set_modified(std::time::SystemTime::UNIX_EPOCH)
                .unwrap();
        }
    }

    /// A stand-in for what `dx` writes into `assets/`: a filename carrying its own content hash.
    const HASHED_ASSET: &str = "app-dxh0123456789abcd.js";

    /// The stand-in notices document and the name [`write_bundle`] writes it under.
    const NOTICES: &str = "THIRD-PARTY NOTICES\nApache License 2.0\n";
    const NOTICES_FILE: &str = "THIRD-PARTY-NOTICES";

    /// The stand-in app shell: an inline boot script (which the CSP has to admit by hash) and
    /// an external one (which `'self'` already covers), matching the real shell's shape.
    const SHELL: &str = "<html><head><script>window.tv=1;</script>\
                         <script type=\"module\" src=\"/app.js\"></script></head>\
                         <body>INDEX</body></html>";

    /// The service configuration these tests run against: the written bundle, its notices file
    /// beside it, and a body cap small enough to be reached deliberately.
    fn test_config(static_dir: &str) -> FrontendConfig {
        FrontendConfig {
            static_dir: static_dir.to_owned(),
            notices_path: format!("{static_dir}/{NOTICES_FILE}"),
            max_body_bytes: 1024 * 1024,
            ..FrontendConfig::default()
        }
    }

    /// A client for the tests that send conditional request headers, which `reqwest::get` cannot.
    ///
    /// Built rather than `Client::new()`: `clippy.toml` disallows that constructor for having no
    /// timeouts and no pool bound.
    fn conditional_client() -> reqwest::Client {
        reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(5))
            .timeout(Duration::from_secs(30))
            .build()
            .unwrap()
    }

    /// The `Content-Security-Policy` the shell is actually served with.
    async fn served_csp(front: SocketAddr) -> String {
        reqwest::get(format!("http://{front}/"))
            .await
            .unwrap()
            .headers()
            .get("content-security-policy")
            .and_then(|value| value.to_str().ok())
            .unwrap_or_default()
            .to_owned()
    }

    /// The `'sha256-…'` source expression the policy has to carry for an inline `script`.
    ///
    /// Goes through the scanner rather than restating a literal, so an assertion cannot pass
    /// against a hash that agrees with the test and with nothing a browser computes.
    fn inline_hash(script: &str) -> String {
        let scan = csp_shell::scan_shell(&format!("<script>{script}</script>"));
        scan.hashes
            .first()
            .unwrap_or_else(|| panic!("no inline script found in {script}"))
            .to_string()
    }

    /// The `script-src` directive of `csp`, without the `;` that closes it.
    ///
    /// Split out because `style-src` legitimately carries `'unsafe-inline'`: an assertion over
    /// the whole policy string cannot tell the two apart, and the one that matters is scripts.
    fn script_src(csp: &str) -> &str {
        csp.split(';')
            .map(str::trim)
            .find(|directive| directive.starts_with("script-src "))
            .unwrap_or_else(|| panic!("no script-src directive in {csp}"))
    }

    /// Stand up the real frontend application — the shared stack included — against a stub
    /// upstream on an ephemeral port.
    ///
    /// Goes through [`build_app`], not [`build_router`]: the middleware previously found
    /// missing (request id, ops probes) lives in the stack, so testing only the inner router
    /// would miss a regression.
    async fn spawn_frontend(static_dir: &str, upstream: SocketAddr) -> SocketAddr {
        spawn_frontend_with(test_config(static_dir), upstream).await
    }

    /// As [`spawn_frontend`], for a caller that needs a configuration of its own.
    async fn spawn_frontend_with(frontend: FrontendConfig, upstream: SocketAddr) -> SocketAddr {
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
            &frontend,
            state,
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

        let csp = served_csp(front).await;
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

        let csp = served_csp(front).await;
        assert!(
            csp.contains(&inline_hash("window.tv=1;")),
            "the shell's inline boot script is not admitted: {csp}"
        );
    }

    /// **The bug this pins, which was live in production.** The shell used to be served off disk
    /// by `ServeFile`. Image timestamps are clamped to `SOURCE_DATE_EPOCH`, which this repository
    /// pins to a constant `0` on purpose, so every build answered `Last-Modified: Thu, 01 Jan 1970
    /// 00:00:00 GMT` — and `Cache-Control: no-cache` means *revalidate*, not *do not store*. Every
    /// conditional request therefore succeeded with a `304`, and the browser re-used the previous
    /// build's shell under the current build's headers, indefinitely and across redeploys.
    ///
    /// Both symptoms followed from that one pairing. The stored body's inline scripts were not in
    /// the new build's hash list and the nonce Cloudflare had stamped into it belonged to an older
    /// response, so every inline script was refused; and the stored body named a content-hashed
    /// bundle file the new build does not have, which the SPA fallback answered with the shell,
    /// failing in the browser on its MIME type.
    #[tokio::test]
    async fn a_conditional_request_for_the_shell_is_never_answered_304() {
        let upstream = spawn_stub_upstream().await;
        let dir = write_bundle();
        let front = spawn_frontend(dir.to_str().unwrap(), upstream).await;

        // Both shapes a browser can arrive in. Production sees the first: Cloudflare injects into
        // the HTML, so it strips the `ETag`, and the only validator the browser can store is the
        // `Last-Modified` the fixture reproduces in `clamp_timestamps`.
        let conditional = [
            vec![("if-modified-since", "Thu, 01 Jan 1970 00:00:00 GMT")],
            vec![("if-none-match", "*")],
        ];
        let client = conditional_client();
        for headers in conditional {
            let mut request = client.get(format!("http://{front}/"));
            for (name, value) in &headers {
                request = request.header(*name, *value);
            }
            let response = request.send().await.unwrap();

            assert_eq!(
                response.status(),
                StatusCode::OK,
                "the shell revalidated into a stored body: {headers:?}"
            );
            assert_eq!(response.text().await.unwrap(), SHELL);
        }
    }

    /// The other half of the same requirement: a document that cannot be stored cannot be
    /// revalidated into. `no-store` rather than `no-cache`, and no validator to revalidate with —
    /// a `Last-Modified` or `ETag` here is what a conditional request would answer `304` against.
    #[tokio::test]
    async fn the_shell_is_never_stored() {
        let upstream = spawn_stub_upstream().await;
        let dir = write_bundle();
        let front = spawn_frontend(dir.to_str().unwrap(), upstream).await;

        for path in ["/", "/index.html", "/series/abc/deep"] {
            let response = reqwest::get(format!("http://{front}{path}")).await.unwrap();
            let headers = response.headers();
            assert_eq!(headers.get("cache-control").unwrap(), "no-store", "{path}");
            assert!(headers.get("last-modified").is_none(), "{path}");
            assert!(headers.get("etag").is_none(), "{path}");
        }
    }

    /// The invariant the whole policy rests on: the document the browser receives and the document
    /// whose inline scripts the header admits are the same bytes. Two reads of the same path could
    /// not guarantee it — the shell is now read once and served from memory — so this asserts the
    /// property rather than the mechanism, by scanning what was actually served.
    #[tokio::test]
    async fn the_served_shell_is_the_document_the_policy_admits() {
        let upstream = spawn_stub_upstream().await;
        let dir = write_bundle();
        let front = spawn_frontend(dir.to_str().unwrap(), upstream).await;

        let csp = served_csp(front).await;
        let body = reqwest::get(format!("http://{front}/"))
            .await
            .unwrap()
            .text()
            .await
            .unwrap();

        let scan = csp_shell::scan_shell(&body);
        assert!(
            !scan.hashes.is_empty(),
            "no inline script in the served shell"
        );
        for hash in &scan.hashes {
            assert!(
                csp.contains(&hash.to_string()),
                "the served shell carries an inline script the header does not admit: {csp}"
            );
        }
    }

    /// A bundle file that is not there must be a `404`. Answered with the shell it is a `200` of
    /// HTML where the browser expected a module, which fails on the MIME type and reads as a
    /// bundler or CSP fault rather than the missing file it is — the same reasoning that makes
    /// the licence notices a route of their own.
    #[tokio::test]
    async fn a_missing_hashed_asset_is_a_404_rather_than_the_app_shell() {
        let upstream = spawn_stub_upstream().await;
        let dir = write_bundle();
        let front = spawn_frontend(dir.to_str().unwrap(), upstream).await;

        let response = reqwest::get(format!("http://{front}/assets/gone-dxhdeadbeef.js"))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
        assert_ne!(response.text().await.unwrap(), SHELL);
    }

    /// `/assets/…` filenames carry their own content hash, so a changed file is a changed URL:
    /// the one thing in the bundle that may be cached for a year. The blanket `no-cache` used to
    /// cover these too, costing a revalidation round-trip per asset per load.
    #[tokio::test]
    async fn hashed_assets_are_cached_immutably() {
        let upstream = spawn_stub_upstream().await;
        let dir = write_bundle();
        let front = spawn_frontend(dir.to_str().unwrap(), upstream).await;

        let response = reqwest::get(format!("http://{front}/assets/{HASHED_ASSET}"))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response.headers().get("cache-control").unwrap(),
            "public, max-age=31536000, immutable"
        );
        assert_eq!(response.text().await.unwrap(), "HASHEDJS");
    }

    /// Pinned against a hash computed outside this codebase: `sha256-bhHHL3z2…` is the value MDN
    /// documents for `alert(1)`. The scan itself lives in `csp-shell` now, but a browser
    /// silently refusing a script it should run stays invisible server-side, so the value that
    /// reaches this service's header is pinned here rather than only upstream.
    #[test]
    fn inline_scripts_hash_the_way_a_browser_does() {
        assert_eq!(
            inline_hash("alert(1)"),
            "'sha256-bhHHL3z2vDgxUt0W3dWQOrprscmda2Y5pLsLg4GF+pI='"
        );
    }

    /// An unreadable shell must not take the whole tier down — the API proxy and every hashed
    /// asset still work, only the inline scripts are refused.
    #[test]
    fn a_missing_shell_degrades_to_a_hashless_policy() {
        let missing = std::path::Path::new("./no-such-directory/index.html");
        assert!(read_shell(missing).is_none());
        let policy = content_security_policy(None, missing, &CloudflareConfig::default());
        let csp = header_value(&policy);
        let csp = csp.to_str().unwrap();
        assert!(
            csp.contains("script-src 'self' 'wasm-unsafe-eval';"),
            "{csp}"
        );
    }

    /// Cloudflare's bot products (Bot Fight Mode, JavaScript Detections, the challenge platform)
    /// inject an inline `<script>` at the edge, long after this service hashed the shell. The
    /// documented way to admit it is a nonce, which Cloudflare copies out of the response
    /// header — so a *fresh* one has to reach every response. A constant nonce would pass a
    /// naive "contains `nonce-`" assertion while being `'unsafe-inline'` under another name,
    /// which is why this asserts the two differ.
    #[tokio::test]
    async fn the_cloudflare_nonce_is_fresh_on_every_response() {
        let upstream = spawn_stub_upstream().await;
        let dir = write_bundle();
        let front = spawn_frontend_with(
            FrontendConfig {
                cloudflare: CloudflareConfig {
                    script_nonce: true,
                    turnstile: false,
                },
                ..test_config(dir.to_str().unwrap())
            },
            upstream,
        )
        .await;

        let (first, second) = (served_csp(front).await, served_csp(front).await);
        for csp in [&first, &second] {
            assert!(
                script_src(csp).contains("'nonce-"),
                "no nonce in script-src: {csp}"
            );
            // The nonce is an addition, not a replacement: the shell's own inline scripts are
            // still admitted by hash, so a Cloudflare-less request path is unaffected.
            assert!(
                csp.contains(&inline_hash("window.tv=1;")),
                "the nonce displaced the shell's inline-script hashes: {csp}"
            );
            // Neither concession the nonce exists to avoid may ride along with it.
            assert!(!csp.contains("'unsafe-eval'"), "{csp}");
            assert!(!script_src(csp).contains("'unsafe-inline'"), "{csp}");
        }
        assert_ne!(first, second, "the same nonce was reused across responses");
    }

    /// `csp_shell::Headers::cache_control` is an obligation, not a suggestion: a nonce served
    /// from cache is pinned across every reader for the lifetime of the entry, which is
    /// `'unsafe-inline'` under another name. This tier discharges it with the shell's
    /// unconditional `no-store` rather than by reading the field, so the two have to be pinned
    /// together — weakening it would silently invalidate the whole nonce argument.
    ///
    /// `no-store` and not `no-cache`: the latter permits a stored copy and only requires it to be
    /// revalidated, and a `304` then pairs one response's nonce with another response's body.
    /// That is not hypothetical — see [`a_conditional_request_for_the_shell_is_never_answered_304`].
    #[tokio::test]
    async fn a_nonced_shell_is_served_uncacheable() {
        let upstream = spawn_stub_upstream().await;
        let dir = write_bundle();
        let front = spawn_frontend_with(
            FrontendConfig {
                cloudflare: CloudflareConfig {
                    script_nonce: true,
                    turnstile: false,
                },
                ..test_config(dir.to_str().unwrap())
            },
            upstream,
        )
        .await;

        let response = reqwest::get(format!("http://{front}/")).await.unwrap();
        assert_eq!(response.headers().get("cache-control").unwrap(), "no-store");
    }

    /// Off by default, and off is the whole policy unchanged — a deployment that is not behind
    /// Cloudflare must not carry either concession.
    #[tokio::test]
    async fn the_cloudflare_flags_are_off_by_default() {
        let upstream = spawn_stub_upstream().await;
        let dir = write_bundle();
        let front = spawn_frontend(dir.to_str().unwrap(), upstream).await;

        let csp = served_csp(front).await;
        assert!(!csp.contains("nonce-"), "{csp}");
        assert!(!csp.contains("challenges.cloudflare.com"), "{csp}");
        assert!(!csp.contains("frame-src"), "{csp}");
        // Two responses are byte-identical without the flag, so the header stays cacheable.
        assert_eq!(csp, served_csp(front).await);
    }

    /// Turnstile serves `api.js` and the widget's iframe from one origin `'self'` cannot cover,
    /// and admitting the script without the frame renders an empty box — so the flag has to
    /// reach both directives.
    #[tokio::test]
    async fn the_turnstile_flag_admits_the_widget_origin_in_both_directives() {
        let upstream = spawn_stub_upstream().await;
        let dir = write_bundle();
        let front = spawn_frontend_with(
            FrontendConfig {
                cloudflare: CloudflareConfig {
                    script_nonce: false,
                    turnstile: true,
                },
                ..test_config(dir.to_str().unwrap())
            },
            upstream,
        )
        .await;

        let csp = served_csp(front).await;
        assert!(
            script_src(&csp).contains("https://challenges.cloudflare.com"),
            "{csp}"
        );
        // Seeded from the `default-src 'self'` it replaces, so admitting the widget cannot
        // silently revoke same-origin frames.
        assert!(
            csp.contains("frame-src 'self' https://challenges.cloudflare.com"),
            "{csp}"
        );
        // `frame-ancestors` is what stops this app being framed; the widget being framed *by*
        // it is the opposite direction and must not have loosened it.
        assert!(csp.contains("frame-ancestors 'none'"), "{csp}");
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
        let front = spawn_frontend_with(
            FrontendConfig {
                notices_path: "./no-such-notices-file".to_owned(),
                ..test_config(dir.to_str().unwrap())
            },
            upstream,
        )
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
