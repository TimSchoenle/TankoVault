//! In-process HTTP harness for the API service: wires the real Axum router to an isolated,
//! migrated database and answers requests via `tower`'s `oneshot`, with no socket bound. Kept
//! separate from `crates/test-support` so repository-layer suites don't compile the full API
//! stack to run SQL tests.
//!
//! # On `# Panics`
//!
//! Exempt from `clippy::missing_panics_doc`: a harness helper panicking is its contract — a
//! failed test — so documenting each site would only restate that once per function.
#![expect(
    clippy::missing_panics_doc,
    reason = "a test-harness helper's failure mode is a panicking test, which is its contract"
)]

use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, Response, StatusCode};
use secrecy::ExposeSecret as _;
use tankovault_api::AppState;
use tankovault_api::stream_tickets::{MemoryStreamTickets, StreamTicketStore as _};
use tankovault_config::RateLimitConfig;
use tankovault_domain::{AccountStatus, Permission, UserId};
use tankovault_email::EmailService;
use tankovault_service::{FeatureGate, Health, MetricsRegistry};
use tankovault_test_support::{RecordingAuditSink, TestDb, bearer, test_jwt_secret};
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
    features: FeatureGate,
    tunables: tankovault_service::TunableSet,
    webauthn: Option<tankovault_api::SharedRelyingParty>,
    legal: tankovault_config::LegalConfig,
}

impl Default for TestConfig {
    fn default() -> Self {
        Self {
            mailer: tankovault_email::build(&tankovault_config::EmailConfig::default()),
            rate_limit: RateLimitConfig::default(),
            // Matches production, so the suite exercises the real `__Host-refresh_token` shape.
            cookie_secure: true,
            features: FeatureGate::defaults(),
            tunables: tankovault_service::TunableSet::defaults(),
            webauthn: Some(Arc::new(
                tankovault_api::RelyingParty::from_config(Some("http://localhost"), None, None)
                    .expect("the harness origin builds a relying party")
                    .expect("and is not empty"),
            )),
            // Empty by default, which is what most deployments run: `/v1/legal` answers with an
            // empty index rather than 404ing, and the footer publishes no Legal column.
            legal: tankovault_config::LegalConfig::default(),
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

    /// Publish a set of legal documents, as an operator's `[legal]` section would.
    #[must_use]
    pub fn with_legal(mut self, legal: tankovault_config::LegalConfig) -> Self {
        self.legal = legal;
        self
    }

    /// Do not mount the rate limiter.
    ///
    /// The access-control matrix alone drives every admin route three times over; left on, a
    /// throttle would be misread as an authorization failure.
    #[must_use]
    pub fn without_rate_limiting(mut self) -> Self {
        self.rate_limit.enabled = false;
        self
    }

    /// Wire the deployment that configured no `WebAuthn` origin, so every passkey route answers
    /// `503`.
    ///
    /// Distinct from a switched-off `accounts.passkeys` flag (`404`): this is an operator
    /// misconfiguration, not an absent feature.
    #[must_use]
    pub fn without_passkeys(mut self) -> Self {
        self.webauthn = None;
        self
    }

    /// Wire the local-HTTP development cookie shape: no `Secure`, no `__Host-` prefix, and the
    /// narrow `Path=/v1/auth`.
    #[must_use]
    pub fn with_insecure_cookies(mut self) -> Self {
        self.cookie_secure = false;
        self
    }

    /// Switch `disabled` off, as an operator would on the feature-flag page.
    ///
    /// Before this existed the harness pinned `FeatureGate::defaults()`, so no test could drive
    /// a route whose feature is off — `flags.rs`'s unit tests cover resolution logic only, not
    /// that the gate is mounted on a real router.
    #[must_use]
    pub fn with_features_disabled(mut self, disabled: &[tankovault_domain::Feature]) -> Self {
        self.features = FeatureGate::with_disabled(disabled);
        self
    }

    /// Move named tuning values, so a test can prove a knob reaches the thing it configures
    /// without going through the admin endpoint and a refresh.
    #[must_use]
    pub fn with_tunables(mut self, values: &[(tankovault_domain::Tunable, f64)]) -> Self {
        self.tunables = tankovault_service::TunableSet::with_values(values);
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
    #[expect(
        clippy::disallowed_methods,
        reason = "the three upstreams point at `.invalid` hosts that resolve nowhere: this \
                  harness never issues a request through them, so the timeouts the ban exists \
                  to require have nothing to bound"
    )]
    pub async fn spawn_with(cfg: TestConfig) -> Self {
        let db = TestDb::spawn().await;
        let audit = Arc::new(RecordingAuditSink::default());
        // Held separately so a test can mint a ticket for an arbitrary account, including a
        // suspended one the mint endpoint itself would refuse. See `Self::stream_ticket`.
        let stream_tickets = Arc::new(MemoryStreamTickets::new());

        let state = AppState {
            pool: db.pool.clone(),
            jwt_secret: Arc::new(test_jwt_secret()),
            // Empty: the harness hashes un-peppered, matching a deployment that configured
            // no pepper. `SecretSlice::default()` is an empty slice, not a missing one.
            password_pepper: Arc::new(secrecy::SecretSlice::default()),
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
            features: cfg.features.clone(),
            tunables: cfg.tunables.clone(),
            cookie_secure: cfg.cookie_secure,
            // A real relying party, so passkey routes answer their genuine statuses rather than
            // a blanket `503`. No test drives a browser, so nothing here verifies a signature —
            // only the surrounding surface (auth gates, feature flags, ownership scoping) is
            // exercised.
            webauthn: cfg.webauthn,
            mailer: cfg.mailer,
            email_base_url: "http://localhost".to_owned(),
            legal: tankovault_api::LegalDocs::new(cfg.legal.clone()),
            // Pass-through, not a short TTL: a test that seeds rows and reads a console rollup
            // back must see its own writes.
            system_stats: tankovault_api::Cached::uncached(),
            provider_stats: tankovault_api::Cached::uncached(),
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
    /// The mint endpoint is gated by `AuthUser`, which refuses a suspended account — going
    /// through the store directly keeps that leg of the access matrix testable.
    pub async fn stream_ticket(&self, user: UserId) -> String {
        // Unwrapped here because a test's next move is to put it in a query string.
        self.stream_tickets
            .mint(user)
            .await
            .expect("the in-memory ticket store cannot fail")
            .expose_secret()
            .to_owned()
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
