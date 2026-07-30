//! In-process HTTP harness for the API service.
//!
//! [`TestApp`] wires the **real** Axum router — extractors, middleware and authorization
//! included — to an isolated, freshly-migrated database from
//! [`tankovault_test_support::TestDb`], and answers requests through `tower`'s `oneshot`. No
//! socket is bound and no network is touched.
//!
//! # Why this is a separate crate
//!
//! Only the router harness needs `tankovault-api`. Keeping it here rather than in
//! `crates/test-support` means the repository-layer suites (`cargo test -p tankovault-db`) no
//! longer compile the API service and its transitive stack to run SQL tests, and the lowest
//! layer of the workspace no longer has a dev-time dependency on the highest (ARCH-17).

use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, Response, StatusCode};
use tankovault_api::AppState;
use tankovault_api::stream_tickets::{MemoryStreamTickets, StreamTicketStore as _};
use tankovault_config::RateLimitConfig;
use tankovault_domain::{AccountStatus, Permission, UserId};
use tankovault_email::EmailService;
use tankovault_service::{FeatureGate, Health, MetricsRegistry};
use tankovault_test_support::{RecordingAuditSink, TEST_JWT_SECRET, TestDb, bearer};
use time::Duration;
use tower::ServiceExt as _;

/// What a [`TestApp`] should be wired with, for the axes that change behaviour.
///
/// Defaults reproduce [`TestApp::spawn`] exactly, so an existing test is unaffected and a new
/// one names only the axis it cares about.
pub struct TestConfig {
    mailer: Arc<dyn EmailService>,
    rate_limit: RateLimitConfig,
    cookie_secure: bool,
}

impl Default for TestConfig {
    fn default() -> Self {
        Self {
            mailer: tankovault_email::build(&tankovault_config::EmailConfig::default()),
            rate_limit: RateLimitConfig::default(),
            // Matches the production default, which is what makes the suite exercise the
            // `__Host-refresh_token` / `Path=/` shape a real deployment issues (SEC-7). The
            // previous `false` meant every cookie assertion in the suite was checking the
            // local-HTTP development spelling.
            cookie_secure: true,
        }
    }
}

impl TestConfig {
    /// The default wiring: no mailer, production rate limits.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Wire a specific mailer — see [`tankovault_test_support::RecordingMailer`].
    #[must_use]
    pub fn with_mailer(mut self, mailer: Arc<dyn EmailService>) -> Self {
        self.mailer = mailer;
        self
    }

    /// Do not mount the rate limiter.
    ///
    /// For suites that issue more requests than a real client would in a minute — the
    /// access-control matrix drives every admin route three times over. Without this the
    /// limiter answers `429` part-way through and the suite reports an authorization failure
    /// that is really a throttle.
    #[must_use]
    pub fn without_rate_limiting(mut self) -> Self {
        self.rate_limit.enabled = false;
        self
    }

    /// Wire the local-HTTP development cookie shape: no `Secure`, no `__Host-` prefix, and the
    /// narrow `Path=/v1/auth` (SEC-7). For the test that pins that opt-out; everything else
    /// should stay on the production default.
    #[must_use]
    pub fn with_insecure_cookies(mut self) -> Self {
        self.cookie_secure = false;
        self
    }
}

/// The real router wired to an isolated database, ready to answer `oneshot` requests.
///
/// Holds the [`RecordingAuditSink`] the router was built with so tests can assert
/// audit-on-deny, and mints tokens under the same secret it signed [`AppState`] with.
pub struct TestApp {
    /// The isolated database, exposed so tests can seed and inspect rows directly.
    pub db: TestDb,
    /// The in-memory audit sink capturing every emitted event.
    pub audit: Arc<RecordingAuditSink>,
    router: axum::Router,
    /// The same store the router was built with, so [`Self::stream_ticket`] can mint one.
    stream_tickets: Arc<MemoryStreamTickets>,
}

impl TestApp {
    /// Stand up an ephemeral database and the fully-wired router against it.
    pub async fn spawn() -> Self {
        Self::spawn_with(TestConfig::new()).await
    }

    /// As [`Self::spawn`], with the wiring axes in `cfg` overridden.
    pub async fn spawn_with(cfg: TestConfig) -> Self {
        let db = TestDb::spawn().await;
        let audit = Arc::new(RecordingAuditSink::default());
        // Held as well as handed to the router so a test can mint a ticket for an arbitrary
        // account — which the access matrix needs, because the mint *endpoint* refuses the
        // suspended account whose stream leg it has to drive. See `Self::stream_ticket`.
        let stream_tickets = Arc::new(MemoryStreamTickets::new());

        let state = AppState {
            pool: db.pool.clone(),
            jwt_secret: Arc::new(TEST_JWT_SECRET.to_vec()),
            password_pepper: Arc::new(Vec::new()),
            access_ttl: Duration::minutes(15),
            refresh_ttl: Duration::days(30),
            control_plane: tankovault_api::Upstream::new(
                reqwest::Client::new(),
                "http://control-plane.invalid",
                None,
                "control-plane",
            ),
            sync: tankovault_api::Upstream::new(
                reqwest::Client::new(),
                "http://sync.invalid",
                None,
                "sync",
            ),
            worker: tankovault_api::Upstream::new(
                reqwest::Client::new(),
                "http://worker.invalid",
                None,
                "worker",
            ),
            bus: None,
            stream_tickets: stream_tickets.clone(),
            audit: audit.clone(),
            features: FeatureGate::defaults(),
            cookie_secure: cfg.cookie_secure,
            mailer: cfg.mailer,
            email_base_url: "http://localhost".to_owned(),
        };

        let router = tankovault_api::build_router(
            state,
            &tankovault_config::SecurityConfig::default(),
            &cfg.rate_limit,
            MetricsRegistry::disabled(),
            Health::builder().build(),
            None,
        );

        Self {
            db,
            audit,
            router,
            stream_tickets,
        }
    }

    /// Mint a single-use stream ticket for `user`, bypassing `POST /v1/me/stream-ticket`.
    ///
    /// Needed because the mint endpoint is gated by `AuthUser`, which refuses a suspended
    /// account — so the access matrix could not otherwise drive `GET /v1/me/stream`'s *own*
    /// suspension check, which is the leg SEC-8 exists for. Going through the store directly
    /// keeps that leg in the sweep instead of dropping the row.
    pub async fn stream_ticket(&self, user: UserId) -> String {
        self.stream_tickets
            .mint(user)
            .await
            .expect("the in-memory ticket store cannot fail")
    }

    /// Seed a user with the given capabilities and status. See [`TestDb::seed_user`].
    pub async fn seed_user(
        &self,
        username: &str,
        perms: &[Permission],
        status: AccountStatus,
    ) -> UserId {
        self.db.seed_user(username, perms, status).await
    }

    /// A `Bearer …` header value carrying a freshly-minted, valid access token for `user`.
    #[must_use]
    pub fn bearer(&self, user: UserId) -> String {
        bearer(user)
    }

    /// Drive a raw request through the real router.
    pub async fn request(&self, req: Request<Body>) -> Response<Body> {
        self.router
            .clone()
            .oneshot(req)
            .await
            .expect("router is infallible")
    }

    /// Issue a request and return the status and JSON body (or [`serde_json::Value::Null`] when
    /// the body is empty), so a test reads as a single assertion.
    pub async fn call(
        &self,
        method: &str,
        path: &str,
        bearer: Option<&str>,
        body: Option<serde_json::Value>,
    ) -> (StatusCode, serde_json::Value) {
        let mut builder = Request::builder().method(method).uri(path);
        if let Some(bearer) = bearer {
            builder = builder.header(axum::http::header::AUTHORIZATION, bearer);
        }
        let request = match body {
            Some(json) => builder
                .header(axum::http::header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    serde_json::to_vec(&json).expect("serialize body"),
                ))
                .expect("build request"),
            None => builder.body(Body::empty()).expect("build request"),
        };

        let response = self.request(request).await;
        let status = response.status();
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("read response body");
        let json = if bytes.is_empty() {
            serde_json::Value::Null
        } else {
            serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null)
        };
        (status, json)
    }
}
