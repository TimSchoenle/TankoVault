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
use tankovault_db::repo::users::mfa::StepUpMethod;
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
    branding: tankovault_config::BrandingConfig,
    client: tankovault_config::ClientConfig,
    step_up_max_ttl: Duration,
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
                tankovault_api::RelyingParty::from_config(
                    Some("http://localhost"),
                    None,
                    "TankoVault Test",
                )
                .expect("the harness origin builds a relying party")
                .expect("and is not empty"),
            )),
            // Empty by default, which is what most deployments run: `/v1/legal` answers with an
            // empty index rather than 404ing, and the footer publishes no Legal column.
            legal: tankovault_config::LegalConfig::default(),
            // The shipped identity, so `/v1/branding` assertions read as what a stock
            // deployment publishes.
            branding: tankovault_config::BrandingConfig::default(),
            // The upstream channel, so `/v1/client` assertions read as what a stock deployment
            // publishes.
            client: tankovault_config::ClientConfig::default(),
            step_up_max_ttl: Duration::hours(1),
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

    /// Rebrand the deployment, as an operator's `[branding]` section would.
    #[must_use]
    pub fn with_branding(mut self, branding: tankovault_config::BrandingConfig) -> Self {
        self.branding = branding;
        self
    }

    /// Name a different client channel, as an operator's `[client]` section would.
    #[must_use]
    pub fn with_client(mut self, client: tankovault_config::ClientConfig) -> Self {
        self.client = client;
        self
    }

    /// Cap how long an elevation may be honoured after it was earned, whatever it is used for.
    ///
    /// `Duration::ZERO` is the interesting setting and is not a deployment: it makes the ceiling
    /// bite the instant a grant exists, which is the only way to observe it without moving the
    /// clock — the sliding window otherwise keeps a used grant alive right up to it.
    #[must_use]
    pub fn with_step_up_lifetime(mut self, max: Duration) -> Self {
        self.step_up_max_ttl = max;
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

    /// Force named features on. Needed for the three that ship off, which no test can otherwise
    /// reach from the compiled defaults.
    #[must_use]
    pub fn with_features_enabled(mut self, enabled: &[tankovault_domain::Feature]) -> Self {
        self.features = FeatureGate::with_enabled(enabled);
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
            // A fixed key, so TOTP enrolment answers its real statuses rather than the `503` an
            // unconfigured deployment gets. Fixed rather than random because a suite that seals
            // a secret in one request and opens it in the next needs both to use the same key.
            mfa_sealer: Some(test_sealer()),
            totp_issuer: "TankoVault Test".to_owned(),
            step_up_ttl: Duration::minutes(5),
            step_up_max_ttl: cfg.step_up_max_ttl,
            mfa_challenge_ttl: Duration::minutes(5),
            mailer: cfg.mailer,
            email_base_url: "http://localhost".to_owned(),
            legal: tankovault_api::LegalDocs::new(cfg.legal.clone()),
            branding: tankovault_api::Branding::new(cfg.branding.clone()),
            client_channel: tankovault_api::ClientChannel::new(&cfg.client, "0.0.0")
                .expect("the harness client channel resolves"),
            // Pass-through, not a short TTL: a test that seeds rows and reads a console rollup
            // back must see its own writes.
            system_stats: tankovault_api::Cached::uncached(),
            provider_stats: tankovault_api::Cached::uncached(),
            adult_tags: Arc::new(tankovault_domain::AdultTagSet::defaults()),
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

    /// Enrol a confirmed authenticator app for `user`, returning its secret.
    ///
    /// Goes through the repository rather than the HTTP surface because the HTTP surface is
    /// often the thing under test: a suite checking that admin writes demand a step-up should
    /// not have to drive four enrolment requests first, and a failure in one of those would
    /// surface as a confusing failure in the other.
    ///
    /// The secret is sealed with the same fixed key `spawn_with` hands `AppState`, so codes
    /// produced from the returned value are codes the server will accept. Feed it to
    /// [`totp_code`].
    pub async fn seed_totp(&self, user: UserId) -> secrecy::SecretSlice<u8> {
        use secrecy::ExposeSecret as _;
        let secret = tankovault_auth::totp::generate_secret();
        let sealed = test_sealer()
            .seal(secret.expose_secret())
            .expect("seal a test TOTP secret");
        tankovault_db::repo::users::mfa::begin_totp_enrolment(&self.db.pool, user, &sealed, "test")
            .await
            .expect("store the enrolment");
        // Step `0`, i.e. the Unix epoch, is far enough in the past to be no replay floor at all.
        tankovault_db::repo::users::mfa::confirm_totp(&self.db.pool, user, 0)
            .await
            .expect("confirm the enrolment");
        secret
    }

    /// Mint a live step-up grant for `user`, returning the `X-Step-Up` header value.
    ///
    /// Minted through the store rather than `POST /v1/me/step-up`, for the same reason
    /// [`Self::seed_totp`] bypasses the enrolment endpoints — and because the endpoint's own
    /// gate would mask the check under test on the route the grant is being used for.
    ///
    /// `method` decides whether the grant survives enrolment: a `Password` grant stops counting
    /// the moment a factor exists, which is itself a thing worth testing.
    pub async fn step_up(&self, user: UserId, method: StepUpMethod) -> String {
        self.step_up_expiring_at(
            user,
            method,
            time::OffsetDateTime::now_utc() + Duration::minutes(5),
        )
        .await
    }

    /// [`Self::step_up`] with the lapse time chosen, including one already in the past.
    ///
    /// The window is the only thing bounding a stolen grant, and it is enforced by a predicate
    /// in one `SELECT` — drop it and every elevation ever issued becomes permanent, with no
    /// functional test noticing. Reaching the expired case needs a grant minted old, since
    /// nothing else can move the clock.
    pub async fn step_up_expiring_at(
        &self,
        user: UserId,
        method: StepUpMethod,
        expires_at: time::OffsetDateTime,
    ) -> String {
        use secrecy::ExposeSecret as _;
        let token = tankovault_auth::generate_handle();
        tankovault_db::repo::users::mfa::insert_step_up(
            &self.db.pool,
            user,
            &tankovault_auth::hash_handle(&token),
            method,
            expires_at,
        )
        .await
        .expect("store the step-up grant");
        token.expose_secret().to_owned()
    }

    /// Enrol a factor **and** elevate in one call — what most suites want, since almost every
    /// sensitive route needs both and neither is what they are testing.
    pub async fn enrolled_and_elevated(&self, user: UserId) -> String {
        self.seed_totp(user).await;
        self.step_up(user, StepUpMethod::Totp).await
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
        self.call_elevated(method, path, bearer, None, body).await
    }

    /// [`Self::call`], additionally presenting a step-up grant in `X-Step-Up`.
    ///
    /// A separate entry point rather than a sixth parameter on `call`, because the overwhelming
    /// majority of calls carry no elevation and a `None` in every one of them would be noise.
    pub async fn call_elevated(
        &self,
        method: &str,
        path: &str,
        bearer: Option<&str>,
        step_up: Option<&str>,
        body: Option<serde_json::Value>,
    ) -> (StatusCode, serde_json::Value) {
        let mut builder = Request::builder().method(method).uri(path);
        if let Some(bearer) = bearer {
            builder = builder.header(axum::http::header::AUTHORIZATION, bearer);
        }
        if let Some(step_up) = step_up {
            builder = builder.header(tankovault_api::STEP_UP_HEADER, step_up);
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

/// The fixed sealing key every harness instance uses for TOTP secrets.
///
/// Fixed rather than random because a suite that seals a secret through [`TestApp::seed_totp`]
/// and opens it through a request in the next line needs both halves to agree, and because a
/// failure caused by two different random keys reads as "the code was wrong" — the least
/// diagnosable outcome available.
fn test_sealer() -> tankovault_auth::Sealer {
    tankovault_auth::Sealer::new(&[0x2a; 32])
}

/// The code `secret` produces right now, for driving a sign-in or a step-up.
///
/// Computed with the server's own implementation rather than a second one written for tests: a
/// hand-rolled copy that agreed with the RFC but disagreed with this build would fail every
/// suite with "wrong code" and point at nothing.
#[must_use]
pub fn totp_code(secret: &secrecy::SecretSlice<u8>) -> String {
    use secrecy::ExposeSecret as _;
    let step = tankovault_auth::totp::step_at(time::OffsetDateTime::now_utc());
    tankovault_auth::totp::code_at_step(secret, step)
        .expose_secret()
        .to_owned()
}
