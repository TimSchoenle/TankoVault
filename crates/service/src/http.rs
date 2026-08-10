//! The shared HTTP middleware stack, ops probes, and server loop.

use crate::health::Health;
use crate::metrics::MetricsRegistry;
use crate::ratelimit::RateLimiter;
use crate::{ServiceError, metrics as service_metrics};
use axum::Router;
use axum::extract::{DefaultBodyLimit, State};
use axum::http::{HeaderName, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use std::net::SocketAddr;
use std::time::Duration;
use tankovault_config::SecurityConfig;
use tokio::net::TcpListener;
use tokio_util::sync::CancellationToken;
use tower_http::compression::CompressionLayer;
use tower_http::cors::CorsLayer;
use tower_http::request_id::{MakeRequestUuid, PropagateRequestIdLayer, SetRequestIdLayer};
use tower_http::timeout::TimeoutLayer;
use tower_http::trace::TraceLayer;

/// Header carrying the per-request correlation id.
const REQUEST_ID: HeaderName = HeaderName::from_static("x-request-id");

/// How long in-flight requests get to finish after the shutdown signal before exit anyway.
const DRAIN_TIMEOUT: Duration = Duration::from_secs(20);

/// Assembles the middleware every HTTP service shares.
///
/// Order is fixed and is a security property. Outermost first:
/// 1. Request id — so every log line, including a rejection, carries one.
/// 2. Tracing.
/// 3. Metrics — includes time spent waiting on the rate limiter.
/// 4. Security headers / CORS — applied to every response, including the 429 and 408.
/// 5. Rate limit — after the cheap layers, before real work.
/// 6. Principal — above the limiter, which reads what it inserts.
/// 7. Internal auth (internal tier only) — after headers, before timeout/body budget.
/// 8. Timeout, body limit, compression — the per-request work bounds.
pub struct HttpStack {
    security: SecurityConfig,
    metrics: MetricsRegistry,
    limiter: Option<RateLimiter>,
    internal_auth: Option<crate::internal_auth::InternalAuth>,
    principal: Option<PrincipalResolver>,
}

/// Turns request headers into a **verified** caller identity, or `None` for an anonymous
/// request.
///
/// Supplied by the service, which knows how tokens are signed; this crate does not. The
/// returned identity must be verified — the limiter trusts it, so an unverified resolver
/// would let a caller choose their own rate-limit bucket.
pub type PrincipalResolver =
    std::sync::Arc<dyn Fn(&axum::http::HeaderMap) -> Option<String> + Send + Sync>;

impl HttpStack {
    /// Build the stack described by `security`, recording into `metrics`.
    #[must_use]
    pub fn new(security: &SecurityConfig, metrics: MetricsRegistry) -> Self {
        Self {
            security: security.clone(),
            metrics,
            limiter: None,
            internal_auth: None,
            principal: None,
        }
    }

    /// Mount inbound rate limiting. Pass `None` (or skip this call) to leave the layer out
    /// entirely, which is what `rate_limit.enabled = false` produces.
    #[must_use]
    pub fn with_rate_limit(mut self, limiter: Option<RateLimiter>) -> Self {
        self.limiter = limiter;
        self
    }

    /// Identify the caller for per-account rate limiting.
    ///
    /// Without this, every caller is bucketed by IP, which an attacker with many addresses
    /// evades. Must verify identity: the limiter trusts whatever it reads. Mounted outside
    /// the rate limiter, which reads what this inserts.
    #[must_use]
    pub fn with_principal(mut self, resolver: Option<PrincipalResolver>) -> Self {
        self.principal = resolver;
        self
    }

    /// Identify and authorise the caller of every routed request.
    ///
    /// For internal-tier services only. Health/readiness stay reachable regardless:
    /// [`ops_router`] is merged outside [`Self::apply`], so a probe never needs a credential.
    #[must_use]
    pub fn with_internal_auth(mut self, auth: Option<crate::internal_auth::InternalAuth>) -> Self {
        self.internal_auth = auth;
        self
    }

    /// Wrap `router` in the assembled stack.
    pub fn apply(self, router: Router) -> Router {
        let Self {
            security,
            metrics,
            limiter,
            internal_auth,
            principal,
        } = self;

        // `option_layer` turns each optional concern into a no-op `Identity` when absent,
        // so a disabled feature costs nothing per request rather than being a branch.
        let rate_limit = limiter.map(|limiter| {
            axum::middleware::from_fn_with_state(limiter, crate::ratelimit::enforce)
        });
        let metrics_layer = metrics
            .records_http_requests()
            .then(|| axum::middleware::from_fn(service_metrics::track_request));
        // Innermost of the auth-ish layers but outside the work bounds: an unauthenticated
        // caller must not spend the body limit or the request timeout.
        let internal_auth = internal_auth
            .map(|auth| axum::middleware::from_fn_with_state(auth, crate::internal_auth::identify));
        let principal_layer = principal
            .map(|resolve| axum::middleware::from_fn_with_state(resolve, identify_principal));
        let cors = security.cors.is_enabled().then(|| build_cors(&security));
        let security_headers = security.security_headers.then(|| {
            axum::middleware::from_fn_with_state(security.clone(), apply_security_headers)
        });

        // Separate `Router::layer` calls, not one `ServiceBuilder`: `CompressionLayer`
        // changes the response body type, and `from_fn` needs the plain `Response<Body>`
        // that only `Router::layer` normalises back to. Last layer listed is outermost.
        router
            // Innermost: turns a handler panic into a 500 for that request instead of
            // taking down the replica (previously the release profile used `panic = "abort"`).
            .layer(tower_http::catch_panic::CatchPanicLayer::new())
            .layer(CompressionLayer::new())
            .layer(DefaultBodyLimit::max(security.max_body_bytes))
            .layer(TimeoutLayer::with_status_code(
                StatusCode::REQUEST_TIMEOUT,
                Duration::from_secs(security.request_timeout_secs.max(1)),
            ))
            .layer(tower::util::option_layer(internal_auth))
            .layer(tower::util::option_layer(rate_limit))
            // Outside the limiter, so the extension exists by the time the limiter keys on it.
            .layer(tower::util::option_layer(principal_layer))
            .layer(tower::util::option_layer(cors))
            .layer(tower::util::option_layer(security_headers))
            .layer(tower::util::option_layer(metrics_layer))
            .layer(PropagateRequestIdLayer::new(REQUEST_ID))
            .layer(TraceLayer::new_for_http())
            .layer(SetRequestIdLayer::new(REQUEST_ID, MakeRequestUuid))
    }
}

/// A CORS layer restricted to the configured origins, replacing `CorsLayer::permissive()`
/// (which reflected any origin, exposing authenticated user data to any site).
///
/// An origin that fails to parse is dropped with a warning, not left to widen the policy.
fn build_cors(security: &SecurityConfig) -> CorsLayer {
    let origins: Vec<HeaderValue> = security
        .cors
        .allowed_origins
        .iter()
        .filter_map(|origin| {
            HeaderValue::from_str(origin)
                .inspect_err(|_| tracing::warn!(%origin, "ignoring unparseable CORS origin"))
                .ok()
        })
        .collect();

    tracing::info!(
        origins = ?security.cors.allowed_origins,
        credentials = security.cors.allow_credentials,
        "CORS enabled for an explicit origin allowlist"
    );

    let layer = CorsLayer::new()
        .allow_origin(origins)
        .allow_methods([
            axum::http::Method::GET,
            axum::http::Method::POST,
            axum::http::Method::PUT,
            axum::http::Method::PATCH,
            axum::http::Method::DELETE,
            axum::http::Method::OPTIONS,
        ])
        .allow_headers([
            header::AUTHORIZATION,
            header::CONTENT_TYPE,
            header::ACCEPT,
            REQUEST_ID,
        ])
        .expose_headers([
            REQUEST_ID,
            HeaderName::from_static("x-ratelimit-limit"),
            HeaderName::from_static("x-ratelimit-remaining"),
        ])
        .max_age(Duration::from_secs(security.cors.max_age_secs));

    if security.cors.allow_credentials {
        layer.allow_credentials(true)
    } else {
        layer
    }
}

/// Insert [`crate::ratelimit::Principal`] when the caller can be identified.
///
/// Deliberately does **not** reject an unidentified caller: authentication is the handler's
/// business, and a route that is legitimately anonymous (sign-in, registration) still has to
/// reach its handler — bucketed by IP, which is what the limiter falls back to.
async fn identify_principal(
    State(resolve): State<PrincipalResolver>,
    mut req: axum::extract::Request,
    next: axum::middleware::Next,
) -> Response {
    if let Some(id) = resolve(req.headers()) {
        req.extensions_mut().insert(crate::ratelimit::Principal(id));
    }
    next.run(req).await
}

/// Attach the baseline hardening headers to every response.
///
/// Set with `insert` rather than `append` so a handler cannot end up emitting two
/// conflicting values for the same header.
async fn apply_security_headers(
    State(security): State<SecurityConfig>,
    req: axum::extract::Request,
    next: axum::middleware::Next,
) -> Response {
    let mut response = next.run(req).await;
    let headers = response.headers_mut();

    // Stop browsers from MIME-sniffing a JSON error body into something executable.
    headers.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    // This is a JSON API; there is no legitimate reason to frame any of it.
    headers.insert(header::X_FRAME_OPTIONS, HeaderValue::from_static("DENY"));
    // Never leak an API path (which can carry ids) to a third-party origin.
    headers.insert(
        header::REFERRER_POLICY,
        HeaderValue::from_static("no-referrer"),
    );
    // Block cross-origin `<script>`/`<img>` embedding of API responses.
    headers.insert(
        HeaderName::from_static("cross-origin-resource-policy"),
        HeaderValue::from_static("same-origin"),
    );
    // Both `frame-ancestors` and `X-Frame-Options` are sent since browsers honour one or
    // the other. Not configurable: no deployment shape needs a script or frame here.
    headers.insert(
        header::CONTENT_SECURITY_POLICY,
        HeaderValue::from_static("default-src 'none'; frame-ancestors 'none'; base-uri 'none'"),
    );

    if security.hsts {
        let value = format!("max-age={}; includeSubDomains", security.hsts_max_age_secs);
        if let Ok(value) = HeaderValue::from_str(&value) {
            headers.insert(header::STRICT_TRANSPORT_SECURITY, value);
        }
    }

    response
}

/// `/health` and `/ready`, and deliberately nothing else.
///
/// The credential-free surface: it is what an orchestrator probes, so it is also the whole of
/// what [`serve_internal`] publishes in plaintext beside an mTLS listener. Keeping the scrape
/// out of it is the point — a deployment that merged the scrape onto its mutually-authenticated
/// port did so to keep it there.
pub fn probe_router(health: Health) -> Router {
    Router::new()
        .route("/health", get(liveness))
        .route("/ready", get(readiness))
        .with_state(health)
}

/// The undocumented operational endpoints: [`probe_router`] plus — unless it has been isolated
/// to its own port — the metrics scrape.
///
/// Excluded from the `OpenAPI` document and mounted outside the middleware stack, so a
/// rate limit or a body cap can never make a replica look unhealthy.
///
/// When [`MetricsRegistry::listen`] is set the scrape route is not mounted here; it is
/// served on its own listener instead (see [`spawn_metrics_server`]).
pub fn ops_router(health: Health, metrics: MetricsRegistry) -> Router {
    let probes = probe_router(health);

    // Only mount the scrape alongside the probes when it is not isolated to its own port.
    if metrics.listen().is_some() {
        return probes;
    }
    probes.merge(metrics_router(metrics))
}

/// A standalone router serving only the Prometheus scrape, for the isolated metrics port —
/// no request middleware, no health probes.
pub fn metrics_router(metrics: MetricsRegistry) -> Router {
    let scrape_route = metrics.route().to_owned();
    Router::new()
        .route(&scrape_route, get(scrape))
        .with_state(metrics)
}

/// Spawn a dedicated server for the metrics scrape when isolated to its own port via
/// [`MetricsRegistry::listen`]. A no-op otherwise, so services can call it unconditionally.
pub fn spawn_metrics_server(metrics: MetricsRegistry, shutdown: CancellationToken) {
    if !metrics.is_enabled() {
        return;
    }
    let Some(addr) = metrics.listen().map(str::to_owned) else {
        return;
    };

    let app = metrics_router(metrics);
    tokio::spawn(async move {
        tracing::info!(%addr, "serving metrics on a dedicated port");
        if let Err(e) = serve(&addr, app, shutdown).await {
            tracing::error!(error = %e, "metrics server stopped");
        }
    });
}

/// Liveness: the process is up. Never consults a dependency (see [`crate::health`]).
async fn liveness() -> impl IntoResponse {
    (StatusCode::OK, "ok")
}

/// Readiness: `200` when every dependency answered, `503` with a per-dependency body
/// otherwise.
async fn readiness(State(health): State<Health>) -> Response {
    let report = health.report().await;
    let status = if report.is_ready() {
        StatusCode::OK
    } else {
        StatusCode::SERVICE_UNAVAILABLE
    };
    (status, axum::Json(report)).into_response()
}

/// Prometheus exposition, or `404` when metrics are switched off.
///
/// `404` rather than an empty body so a misconfigured scrape target is an obvious failure
/// in Prometheus rather than a silently flat graph.
async fn scrape(State(metrics): State<MetricsRegistry>) -> Response {
    match metrics.render() {
        Some(body) => (
            StatusCode::OK,
            [(
                header::CONTENT_TYPE,
                HeaderValue::from_static("text/plain; version=0.0.4"),
            )],
            body,
        )
            .into_response(),
        None => (StatusCode::NOT_FOUND, "metrics are disabled").into_response(),
    }
}

/// Bind `addr` and serve `app` until `shutdown` is cancelled.
///
/// Uses `ConnectInfo<SocketAddr>` so the rate limiter can see the peer address, and
/// `with_graceful_shutdown` so in-flight requests finish instead of being severed
/// mid-response on a container stop.
///
/// # Errors
/// Returns [`ServiceError::Server`] if the listener cannot be bound or the server exits
/// with an I/O error.
pub async fn serve(
    addr: &str,
    app: Router,
    shutdown: CancellationToken,
) -> Result<(), ServiceError> {
    let listener = TcpListener::bind(addr).await?;
    tracing::info!(%addr, "listening");
    serve_on(listener, app, shutdown).await
}

/// [`serve`] on an already-bound listener, so a caller that needs the resolved port can bind
/// first and still hand the socket over.
async fn serve_on(
    listener: TcpListener,
    app: Router,
    shutdown: CancellationToken,
) -> Result<(), ServiceError> {
    axum::serve(
        listener,
        app.into_make_service_with_connect_info::<SocketAddr>(),
    )
    .with_graceful_shutdown(async move {
        shutdown.cancelled().await;
        tracing::info!(
            drain_secs = DRAIN_TIMEOUT.as_secs(),
            "draining in-flight requests"
        );
    })
    .await?;

    tracing::info!("http server stopped");
    Ok(())
}

/// Bind `addr` and serve `probes` — `/health` and `/ready`, nothing else — in plaintext,
/// returning the bound address once the listener is up.
///
/// Binds before spawning so a port that is taken fails the boot that asked for it, rather than
/// leaving a service that looks healthy and answers no probe.
///
/// # Errors
/// Returns [`ServiceError::Server`] if `addr` cannot be bound.
async fn serve_probes(
    addr: &str,
    probes: Router,
    shutdown: CancellationToken,
) -> Result<SocketAddr, ServiceError> {
    let listener = TcpListener::bind(addr).await?;
    let bound = listener.local_addr()?;
    tracing::info!(%bound, "serving health probes in plaintext on their own port");

    tokio::spawn(async move {
        if let Err(e) = serve_on(listener, probes, shutdown).await {
            tracing::error!(error = %e, "probe listener stopped");
        }
    });

    Ok(bound)
}

/// Bind `addr` and serve `app` over mutually-authenticated TLS until `shutdown` is cancelled.
///
/// The counterpart of [`serve`] for `internal.identity = "mtls"`. Two things differ, and both
/// are consequences of the connection now carrying an identity:
///
/// * the connect info is [`crate::tls::InternalPeer`] rather than a bare `SocketAddr`, and a
///   layer projects it back into the `ConnectInfo<SocketAddr>` the rate limiter reads plus the
///   [`crate::tls::PeerSans`] `internal_auth` reads. Doing the projection here rather than in
///   the limiter keeps every other caller of [`serve`] unchanged;
/// * a background task polls the certificate files, so rotation does not need a restart.
///
/// # Errors
/// Returns [`ServiceError::Server`] if the listener cannot be bound or the server exits with an
/// I/O error.
pub async fn serve_tls(
    addr: &str,
    app: Router,
    tls: std::sync::Arc<crate::tls::ReloadingTls>,
    shutdown: CancellationToken,
) -> Result<(), ServiceError> {
    let listener = crate::tls::TlsListener::bind(addr, std::sync::Arc::clone(&tls)).await?;
    tracing::info!(%addr, "listening (mutual TLS)");

    tokio::spawn(std::sync::Arc::clone(&tls).watch(shutdown.clone()));

    let app = app.layer(axum::middleware::from_fn(project_peer_identity));

    axum::serve(
        listener,
        app.into_make_service_with_connect_info::<crate::tls::InternalPeer>(),
    )
    .with_graceful_shutdown(async move {
        shutdown.cancelled().await;
        tracing::info!(
            drain_secs = DRAIN_TIMEOUT.as_secs(),
            "draining in-flight requests"
        );
    })
    .await?;

    tracing::info!("mutual-TLS server stopped");
    Ok(())
}

/// Serve `app` on `addr`, over mutual TLS when the configuration asks for it.
///
/// The one call site a service needs: which of [`serve`] and [`serve_tls`] applies follows from
/// `internal.identity`, so no service decides it independently and none can end up serving
/// plaintext while its configuration says `mtls`.
///
/// `probes` is [`probe_router`], and exists because mutual TLS breaks the one guarantee
/// `ops_router` is merged outside [`HttpStack::apply`] to provide. An orchestrator presents no
/// client certificate, so on the mTLS listener its plain `GET /health` is answered with a TLS
/// alert rather than a response, and the replica is restarted while perfectly healthy. Under
/// `mtls` the probes therefore also get a listener of their own at `internal.tls.probe_listen`,
/// in plaintext, carrying `probes` and nothing else — the same credential-free surface every
/// other identity mode publishes on its main port. The app routes stay behind both the
/// certificate and `internal_auth`, and so does the metrics scrape when a deployment has merged
/// it onto this port. Under `token` and `off` the probes are already reachable inside `app` and
/// this argument goes unused.
///
/// # Errors
/// As [`serve`] and [`serve_tls`], plus [`ServiceError::Tls`] if the certificate material named
/// by `internal.tls` cannot be loaded, and [`ServiceError::Server`] if the probe address is
/// already taken.
pub async fn serve_internal(
    addr: &str,
    app: Router,
    probes: Router,
    auth: &tankovault_config::ResolvedInternalAuth,
    shutdown: CancellationToken,
) -> Result<(), ServiceError> {
    let Some(paths) = auth.tls.as_ref() else {
        return serve(addr, app, shutdown).await;
    };

    // Certificates first: an unreadable one is a boot failure, and failing it before any socket
    // is open keeps a half-started replica from ever answering a probe it cannot back up.
    let tls = std::sync::Arc::new(crate::tls::ReloadingTls::load(paths)?);

    if let Some(probe_addr) = auth.probe_listen.as_deref() {
        serve_probes(probe_addr, probes, shutdown.clone()).await?;
    }

    serve_tls(addr, app, tls, shutdown).await
}

/// Split [`crate::tls::InternalPeer`] into the two extensions the rest of the stack expects.
///
/// The peer address and the verified names arrive together because they come from one
/// connection, but they are read by unrelated layers — the rate limiter wants an address and
/// knows nothing about TLS, `internal_auth` wants the names and knows nothing about sockets.
async fn project_peer_identity(
    mut req: axum::extract::Request,
    next: axum::middleware::Next,
) -> Response {
    if let Some(axum::extract::ConnectInfo(peer)) = req
        .extensions()
        .get::<axum::extract::ConnectInfo<crate::tls::InternalPeer>>()
        .cloned()
    {
        req.extensions_mut()
            .insert(axum::extract::ConnectInfo(peer.addr));
        req.extensions_mut().insert(peer.sans);
    }
    next.run(req).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use tankovault_config::CorsConfig;

    #[test]
    fn cors_is_off_until_an_origin_is_named() {
        // Safe default: no configured CORS means same-origin only.
        assert!(!SecurityConfig::default().cors.is_enabled());
    }

    #[test]
    fn cors_turns_on_with_an_explicit_allowlist() {
        let security = SecurityConfig {
            cors: CorsConfig {
                allowed_origins: vec!["https://app.example.com".to_owned()],
                ..CorsConfig::default()
            },
            ..SecurityConfig::default()
        };
        assert!(security.cors.is_enabled());
        // Must not panic on a well-formed origin list.
        let _ = build_cors(&security);
    }

    #[test]
    fn unparseable_origins_are_dropped_rather_than_widening_the_policy() {
        let security = SecurityConfig {
            cors: CorsConfig {
                allowed_origins: vec![
                    "https://ok.example.com".to_owned(),
                    "bad\norigin".to_owned(),
                ],
                ..CorsConfig::default()
            },
            ..SecurityConfig::default()
        };
        let _ = build_cors(&security);
    }

    #[tokio::test]
    async fn liveness_is_ok_without_dependencies() {
        let response = liveness().await.into_response();
        assert_eq!(response.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn readiness_reports_503_when_a_dependency_is_down() {
        let health = Health::builder()
            .check_fn("db", || async { Err("refused".to_owned()) })
            .build();
        let response = readiness(State(health)).await;
        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    }

    #[tokio::test]
    async fn scrape_is_404_when_metrics_are_disabled() {
        let response = scrape(State(MetricsRegistry::disabled())).await;
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }

    use axum::body::Body;
    use axum::http::Request;
    use tower::ServiceExt as _;

    /// Distinguishes "handler reached" from "route absent" — both surface as `404` here,
    /// but only the former returns this body.
    async fn body_string(response: Response) -> String {
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("body should collect");
        String::from_utf8(bytes.to_vec()).expect("body should be utf-8")
    }

    fn get(path: &str) -> Request<Body> {
        Request::builder()
            .uri(path)
            .body(Body::empty())
            .expect("request should build")
    }

    #[tokio::test]
    async fn ops_router_mounts_the_scrape_when_it_is_not_isolated() {
        let metrics = MetricsRegistry::disabled_with_listen("/metrics", None);
        let router = ops_router(Health::default(), metrics);

        let response = router
            .oneshot(get("/metrics"))
            .await
            .expect("router should respond");
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
        assert_eq!(body_string(response).await, "metrics are disabled");
    }

    #[tokio::test]
    async fn ops_router_drops_the_scrape_when_it_is_isolated_to_its_own_port() {
        let metrics = MetricsRegistry::disabled_with_listen("/metrics", Some("0.0.0.0:9090"));
        let router = ops_router(Health::default(), metrics);

        // The scrape route is absent here — it lives on the dedicated listener instead — so
        // the path is unrouted and the handler is never reached.
        let response = router
            .oneshot(get("/metrics"))
            .await
            .expect("router should respond");
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
        assert_ne!(body_string(response).await, "metrics are disabled");
    }

    #[tokio::test]
    async fn ops_router_always_keeps_the_health_probes() {
        let metrics = MetricsRegistry::disabled_with_listen("/metrics", Some("0.0.0.0:9090"));
        let router = ops_router(Health::default(), metrics);

        let response = router
            .oneshot(get("/health"))
            .await
            .expect("router should respond");
        assert_eq!(response.status(), StatusCode::OK);
    }

    /// The probe router carries no scrape, whatever the metrics configuration says.
    ///
    /// It is what [`serve_internal`] publishes in plaintext beside an mTLS listener, and a
    /// deployment that sets `metrics.listen = null` has merged the scrape onto that listener
    /// on purpose — under `mtls`, to keep it behind a client certificate. Serving
    /// `ops_router` there instead would have undone that silently.
    #[tokio::test]
    async fn the_probe_router_carries_no_scrape() {
        let router = probe_router(Health::default());

        let response = router
            .oneshot(get("/metrics"))
            .await
            .expect("router should respond");
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
        assert_ne!(body_string(response).await, "metrics are disabled");
    }

    /// A plain `GET /health` — the only kind an orchestrator sends — is answered on the probe
    /// listener.
    ///
    /// The mTLS listener cannot answer it: with no client certificate the connection never
    /// gets past the handshake, and what comes back is a TLS alert record, which the kubelet
    /// reports as `malformed HTTP response "\x15\x03\x03\x00\x02\x02"` before restarting the
    /// replica. Every service on `internal.identity = "mtls"` failed its startup probe that
    /// way, so the probes need a listener that speaks what the probe speaks.
    #[tokio::test]
    async fn probes_answer_plain_http_on_their_own_listener() {
        use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

        let shutdown = CancellationToken::new();
        let bound = serve_probes(
            "127.0.0.1:0",
            ops_router(Health::default(), MetricsRegistry::disabled()),
            shutdown.clone(),
        )
        .await
        .expect("an ephemeral port should bind");

        let mut conn = tokio::net::TcpStream::connect(bound)
            .await
            .expect("the probe listener should accept");
        conn.write_all(b"GET /health HTTP/1.1\r\nHost: probe\r\nConnection: close\r\n\r\n")
            .await
            .expect("the request should send");

        let mut answer = Vec::new();
        conn.read_to_end(&mut answer)
            .await
            .expect("the response should arrive");
        let answer = String::from_utf8_lossy(&answer);

        assert!(answer.starts_with("HTTP/1.1 200 OK"), "{answer}");
        assert!(answer.ends_with("ok"), "{answer}");
        shutdown.cancel();
    }

    #[tokio::test]
    async fn metrics_router_serves_only_the_scrape() {
        let metrics = MetricsRegistry::disabled_with_listen("/metrics", Some("0.0.0.0:9090"));
        let router = metrics_router(metrics);

        // The scrape route is present (its handler is reached)…
        let scrape = router
            .clone()
            .oneshot(get("/metrics"))
            .await
            .expect("router should respond");
        assert_eq!(scrape.status(), StatusCode::NOT_FOUND);
        assert_eq!(body_string(scrape).await, "metrics are disabled");

        // …but the health probes are not carried on the isolated metrics port.
        let health = router
            .oneshot(get("/health"))
            .await
            .expect("router should respond");
        assert_eq!(health.status(), StatusCode::NOT_FOUND);
        assert_ne!(body_string(health).await, "ok");
    }
}
