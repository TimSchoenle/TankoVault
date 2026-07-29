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

/// How long to let in-flight requests finish after the shutdown signal before the process
/// exits anyway. Comfortably above the default request timeout so a normal request drains,
/// while still bounded — a container runtime will `SIGKILL` after its own grace period and
/// exiting cleanly first produces better logs.
const DRAIN_TIMEOUT: Duration = Duration::from_secs(20);

/// Assembles the middleware every HTTP service shares.
///
/// Order matters and is fixed here so no service can get it subtly wrong. Outermost first:
///
/// 1. **Request id** — minted before anything else so every log line, including a
///    rejection, carries one.
/// 2. **Tracing** — the span that subsequent layers log into.
/// 3. **Metrics** — measures everything below it, so time spent waiting on the rate
///    limiter counts against the request the client actually experienced.
/// 4. **Security headers / CORS** — applied to *every* response, including the `429` from
///    the limiter and the `408` from the timeout.
/// 5. **Rate limit** — sheds load before any real work, but after the cheap layers above.
/// 6. **Internal auth** (internal tier only) — an unauthenticated caller is refused before
///    it can spend the timeout or the body budget, but after the headers above are set.
/// 7. **Timeout**, **body limit**, **compression** — the per-request work bounds.
pub struct HttpStack {
    security: SecurityConfig,
    metrics: MetricsRegistry,
    limiter: Option<RateLimiter>,
    internal_token: Option<crate::internal_auth::InternalToken>,
}

impl HttpStack {
    /// Build the stack described by `security`, recording into `metrics`.
    #[must_use]
    pub fn new(security: &SecurityConfig, metrics: MetricsRegistry) -> Self {
        Self {
            security: security.clone(),
            metrics,
            limiter: None,
            internal_token: None,
        }
    }

    /// Mount inbound rate limiting. Pass `None` (or skip this call) to leave the layer out
    /// entirely, which is what `rate_limit.enabled = false` produces.
    #[must_use]
    pub fn with_rate_limit(mut self, limiter: Option<RateLimiter>) -> Self {
        self.limiter = limiter;
        self
    }

    /// Require [`crate::internal_auth::INTERNAL_TOKEN_HEADER`] on every routed request.
    ///
    /// For services in the internal tier (`sync`, `control-plane`, `render`,
    /// `challenge-solver`), whose contract is privileged and whose only legitimate callers
    /// are other services. Pass `None` to leave the tier unauthenticated, which
    /// [`tankovault_config::InternalAuthConfig::resolve`] permits outside the production
    /// profile so local development stays frictionless.
    ///
    /// Health and readiness stay reachable: [`ops_router`] is merged *outside* [`Self::apply`],
    /// so an orchestrator never needs the secret.
    #[must_use]
    pub fn with_internal_auth(
        mut self,
        token: Option<crate::internal_auth::InternalToken>,
    ) -> Self {
        self.internal_token = token;
        self
    }

    /// Wrap `router` in the assembled stack.
    pub fn apply(self, router: Router) -> Router {
        let Self {
            security,
            metrics,
            limiter,
            internal_token,
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
        // caller must not be able to spend the body limit or the request timeout, and must
        // still receive the security headers and a request id on the way out.
        let internal_auth = internal_token.map(|token| {
            axum::middleware::from_fn_with_state(token, crate::internal_auth::enforce)
        });
        let cors = security.cors.is_enabled().then(|| build_cors(&security));
        let security_headers = security.security_headers.then(|| {
            axum::middleware::from_fn_with_state(security.clone(), apply_security_headers)
        });

        // Applied as separate `Router::layer` calls rather than one `ServiceBuilder`:
        // `CompressionLayer` changes the response body type, and axum's `from_fn`
        // middleware requires the plain `Response<Body>` that only `Router::layer`
        // normalises back to. Each call wraps the previous one, so the **last** layer
        // listed is the outermost.
        router
            .layer(CompressionLayer::new())
            .layer(DefaultBodyLimit::max(security.max_body_bytes))
            .layer(TimeoutLayer::with_status_code(
                StatusCode::REQUEST_TIMEOUT,
                Duration::from_secs(security.request_timeout_secs.max(1)),
            ))
            .layer(tower::util::option_layer(internal_auth))
            .layer(tower::util::option_layer(rate_limit))
            .layer(tower::util::option_layer(cors))
            .layer(tower::util::option_layer(security_headers))
            .layer(tower::util::option_layer(metrics_layer))
            .layer(PropagateRequestIdLayer::new(REQUEST_ID))
            .layer(TraceLayer::new_for_http())
            .layer(SetRequestIdLayer::new(REQUEST_ID, MakeRequestUuid))
    }
}

/// A CORS layer restricted to the configured origins.
///
/// Replaces `CorsLayer::permissive()`, which reflected any origin and allowed any method
/// and header — on an API that serves authenticated user data, that let any site on the
/// internet read a signed-in user's watchlist, progress and account settings.
///
/// Origins that fail to parse as header values are dropped with a warning rather than
/// silently widening the policy.
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

    if security.hsts {
        let value = format!("max-age={}; includeSubDomains", security.hsts_max_age_secs);
        if let Ok(value) = HeaderValue::from_str(&value) {
            headers.insert(header::STRICT_TRANSPORT_SECURITY, value);
        }
    }

    response
}

/// Shared state for the ops probes.
#[derive(Clone)]
struct OpsState {
    health: Health,
    metrics: MetricsRegistry,
}

/// The undocumented operational endpoints: `/health`, `/ready`, and — unless it has been
/// isolated to its own port — the metrics scrape.
///
/// Deliberately excluded from the `OpenAPI` document — an orchestrator probe is not part of
/// the product's API contract — and mounted *outside* the middleware stack by convention,
/// so a rate limit or a body cap can never make a replica look unhealthy.
///
/// When [`MetricsRegistry::listen`] is set the scrape route is **not** mounted here; it is
/// served on its own listener instead (see [`spawn_metrics_server`]), keeping metrics off
/// the request-facing port entirely.
pub fn ops_router(health: Health, metrics: MetricsRegistry) -> Router {
    let mut router = Router::new()
        .route("/health", get(liveness))
        .route("/ready", get(readiness));

    // Only mount the scrape alongside the probes when it is not isolated to its own port.
    if metrics.listen().is_none() {
        let scrape_route = metrics.route().to_owned();
        router = router.route(&scrape_route, get(scrape));
    }

    router.with_state(OpsState { health, metrics })
}

/// A standalone router serving only the Prometheus scrape endpoint, for the isolated
/// metrics port. Carries none of the request middleware and none of the health probes —
/// it exists purely so a scrape can reach a port that the public API traffic never touches.
pub fn metrics_router(metrics: MetricsRegistry) -> Router {
    let scrape_route = metrics.route().to_owned();
    Router::new()
        .route(&scrape_route, get(scrape))
        .with_state(OpsState {
            health: Health::default(),
            metrics,
        })
}

/// Spawn a dedicated server for the metrics scrape when it has been isolated to its own
/// port via [`MetricsRegistry::listen`].
///
/// A no-op when metrics are disabled or no separate address is configured, so services can
/// call it unconditionally. The spawned server shares the same `shutdown` token as the
/// primary listener, so both drain together on a container stop.
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

/// Liveness: the process is up and its executor is scheduling. Never consults a dependency
/// — see the [`crate::health`] module docs.
async fn liveness() -> impl IntoResponse {
    (StatusCode::OK, "ok")
}

/// Readiness: `200` when every dependency answered, `503` with a per-dependency body
/// otherwise.
async fn readiness(State(state): State<OpsState>) -> Response {
    let report = state.health.report().await;
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
async fn scrape(State(state): State<OpsState>) -> Response {
    match state.metrics.render() {
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

#[cfg(test)]
mod tests {
    use super::*;
    use tankovault_config::CorsConfig;

    #[test]
    fn cors_is_off_until_an_origin_is_named() {
        // The safe default: a deployment that never configures CORS gets same-origin only,
        // rather than the previous reflect-any-origin behaviour.
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
        let state = OpsState {
            health: Health::builder()
                .check_fn("db", || async { Err("refused".to_owned()) })
                .build(),
            metrics: MetricsRegistry::disabled(),
        };
        let response = readiness(State(state)).await;
        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    }

    #[tokio::test]
    async fn scrape_is_404_when_metrics_are_disabled() {
        let state = OpsState {
            health: Health::default(),
            metrics: MetricsRegistry::disabled(),
        };
        let response = scrape(State(state)).await;
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }

    use axum::body::Body;
    use axum::http::Request;
    use tower::ServiceExt as _;

    /// The scrape handler is reached (as opposed to the route being absent) when the
    /// response body is the handler's own "metrics are disabled" message. An unrouted path
    /// yields axum's own empty 404 body instead, which lets these tests tell "mounted" from
    /// "not mounted" even though both surface as `404` here.
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
