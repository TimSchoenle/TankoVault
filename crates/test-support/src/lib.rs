//! Integration-test harness for `TankoVault`.
//!
//! Two layers of automated access-control testing share this crate:
//!
//! - **Repo-layer** tests drive the guard-rail SQL in `tankovault_db::repo` against a real,
//!   migrated schema via [`TestDb`].
//! - **HTTP-layer** tests drive the real Axum router (extractor, middleware and authorization
//!   wiring) in-process via [`TestApp`] and `tower`'s `oneshot`.
//!
//! # One database setup, not two
//!
//! Both layers run against an **ephemeral Postgres** started with testcontainers. A single
//! container is shared per test binary; each [`TestDb::spawn`] creates its own freshly-migrated
//! database inside it, so tests are hermetic and parallel-safe without a shared, mutable
//! fixture. This reconciles the plan's "`#[sqlx::test]`" and "testcontainers" strands into one
//! consistent setup: the harness owns container lifecycle, migration, seeding and token
//! minting, and there is no divergent `DATABASE_URL` wiring to keep in sync.
//!
//! # Not on the fast path
//!
//! This crate is a dev-dependency only; the DB-backed suites that use it are feature-gated
//! (`integration`) so the default `cargo test --workspace` unit path stays green without Docker.

use std::sync::{Arc, Mutex};
use std::time::Duration as StdDuration;

use axum::body::Body;
use axum::http::{Request, Response, StatusCode};
use sqlx::postgres::PgPoolOptions;
use sqlx::{Connection as _, PgConnection, PgPool};
use tankovault_api::AppState;
use tankovault_config::RateLimitConfig;
use tankovault_domain::{AccountStatus, Permission, UserId};
use tankovault_email::{EmailError, EmailMessage, EmailService};
use tankovault_service::{
    AuditEvent, AuditOutcome, AuditSink, FeatureGate, Health, MetricsRegistry,
};
use testcontainers_modules::postgres::Postgres;
use testcontainers_modules::testcontainers::runners::AsyncRunner;
use testcontainers_modules::testcontainers::{ContainerAsync, ImageExt as _};
use time::Duration;
use tokio::sync::OnceCell;
use tower::ServiceExt as _;
use uuid::Uuid;

/// The JWT signing secret every [`TestApp`] uses. Fixed so a test can mint a token that the
/// router will accept, and long enough to satisfy the production strength check.
const TEST_JWT_SECRET: &[u8] = b"integration-test-jwt-secret-please-rotate-0123456789";

/// The process-wide Postgres container, started once on first use and kept alive for the
/// lifetime of the test binary.
static PG: OnceCell<PgContainer> = OnceCell::const_new();

/// The Postgres image tag the harness runs. Pinned to a modern major so the schema's
/// generated columns and trigram indexes apply exactly as they do in production; the
/// testcontainers default is far too old for them.
const POSTGRES_TAG: &str = "17-alpine";

/// A running Postgres container plus the base URL of its maintenance connection.
struct PgContainer {
    /// Held only to keep the container running; dropping it stops Postgres.
    _container: ContainerAsync<Postgres>,
    /// `postgres://postgres:postgres@host:port` — without a trailing database name.
    base_url: String,
}

/// Start (or return the already-started) shared Postgres container.
async fn shared_container() -> &'static PgContainer {
    PG.get_or_init(|| async {
        let container = Postgres::default()
            .with_tag(POSTGRES_TAG)
            .start()
            .await
            .expect("start ephemeral postgres container (is Docker running?)");
        let host = container.get_host().await.expect("container host");
        let port = container
            .get_host_port_ipv4(5432)
            .await
            .expect("container mapped port");
        PgContainer {
            _container: container,
            base_url: format!("postgres://postgres:postgres@{host}:{port}"),
        }
    })
    .await
}

/// An isolated, freshly-migrated database inside the shared container.
///
/// Each instance owns its own database, so two tests running in parallel never observe each
/// other's rows.
pub struct TestDb {
    /// A pool onto this test's private database.
    pub pool: PgPool,
}

impl TestDb {
    /// Create a new isolated database, run every migration against it, and return a pool.
    pub async fn spawn() -> Self {
        let container = shared_container().await;
        let db_name = format!("tv_test_{}", Uuid::new_v4().simple());

        // `CREATE DATABASE` cannot run inside a transaction, so it goes over a plain
        // connection to the maintenance database rather than through the pool.
        let mut admin = PgConnection::connect(&format!("{}/postgres", container.base_url))
            .await
            .expect("connect to maintenance database");
        // The database name is a generated UUID hex, not user input, so asserting it safe is
        // sound; `CREATE DATABASE` cannot bind parameters, hence the format.
        sqlx::query(sqlx::AssertSqlSafe(format!(
            "CREATE DATABASE \"{db_name}\""
        )))
        .execute(&mut admin)
        .await
        .expect("create isolated test database");
        admin.close().await.ok();

        let pool = PgPoolOptions::new()
            .max_connections(8)
            .connect(&format!("{}/{db_name}", container.base_url))
            .await
            .expect("connect to isolated test database");

        tankovault_db::migrate(&pool)
            .await
            .expect("run migrations against the test database");

        Self { pool }
    }

    /// Seed a user with the given username, capabilities and account status.
    ///
    /// The email is derived from the username so callers need only supply a name unique within
    /// the test. The stored password hash is a placeholder: the harness authenticates by
    /// minting access tokens directly (see [`TestApp::bearer`]), never by logging in.
    pub async fn seed_user(
        &self,
        username: &str,
        perms: &[Permission],
        status: AccountStatus,
    ) -> UserId {
        let email = format!("{username}@example.test");
        let user =
            tankovault_db::repo::users::create(&self.pool, &email, username, "$argon2id$seed")
                .await
                .expect("seed user");
        let user_id = user.id;

        for &permission in perms {
            tankovault_db::repo::permissions::grant(&self.pool, user_id, permission, None)
                .await
                .expect("grant permission");
        }

        if status != AccountStatus::Active {
            tankovault_db::repo::user_admin::set_status(&self.pool, user_id, status, Some("seed"))
                .await
                .expect("set account status");
        }

        user_id
    }
}

/// An [`AuditSink`] that records every event in memory so a test can assert on the audit trail.
///
/// The audit trail is part of the access-control contract: a denied privileged action must
/// leave an `authz.denied` record. This double makes that assertion possible without a database
/// round trip.
#[derive(Default)]
pub struct RecordingAuditSink {
    events: Mutex<Vec<AuditEvent>>,
}

impl RecordingAuditSink {
    /// Every event recorded so far, in order.
    #[must_use]
    pub fn events(&self) -> Vec<AuditEvent> {
        self.events.lock().expect("audit sink mutex").clone()
    }

    /// The recorded events whose outcome is [`AuditOutcome::Denied`].
    #[must_use]
    pub fn denials(&self) -> Vec<AuditEvent> {
        self.events()
            .into_iter()
            .filter(|e| e.outcome == AuditOutcome::Denied)
            .collect()
    }
}

#[async_trait::async_trait]
impl AuditSink for RecordingAuditSink {
    async fn record(&self, event: AuditEvent) {
        self.events.lock().expect("audit sink mutex").push(event);
    }
}

/// An [`EmailService`] that captures every message instead of sending it.
///
/// # Why this exists
///
/// [`TestApp`] previously hard-wired the disabled default mailer, and `auth::register` forks on
/// `state.mailer.is_enabled()`. The consequence was that the **email-verification half of
/// registration had never been executed by any test** — every registration took the
/// "no mailer, activate immediately" branch. So did password reset, confirmation resend, and
/// the address-change notices. This double flips that fork on.
///
/// # Awaiting a fire-and-forget send
///
/// `api::mailer::send_in_background` spawns the send, so the message is not in the recorder
/// when the HTTP response returns. [`Self::next_message`] awaits the spawned task through a
/// channel rather than sleeping, so a test stays deterministic; the timeout is a failure
/// deadline, not a delay.
pub struct RecordingMailer {
    enabled: bool,
    sent: Mutex<Vec<EmailMessage>>,
    tx: tokio::sync::mpsc::UnboundedSender<EmailMessage>,
    rx: tokio::sync::Mutex<tokio::sync::mpsc::UnboundedReceiver<EmailMessage>>,
}

impl Default for RecordingMailer {
    fn default() -> Self {
        Self::enabled()
    }
}

impl RecordingMailer {
    /// A mailer that reports itself as configured, so the flows that branch on
    /// `is_enabled()` take their email-bearing path.
    #[must_use]
    pub fn enabled() -> Self {
        Self::with_enabled(true)
    }

    /// A mailer that records but reports itself as *not* configured — the shape of a
    /// deployment without SMTP, which is the branch the rest of the suite exercises.
    #[must_use]
    pub fn disabled() -> Self {
        Self::with_enabled(false)
    }

    fn with_enabled(enabled: bool) -> Self {
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        Self {
            enabled,
            sent: Mutex::new(Vec::new()),
            tx,
            rx: tokio::sync::Mutex::new(rx),
        }
    }

    /// Every message captured so far, in send order.
    #[must_use]
    pub fn sent(&self) -> Vec<EmailMessage> {
        self.sent.lock().expect("mailer mutex").clone()
    }

    /// Await the next message, failing the test if none arrives.
    ///
    /// # Panics
    /// If no message is delivered within ten seconds, which means the flow under test did not
    /// send one.
    pub async fn next_message(&self) -> EmailMessage {
        let mut rx = self.rx.lock().await;
        tokio::time::timeout(StdDuration::from_secs(10), rx.recv())
            .await
            .expect("a message should have been sent within the deadline")
            .expect("the mailer's channel is kept open by the sender half")
    }

    /// The first `https?://…` run in `text`, with trailing punctuation trimmed.
    ///
    /// Reset and confirmation links are only ever reachable through the email body, so a test
    /// that completes either flow has to read the link back out of the message the way a user
    /// would.
    #[must_use]
    pub fn first_link(text: &str) -> Option<String> {
        let start = text.find("http")?;
        let rest = &text[start..];
        let end = rest.find(|c: char| c.is_whitespace()).unwrap_or(rest.len());
        Some(rest[..end].trim_end_matches(['.', ',', ')']).to_owned())
    }
}

#[async_trait::async_trait]
impl EmailService for RecordingMailer {
    fn is_enabled(&self) -> bool {
        self.enabled
    }

    async fn send(&self, message: EmailMessage) -> Result<(), EmailError> {
        self.sent
            .lock()
            .expect("mailer mutex")
            .push(message.clone());
        // A closed receiver means the test dropped its interest; recording still succeeded.
        let _ = self.tx.send(message);
        Ok(())
    }
}

/// What a [`TestApp`] should be wired with, for the axes that change behaviour.
///
/// Defaults reproduce [`TestApp::spawn`] exactly, so an existing test is unaffected and a new
/// one names only the axis it cares about.
pub struct TestConfig {
    mailer: Arc<dyn EmailService>,
    rate_limit: RateLimitConfig,
}

impl Default for TestConfig {
    fn default() -> Self {
        Self {
            mailer: tankovault_email::build(&tankovault_config::EmailConfig::default()),
            rate_limit: RateLimitConfig::default(),
        }
    }
}

impl TestConfig {
    /// The default wiring: no mailer, production rate limits.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Wire a specific mailer — see [`RecordingMailer`].
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
}

/// The real router wired to an isolated database, ready to answer `oneshot` requests.
///
/// Holds the same JWT secret it signed [`AppState`] with, so [`Self::bearer`] mints tokens the
/// router accepts, and the [`RecordingAuditSink`] so tests can assert audit-on-deny.
pub struct TestApp {
    /// The isolated database, exposed so tests can seed and inspect rows directly.
    pub db: TestDb,
    /// The in-memory audit sink capturing every emitted event.
    pub audit: Arc<RecordingAuditSink>,
    router: axum::Router,
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
            challenge_solver_url: "http://challenge-solver.invalid".to_owned(),
            internal_token: None,
            bus: None,
            audit: audit.clone(),
            features: FeatureGate::defaults(),
            cookie_secure: false,
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

        Self { db, audit, router }
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
        let token = tankovault_auth::issue_access_token(
            TEST_JWT_SECRET,
            user,
            "seed",
            Duration::minutes(15),
        )
        .expect("mint access token");
        format!("Bearer {token}")
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
