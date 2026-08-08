//! Connection pool construction and migration running.

use crate::error::DbResult;
use secrecy::{ExposeSecret as _, SecretString};
use sqlx::postgres::{PgPool, PgPoolOptions};
use std::time::Duration;

/// The embedded migration set (compiled from `migrations/`, validated at build time).
///
/// **Adding a file to `migrations/` does not rebuild this.** `sqlx::migrate!` registers no
/// dependency on the directory, so cargo sees nothing changed and the compiled set stays at the
/// previous migration — while `migrations/` on disk says otherwise. The failure is silent and
/// lands somewhere else entirely: the test harness clones a template database that never got the
/// new tables, and the first query against one fails with `relation does not exist`. Touch this
/// file when adding a migration.
pub static MIGRATOR: sqlx::migrate::Migrator = sqlx::migrate!("../../migrations");

/// Build a Postgres connection pool.
///
/// `url` is a [`SecretString`], not `&str`: a DSN carries the password inline, and this is
/// the one funnel every service goes through, so it can't reach a log line as a bare `String`.
///
/// # Errors
/// Returns [`crate::DbError`] if the pool cannot establish an initial connection.
pub async fn connect(
    url: &SecretString,
    max_connections: u32,
    acquire_timeout_secs: u64,
) -> DbResult<PgPool> {
    let pool = PgPoolOptions::new()
        .max_connections(max_connections)
        .acquire_timeout(Duration::from_secs(acquire_timeout_secs))
        // Off trades a rare retryable error for skipping a `SELECT 1` probe per acquisition;
        // the pool already discards a connection whose query fails.
        .test_before_acquire(false)
        .connect(url.expose_secret())
        .await?;
    Ok(pool)
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
