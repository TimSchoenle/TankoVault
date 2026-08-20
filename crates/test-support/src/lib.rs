//! Database-layer test harness for `TankoVault`.
//!
//! A Postgres container (via testcontainers) shared by every test binary and run;
//! [`TestDb::spawn`] creates a freshly-migrated database inside it per test — see
//! [`shared_container`] for why the container is reused rather than ephemeral. Also holds
//! service-agnostic in-memory doubles ([`RecordingAuditSink`], [`RecordingMailer`]) and the
//! entity builders in [`seed`].
//!
//! Depends on no `services/*` crate, so repository-layer suites can use it without compiling a
//! service (ARCH-17); the in-process API router harness lives in `services/api/test-support`.
//!
//! # On `# Panics`
//!
//! This crate is exempt from `clippy::missing_panics_doc` (an `expect` at the crate root, so the
//! exemption lapses if the last panicking helper is ever removed): a harness helper panics *by
//! design* — a failure here is a failed test — and `Result` would only move the `unwrap` into
//! every caller.
#![expect(
    clippy::missing_panics_doc,
    reason = "a test-harness helper's failure mode is a panicking test, which is its contract"
)]
//!
//! # Not on the fast path
//!
//! This crate is a dev-dependency only; the DB-backed suites that use it are feature-gated
//! (`integration`) so the default `cargo test --workspace` unit path stays green without Docker.

mod catalogue;
pub mod seed;

use std::sync::Mutex;
use std::time::Duration as StdDuration;

use secrecy::{ExposeSecret as _, SecretSlice};
use sqlx::postgres::PgPoolOptions;
use sqlx::{Connection as _, PgConnection, PgPool};
use tankovault_domain::{AccountStatus, Permission, UserId};
use tankovault_email::{EmailError, EmailMessage, EmailService};
use tankovault_service::{AuditEvent, AuditOutcome, AuditSink};
use testcontainers::runners::AsyncRunner;
use testcontainers::{ContainerAsync, ImageExt as _, ReuseDirective};
use testcontainers_modules::postgres::Postgres;
use time::Duration;
use tokio::sync::OnceCell;
use uuid::Uuid;

/// The JWT signing secret every harness signs with. Fixed so a test can mint a token that the
/// router will accept, and long enough to satisfy the production strength check.
///
/// Kept as raw bytes rather than a [`SecretSlice`] because a `const` cannot hold a heap
/// allocation. [`test_jwt_secret`] is the wrapped form the auth API takes; this constant is
/// the one place in the workspace where the two spellings meet, and it is test-only.
pub const TEST_JWT_SECRET: &[u8] = b"integration-test-jwt-secret-please-rotate-0123456789";

/// [`TEST_JWT_SECRET`] in the [`SecretSlice`] form `tankovault_auth` and the API's `AppState`
/// both take.
#[must_use]
pub fn test_jwt_secret() -> SecretSlice<u8> {
    SecretSlice::from(TEST_JWT_SECRET.to_vec())
}

/// A `Bearer …` header value carrying a freshly-minted, valid access token for `user`, signed
/// with [`TEST_JWT_SECRET`].
///
/// Lives here rather than on the HTTP harness so a repository-layer test can mint the same token
/// the router would accept.
///
/// # Panics
/// If signing fails, which in a test always means the secret is wrong.
#[must_use]
pub fn bearer(user: UserId) -> String {
    let token = tankovault_auth::issue_access_token(
        &test_jwt_secret(),
        user,
        "seed",
        Duration::minutes(15),
    )
    .expect("mint access token");
    // One of the few places a minted token is deliberately unwrapped: it is going into an
    // `Authorization` header a test will send, which is the value's whole purpose.
    format!("Bearer {}", token.expose_secret())
}

/// The process-wide Postgres container, started once on first use and kept alive for the
/// lifetime of the test binary.
static PG: OnceCell<PgContainer> = OnceCell::const_new();

/// The Postgres image the harness runs.
///
/// **Track `deploy/docker-compose.yml`.** The planner is a major-version artefact, so a suite
/// that asserts on query plans ([`TestDb::spawn_with_catalogue`]) proves nothing about
/// production unless the majors match — and the schema's generated columns and trigram indexes
/// need a modern major regardless, which the testcontainers default is not.
///
/// The *name* is overridden as well as the tag: migration 0027 does `CREATE EXTENSION vector`,
/// so stock `postgres` cannot run the migration set at all and every integration test would
/// fail at schema setup rather than on anything it meant to assert.
const POSTGRES_IMAGE: &str = "pgvector/pgvector";
const POSTGRES_TAG: &str = "pg18";

/// The fixed name of the shared container. A *name* is what makes reuse possible: it is how
/// `testcontainers` finds the already-running container instead of creating a second one, so
/// every test binary and every run converge on the same instance. See [`shared_container`].
///
/// The major is **part of the name**, and derived from [`POSTGRES_TAG`] rather than written out,
/// because reuse attaches by name: a container built from an older tag would otherwise be found,
/// started and reused forever after a bump, leaving the suite testing the very major it was
/// meant to leave — silently, since nothing in the run names a version.
///
/// The move to pgvector (`18-alpine` → `pg18`) changes this string, which is the property that
/// matters: a developer with a stock-Postgres container left over from before migration 0027
/// gets a new one rather than a silent `CREATE EXTENSION vector` failure on every run.
fn container_name() -> String {
    let major = POSTGRES_TAG.split('-').next().unwrap_or(POSTGRES_TAG);
    format!("tankovault-test-postgres-{major}")
}

/// How old a `tv_test_*` database must be before the sweep drops it.
///
/// Sized to be far longer than any conceivable test run and far shorter than "until the disk
/// fills". The whole integration suite is minutes, so an hour cannot catch a live database, and
/// a leftover survives at most one further hour of idleness.
const STALE_DB_AFTER: StdDuration = StdDuration::from_secs(60 * 60);

/// The catalogue-fixture template database, cloned by [`TestDb::spawn_with_catalogue`].
///
/// **Bump the version suffix whenever [`catalogue`]'s generator changes — or a migration does.**
///
/// The template caches a *migrated database with rows in it*, so both halves are part of what it
/// caches, and this name is the entire cache key. Only the generator used to be named here, and
/// that omission is a real trap: adding a migration leaves every later run cloning a template
/// built before it, and the failure is `relation "…" does not exist` from a suite that has
/// nothing to do with the change. Rebuilding costs seconds; a stale template costs an
/// afternoon.
///
/// The name deliberately does not match the `tv_test_%` pattern [`sweep_stale_dbs`] drops: it is
/// meant to outlive a run.
const CATALOGUE_TEMPLATE: &str = "tv_catalogue_template_v20";

/// Advisory-lock key serialising catalogue-template creation and cloning across test binaries.
///
/// Two binaries starting together would otherwise both find the template missing and both try to
/// build it. The lock also covers the clone, because `CREATE DATABASE … TEMPLATE` fails outright
/// if anything else is touching the source.
const CATALOGUE_LOCK: i64 = 0x7401_0CA7;

/// A running Postgres container plus the base URL of its maintenance connection.
struct PgContainer {
    /// Held so the handle outlives the pools built from it. Dropping it does **not** stop
    /// Postgres: the container is started with [`ReuseDirective::Always`], and `ContainerAsync`'s
    /// remove-on-drop is skipped for a reused container. That is deliberate — see
    /// [`shared_container`].
    _container: ContainerAsync<Postgres>,
    /// `postgres://postgres:postgres@host:port` — without a trailing database name.
    base_url: String,
}

/// Start (or attach to) the one shared Postgres container, then sweep stale test databases.
///
/// # Why this reuses a container instead of starting a fresh one (ARCH-6b)
///
/// [`PG`] is a `static`, and Rust never drops statics — so `ContainerAsync`'s remove-on-drop
/// never ran, and `testcontainers` 0.25 ships **no** Ryuk-style reaper (its `watchdog` feature
/// fires on `SIGTERM`/`SIGINT`/`SIGQUIT`, not on a test binary exiting normally). The result was
/// one leaked container *per test binary*, on every platform, every `--features integration`
/// run. Upgrading does not fix it: 0.27 has the same feature set in this respect.
///
/// So the container is named and started with [`ReuseDirective::Always`]: the first binary to
/// need it creates it, every later binary and every later run attaches to the same one, and it
/// is never removed. That bounds the count at exactly one forever rather than leaking one per
/// binary — but it moves the cost, because a container that is never removed accumulates the
/// `tv_test_*` databases every [`TestDb::spawn`] creates inside it. Hence [`sweep_stale_dbs`],
/// which is not optional garnish: reuse *without* the sweep trades a container leak for
/// unbounded disk growth, which is why ARCH-6b was left open until both halves existed.
///
/// Removing the container by hand is still always safe — the next run recreates it:
/// `docker rm -f tankovault-test-postgres-18`. That also discards the catalogue template
/// [`TestDb::spawn_with_catalogue`] caches inside it, which the next run rebuilds.
///
/// # Why it starts the container before asking for it (ARCH-6c)
///
/// Reuse attaches to a **running** container. A named container that exists and is *stopped* —
/// which is what every host reboot and every Docker restart leaves behind — matches neither
/// path: reuse does not find it, creation collides with the name, and `testcontainers` surfaces
/// a `409 Conflict` that this call used to report as "is Docker running?" while Docker was
/// plainly running. So the container is started first, unconditionally.
///
/// `docker start` rather than removing and recreating, deliberately. Starting an already-running
/// container is a documented no-op success, so this is safe to issue from every test binary
/// concurrently, whereas removing one on a name conflict would race a *sibling binary* that had
/// just created it. It also preserves the published port, since the mapping is fixed at creation
/// and restored on start. Every failure is ignored: no container yet, no `docker` on `PATH`, or
/// a daemon that is genuinely down all fall through to `start()` below, which reports them.
async fn shared_container() -> &'static PgContainer {
    PG.get_or_init(|| async {
        start_if_stopped();
        let container = Postgres::default()
            .with_name(POSTGRES_IMAGE)
            .with_tag(POSTGRES_TAG)
            .with_container_name(container_name())
            .with_reuse(ReuseDirective::Always)
            .start()
            .await
            .expect("start ephemeral postgres container (is Docker running?)");
        let host = container.get_host().await.expect("container host");
        let port = container
            .get_host_port_ipv4(5432)
            .await
            .expect("container mapped port");
        let base_url = format!("postgres://postgres:postgres@{host}:{port}");
        sweep_stale_dbs(&base_url).await;
        PgContainer {
            _container: container,
            base_url,
        }
    })
    .await
}

/// Best-effort `docker start` of the shared container; see [`shared_container`] for why.
///
/// Deliberately returns nothing. There is no outcome a caller could act on: success means the
/// container was stopped and now is not, and *every* failure — no such container, no `docker`
/// binary, a daemon that is down — is either the normal first-run case or something the
/// `start()` that follows reports with a better message than this could.
fn start_if_stopped() {
    let _ = std::process::Command::new("docker")
        .args(["start", &container_name()])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status();
}

/// Drop every `tv_test_*` database older than [`STALE_DB_AFTER`], once per test binary.
///
/// # Why an age threshold rather than "drop them all"
///
/// The container is shared, so a sweep cannot assume it is alone: another test binary — or
/// another developer's `cargo test` against the same reused container — may be mid-run, and
/// dropping its database would fail that run with an error pointing nowhere near the cause.
/// Age is the only signal available, which is why [`TestDb::spawn`] puts a creation timestamp in
/// the database *name*: `pg_database` records no creation time, so without the name carrying it
/// there is nothing to threshold on. A test still running after [`STALE_DB_AFTER`] does not
/// exist; a database still present after it belongs to a binary that has exited.
///
/// A name that does not parse as `tv_test_<unix-seconds>_<hex>` is treated as stale
/// unconditionally: the only source of one is a run from before this scheme existed, so those
/// are exactly the leaked databases the sweep is for.
///
/// Failures here are logged to stderr and ignored rather than panicking. The sweep is
/// housekeeping; a test run must not fail because a *previous* run's leftovers could not be
/// cleaned up.
async fn sweep_stale_dbs(base_url: &str) {
    let Ok(mut admin) = PgConnection::connect(&format!("{base_url}/postgres")).await else {
        eprintln!("test-support: could not connect to sweep stale test databases");
        return;
    };

    let rows: Vec<(String,)> =
        match sqlx::query_as("SELECT datname FROM pg_database WHERE datname LIKE 'tv_test_%'")
            .fetch_all(&mut admin)
            .await
        {
            Ok(rows) => rows,
            Err(err) => {
                eprintln!("test-support: could not list test databases to sweep: {err}");
                return;
            }
        };

    let now = unix_secs();
    for (name,) in rows {
        if !is_stale(&name, now) {
            continue;
        }
        // The name came out of `pg_database` and matched `tv_test_%`, so it is our own generated
        // identifier round-tripping, not user input; `DROP DATABASE` cannot bind parameters.
        // `WITH (FORCE)` terminates any connection still attached, which a crashed run leaves
        // behind and which would otherwise make the drop fail forever.
        let sql = format!("DROP DATABASE IF EXISTS \"{name}\" WITH (FORCE)");
        if let Err(err) = sqlx::query(sqlx::AssertSqlSafe(sql))
            .execute(&mut admin)
            .await
        {
            eprintln!("test-support: could not drop stale test database {name}: {err}");
        }
    }

    admin.close().await.ok();
}

/// Whether `name` is a `tv_test_*` database old enough to drop, given the current unix time.
///
/// Split out from [`sweep_stale_dbs`] so the parsing rules are testable without Docker — the
/// interesting cases are all name-shaped, and the two that matter are the boundary (a database
/// exactly at the threshold is *not* stale, so a run cannot delete its own database one second
/// early) and the unparseable legacy name.
fn is_stale(name: &str, now: u64) -> bool {
    let Some(rest) = name.strip_prefix("tv_test_") else {
        return false;
    };
    let Some((created, _uuid)) = rest.split_once('_') else {
        // Pre-timestamp name (`tv_test_<uuid>`): only a run from before this scheme could have
        // created it, so it is leaked by definition.
        return true;
    };
    match created.parse::<u64>() {
        Ok(created) => now.saturating_sub(created) > STALE_DB_AFTER.as_secs(),
        Err(_) => true,
    }
}

/// Seconds since the unix epoch, saturating at 0 if the system clock is before it.
fn unix_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

/// A pool onto one test's database inside the shared container.
async fn connect_pool(base_url: &str, db_name: &str) -> PgPool {
    PgPoolOptions::new()
        .max_connections(8)
        .connect(&format!("{base_url}/{db_name}"))
        .await
        .expect("connect to isolated test database")
}

/// A connection to the container's maintenance database, for statements that cannot run inside
/// a transaction or against the database they operate on.
async fn maintenance_conn(base_url: &str) -> PgConnection {
    PgConnection::connect(&format!("{base_url}/postgres"))
        .await
        .expect("connect to maintenance database")
}

/// Create `db_name` as a clone of the catalogue template, building the template first if this is
/// the run that finds it missing.
///
/// Everything happens under [`CATALOGUE_LOCK`]; see there for why the clone is inside it too.
async fn clone_catalogue_template(base_url: &str, db_name: &str) {
    let mut admin = maintenance_conn(base_url).await;
    sqlx::query("SELECT pg_advisory_lock($1)")
        .bind(CATALOGUE_LOCK)
        .execute(&mut admin)
        .await
        .expect("take the catalogue-template lock");

    let exists: bool =
        sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM pg_database WHERE datname = $1)")
            .bind(CATALOGUE_TEMPLATE)
            .fetch_one(&mut admin)
            .await
            .expect("look up the catalogue template");
    if !exists {
        build_catalogue_template(base_url, &mut admin).await;
    }

    // Both names are generated here, not caller input, so asserting them safe is sound;
    // `CREATE DATABASE` cannot bind parameters.
    sqlx::query(sqlx::AssertSqlSafe(format!(
        "CREATE DATABASE \"{db_name}\" TEMPLATE \"{CATALOGUE_TEMPLATE}\""
    )))
    .execute(&mut admin)
    .await
    .expect("clone the catalogue template");

    // Releasing explicitly rather than relying on the close below, so a later refactor that
    // pools this connection cannot turn the lock into a deadlock that only shows up under `-j`.
    sqlx::query("SELECT pg_advisory_unlock($1)")
        .bind(CATALOGUE_LOCK)
        .execute(&mut admin)
        .await
        .expect("release the catalogue-template lock");
    admin.close().await.ok();
}

/// Build the catalogue template. Caller holds [`CATALOGUE_LOCK`].
///
/// # Why it seeds under a scratch name and renames
///
/// The template is a cache with no validity check beyond its name, so a seed that dies halfway —
/// a cancelled `cargo test`, a container restart — must not leave a half-filled database sitting
/// under the template name for every later run to clone. Under a scratch name a crash leaves
/// litter that is merely ignored, and the rename is atomic, so the template name only ever
/// appears once the fixture behind it is complete.
///
/// It ends `ALLOW_CONNECTIONS false` because `CREATE DATABASE … TEMPLATE` refuses to run while
/// anything is connected to the source. Nothing needs to connect to the template again, and
/// forbidding it outright is what stops one stray connection from failing every clone.
async fn build_catalogue_template(base_url: &str, admin: &mut PgConnection) {
    let scratch = format!("tv_catalogue_building_{}", Uuid::new_v4().simple());
    sqlx::query(sqlx::AssertSqlSafe(format!(
        "CREATE DATABASE \"{scratch}\""
    )))
    .execute(&mut *admin)
    .await
    .expect("create the catalogue template scratch database");

    let pool = connect_pool(base_url, &scratch).await;
    tankovault_db::migrate(&pool)
        .await
        .expect("run migrations against the catalogue template");
    catalogue::seed(&pool).await;
    // Awaited, not dropped: the rename below needs every session gone, and `close` is what
    // waits for that.
    pool.close().await;

    sqlx::query(sqlx::AssertSqlSafe(format!(
        "ALTER DATABASE \"{scratch}\" RENAME TO \"{CATALOGUE_TEMPLATE}\""
    )))
    .execute(&mut *admin)
    .await
    .expect("publish the catalogue template");
    sqlx::query(sqlx::AssertSqlSafe(format!(
        "ALTER DATABASE \"{CATALOGUE_TEMPLATE}\" WITH ALLOW_CONNECTIONS false"
    )))
    .execute(&mut *admin)
    .await
    .expect("seal the catalogue template");
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
        // The unix timestamp is load-bearing, not decoration: `pg_database` records no creation
        // time, so the name is the only place an age can live, and the sweep in
        // [`sweep_stale_dbs`] needs an age to distinguish a leftover from a live run's database.
        let db_name = format!("tv_test_{}_{}", unix_secs(), Uuid::new_v4().simple());

        // `CREATE DATABASE` cannot run inside a transaction, so it goes over a plain
        // connection to the maintenance database rather than through the pool.
        let mut admin = maintenance_conn(&container.base_url).await;
        // The database name is a generated UUID hex, not user input, so asserting it safe is
        // sound; `CREATE DATABASE` cannot bind parameters, hence the format.
        sqlx::query(sqlx::AssertSqlSafe(format!(
            "CREATE DATABASE \"{db_name}\""
        )))
        .execute(&mut admin)
        .await
        .expect("create isolated test database");
        admin.close().await.ok();

        let pool = connect_pool(&container.base_url, &db_name).await;

        tankovault_db::migrate(&pool)
            .await
            .expect("run migrations against the test database");

        Self { pool }
    }

    /// As [`TestDb::spawn`], but the database arrives already holding a production-shaped
    /// workload — catalogue, chapters, readers and their watchlists — analysed.
    ///
    /// For suites that assert on **query plans**. Every planner choice is a cost comparison, so
    /// an assertion is only meaningful where the fixture has the volume production has; see
    /// [`catalogue`] for what it has to get right and what it deliberately scales down.
    ///
    /// The rows come from a `CREATE DATABASE … TEMPLATE` clone of a template built once per
    /// container, so the per-test cost is a file copy (~200 ms) rather than a re-seed (~15 s) —
    /// and the clone carries the template's statistics, which is what makes the copy plan like
    /// the original.
    pub async fn spawn_with_catalogue() -> Self {
        let container = shared_container().await;
        let db_name = format!("tv_test_{}_{}", unix_secs(), Uuid::new_v4().simple());
        clone_catalogue_template(&container.base_url, &db_name).await;
        Self {
            pool: connect_pool(&container.base_url, &db_name).await,
        }
    }

    /// Seed a user with the given username, capabilities and account status.
    ///
    /// The email is derived from the username so callers need only supply a name unique within
    /// the test. The stored password hash is a placeholder: the harness authenticates by
    /// minting access tokens directly (see [`bearer`]), never by logging in.
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

    /// Run a statement against this test's database.
    ///
    /// Exists for one job: **aging rows so a time-dependent branch can be reached**. Token
    /// expiry (`RESET_TOKEN_TTL`, `VERIFY_TOKEN_TTL`, refresh lifetime) is compared against
    /// `now()`, and there is no clock seam in `AppState` to move, so the only way to test the
    /// expired branch is to backdate the row. That drives the same `expires_at <= now()`
    /// comparison the handler makes, so dropping or inverting it still fails the test.
    ///
    /// `&'static str` deliberately: a test may not build SQL from a runtime value.
    ///
    /// # Panics
    /// If the statement fails, which in a test always means the fixture is wrong.
    pub async fn execute(&self, sql: &'static str) {
        sqlx::query(sql)
            .execute(&self.pool)
            .await
            .expect("test fixture statement");
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
/// The HTTP harness previously hard-wired the disabled default mailer, and `auth::register` forks on
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
    /// How long `send` pretends the relay takes. Zero unless `with_send_delay` set it.
    send_delay: StdDuration,
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
            send_delay: StdDuration::ZERO,
            sent: Mutex::new(Vec::new()),
            tx,
            rx: tokio::sync::Mutex::new(rx),
        }
    }

    /// A mailer whose every send takes `delay` before it records anything — a stand-in for a
    /// slow SMTP relay.
    ///
    /// For the SEC-10 timing tests: the anti-enumeration property of
    /// `/v1/auth/password/forgot` and `/v1/auth/verify-email/resend` is that the handler does
    /// *no* account-dependent work before answering, and the only step in that work a test can
    /// make arbitrarily slow is the send. With a delay far larger than any plausible handler
    /// cost, "the response came back fast" is a reliable proof that the work was detached
    /// rather than a flaky measurement of one `INSERT`.
    #[must_use]
    pub fn with_send_delay(mut self, delay: StdDuration) -> Self {
        self.send_delay = delay;
        self
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
        if !self.send_delay.is_zero() {
            tokio::time::sleep(self.send_delay).await;
        }
        self.sent
            .lock()
            .expect("mailer mutex")
            .push(message.clone());
        // A closed receiver means the test dropped its interest; recording still succeeded.
        let _ = self.tx.send(message);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::{STALE_DB_AFTER, is_stale};

    const NOW: u64 = 1_800_000_000;

    /// The sweep must not delete a database belonging to a run that is still going.
    ///
    /// The container is shared across test binaries and across developers' runs (ARCH-6b), so a
    /// sweep is never alone. A fresh database — and, at the boundary, one exactly
    /// [`STALE_DB_AFTER`] old — has to survive; only strictly older names may be dropped. If this
    /// ever flips to `>=`, a run whose clock lands on the boundary drops its own database and
    /// fails with an error pointing nowhere near the cause.
    #[test]
    fn a_live_databases_name_is_never_stale() {
        let ttl = STALE_DB_AFTER.as_secs();
        assert!(!is_stale(&format!("tv_test_{NOW}_abc123"), NOW));
        assert!(!is_stale(&format!("tv_test_{}_abc123", NOW - ttl), NOW));
        assert!(is_stale(&format!("tv_test_{}_abc123", NOW - ttl - 1), NOW));
    }

    /// A name from before the timestamp scheme is stale unconditionally.
    ///
    /// `tv_test_<uuid>` is what the harness generated before ARCH-6b, so the only thing that can
    /// have created one is a run that has already exited — which makes these precisely the
    /// leaked databases the sweep exists to remove. Treating them as *fresh* instead (the easy
    /// mistake, since they carry no age) would leave the pre-existing leak permanently.
    #[test]
    fn a_pre_timestamp_name_is_stale() {
        assert!(is_stale("tv_test_9f2b4c8e1a", NOW));
        assert!(is_stale("tv_test_notanumber_abc123", NOW));
    }

    /// Anything that is not one of ours is left alone.
    ///
    /// The sweep runs `DROP DATABASE` inside a container that a developer may also be using for
    /// something else. The `LIKE 'tv_test_%'` filter is the first guard and this is the second,
    /// so a widened query cannot turn into a data-loss bug on its own.
    #[test]
    fn a_foreign_database_is_never_stale() {
        assert!(!is_stale("postgres", NOW));
        assert!(!is_stale("tankovault", NOW));
        assert!(!is_stale("tv_prod_1700000000_abc123", NOW));
    }
}
