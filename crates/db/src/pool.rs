//! Connection pool construction and migration running.

use crate::error::DbResult;
use secrecy::{ExposeSecret as _, SecretString};
use sqlx::Connection as _;
use sqlx::postgres::{PgConnectOptions, PgPool, PgPoolOptions};
use std::str::FromStr as _;
use std::time::Duration;

/// How often a backend checks, while executing, that its client socket is still open.
///
/// What turns a closed connection into an aborted statement: Postgres otherwise notices a gone
/// client only when it next writes a result, which for a 30-second aggregate is 30 seconds of
/// work nobody will read.
const CLIENT_CONNECTION_CHECK: &str = "2s";

/// How long a released connection may take to answer a readiness probe before it is closed.
///
/// A healthy connection answers in one round trip. One that is still busy was released with a
/// statement in flight — its caller's future was dropped — and keeping it would make the next
/// caller wait for that statement to finish before its own could start.
const RELEASE_PROBE: Duration = Duration::from_secs(1);

/// The embedded migration set (compiled from `migrations/`, validated at build time).
///
/// Adding a file to `migrations/` does not rebuild this on its own — `sqlx::migrate!` registers
/// no dependency on the directory — so `build.rs` declares one. Without it the compiled set stays
/// at the previous migration while `migrations/` on disk says otherwise, and the failure is
/// silent: `migrate` reports success having applied nothing, and the first query against the
/// missing table is where it lands.
pub static MIGRATOR: sqlx::migrate::Migrator = sqlx::migrate!("../../migrations");

/// How a pool is sized and how long its statements may run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PoolSettings {
    /// Upper bound on open connections.
    pub max_connections: u32,
    /// How long a caller waits for a free connection before failing.
    pub acquire_timeout: Duration,
    /// Server-side ceiling on a single statement; `None` leaves Postgres's default (none).
    pub statement_timeout: Option<Duration>,
    /// Plan every execution of a prepared statement with its actual parameters.
    pub custom_plans: bool,
}

impl PoolSettings {
    /// A pool with no statement ceiling.
    #[must_use]
    pub const fn new(max_connections: u32, acquire_timeout_secs: u64) -> Self {
        Self {
            max_connections,
            acquire_timeout: Duration::from_secs(acquire_timeout_secs),
            statement_timeout: None,
            custom_plans: false,
        }
    }

    /// The same pool with every statement capped at `secs`; `0` removes the cap.
    #[must_use]
    pub const fn with_statement_timeout_secs(mut self, secs: u64) -> Self {
        self.statement_timeout = if secs == 0 {
            None
        } else {
            Some(Duration::from_secs(secs))
        };
        self
    }

    /// The same pool, planning every statement for the parameters it was given.
    ///
    /// sqlx prepares each statement once per connection, and after five executions Postgres may
    /// switch it to a generic plan costed without parameter values. For a statement built from
    /// `$n IS NULL OR column = $n` arms that plan can use no partial or column index, so a filter
    /// that matches nothing walks the whole table: the console's flagged-decisions view measured
    /// 0.08 ms with a custom plan and 506 ms cold with the generic one, on a 100 000-row journal.
    /// The price is planning on every call, which is right for low-volume, filter-heavy traffic
    /// and wrong for a hot path.
    #[must_use]
    pub const fn with_custom_plans(mut self) -> Self {
        self.custom_plans = true;
        self
    }
}

/// Build a Postgres connection pool.
///
/// `url` is a [`SecretString`], not `&str`: a DSN carries the password inline, and this is
/// the one funnel every service goes through, so it can't reach a log line as a bare `String`.
///
/// # Errors
/// Returns [`crate::DbError`] if the URL does not parse or the pool cannot establish an
/// initial connection.
pub async fn connect(url: &SecretString, settings: PoolSettings) -> DbResult<PgPool> {
    connect_with(PgConnectOptions::from_str(url.expose_secret())?, settings).await
}

/// [`connect`] for callers that already hold parsed options.
///
/// # A dropped query must not outlive its caller
///
/// sqlx cannot cancel a statement. When a request times out or its client disconnects, the
/// query future is dropped and the connection goes back to the pool with the statement still
/// running; the next caller to acquire it then blocks until that statement finishes before its
/// own is even sent. Under load that turns one slow aggregate into a queue of timeouts. Two
/// pieces close it, and both are needed:
///
/// - the release probe closes a connection that cannot answer within a second, so no
///   caller inherits a busy one;
/// - `client_connection_check_interval` makes the backend notice that close and abort the
///   statement, instead of finishing work nobody will read.
///
/// `statement_timeout` bounds what is left: a statement whose caller is still waiting.
///
/// # Errors
/// Returns [`crate::DbError`] if the pool cannot establish an initial connection.
pub async fn connect_with(options: PgConnectOptions, settings: PoolSettings) -> DbResult<PgPool> {
    let mut options =
        options.options([("client_connection_check_interval", CLIENT_CONNECTION_CHECK)]);
    if let Some(timeout) = settings.statement_timeout {
        options = options.options([("statement_timeout", format!("{}ms", timeout.as_millis()))]);
    }
    if settings.custom_plans {
        options = options.options([("plan_cache_mode", "force_custom_plan")]);
    }
    let pool = PgPoolOptions::new()
        .max_connections(settings.max_connections)
        .acquire_timeout(settings.acquire_timeout)
        // Off trades a rare retryable error for skipping a `SELECT 1` probe per acquisition;
        // the pool already discards a connection whose query fails. The release probe below
        // runs in the pool's own task after the caller has its result, not on the request path.
        .test_before_acquire(false)
        .after_release(|conn, _| {
            Box::pin(async move {
                Ok(tokio::time::timeout(RELEASE_PROBE, conn.ping())
                    .await
                    .is_ok_and(|ping| ping.is_ok()))
            })
        })
        .connect_with(options)
        .await?;
    Ok(pool)
}

/// Begin a transaction whose statements run under `ceiling` rather than the pool's own.
///
/// For work that is off the request path by design and slower than the route class it is
/// reached through — a snapshot refresh that aggregates a whole table. The setting is
/// transaction-local, so the connection returns to the pool with its own ceiling intact.
///
/// # Errors
/// [`crate::DbError::Sqlx`] only.
pub async fn begin_with_statement_timeout(
    pool: &PgPool,
    ceiling: Duration,
) -> DbResult<sqlx::Transaction<'static, sqlx::Postgres>> {
    let mut tx = pool.begin().await?;
    sqlx::query_scalar!(
        "SELECT set_config('statement_timeout', $1, true)",
        format!("{}ms", ceiling.as_millis()),
    )
    .fetch_one(&mut *tx)
    .await?;
    Ok(tx)
}

/// Run all pending migrations. Safe to call on every service boot; the `render`
/// tier or a dedicated migration Job typically gates this before app rollout.
///
/// # Errors
/// Returns [`crate::DbError`] if a migration fails to apply.
pub async fn migrate(pool: &PgPool) -> DbResult<()> {
    MIGRATOR.run(pool).await?;
    Ok(())
}

/// Drop and recreate the `public` schema, then re-apply every migration. Destructive —
/// local development only (`xtask reset`); no service calls this.
///
/// # Errors
/// Returns [`crate::DbError`] if the schema can't be recreated or a migration fails.
pub async fn reset(pool: &PgPool) -> DbResult<()> {
    sqlx::query("DROP SCHEMA IF EXISTS public CASCADE")
        .execute(pool)
        .await?;
    sqlx::query("CREATE SCHEMA public").execute(pool).await?;
    migrate(pool).await?;
    Ok(())
}
