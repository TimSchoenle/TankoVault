//! Connection pool construction and migration running.

use crate::error::DbResult;
use secrecy::{ExposeSecret as _, SecretString};
use sqlx::postgres::{PgPool, PgPoolOptions};
use std::time::Duration;

/// The embedded migration set (compiled from `migrations/`, validated at build time).
pub static MIGRATOR: sqlx::migrate::Migrator = sqlx::migrate!("../../migrations");

/// Build a Postgres connection pool.
///
/// `url` is a [`SecretString`] rather than a `&str` because a Postgres DSN carries the
/// password inline. This is the single funnel every service and `xtask` goes through, so
/// typing it here is what forces each of those call sites to hold the DSN in a wrapper of its
/// own rather than a `String` that reaches a log line on the way to this function.
///
/// # Errors
/// Returns [`crate::DbError`] if the pool cannot establish an initial connection. Note that
/// `sqlx` redacts the password from its own connection errors, so a failure here does not
/// undo the wrapper.
pub async fn connect(
    url: &SecretString,
    max_connections: u32,
    acquire_timeout_secs: u64,
) -> DbResult<PgPool> {
    let pool = PgPoolOptions::new()
        .max_connections(max_connections)
        .acquire_timeout(Duration::from_secs(acquire_timeout_secs))
        // sqlx defaults this to `true`, which sends a `SELECT 1` liveness probe on **every**
        // acquisition — an extra network round trip per repository call, and series detail
        // alone makes about eight. The probe buys very little here: the pool already discards
        // a connection whose query fails, and a connection dropped between the probe and the
        // real statement is not covered by it either. Turning it off trades a rare
        // retryable error for a round trip on every call.
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

/// Drop and recreate the `public` schema, then re-apply every migration from
/// scratch. Destructive — intended only for local development (`xtask reset`);
/// there is no code path that calls this from a service.
///
/// `DROP SCHEMA public CASCADE` removes all tables (including `_sqlx_migrations`),
/// enums, indexes, and any extensions installed there; the migration set then
/// rebuilds the whole schema, so this leaves the database in the same state as a
/// fresh `migrate` against an empty database.
///
/// # Errors
/// Returns [`crate::DbError`] if the schema cannot be dropped/recreated or if a
/// migration fails to apply afterwards.
pub async fn reset(pool: &PgPool) -> DbResult<()> {
    sqlx::query("DROP SCHEMA IF EXISTS public CASCADE")
        .execute(pool)
        .await?;
    sqlx::query("CREATE SCHEMA public").execute(pool).await?;
    migrate(pool).await?;
    Ok(())
}
