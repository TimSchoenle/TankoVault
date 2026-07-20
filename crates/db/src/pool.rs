//! Connection pool construction and migration running.

use crate::error::DbResult;
use sqlx::postgres::{PgPool, PgPoolOptions};
use std::time::Duration;

/// The embedded migration set (compiled from `migrations/`, validated at build time).
pub static MIGRATOR: sqlx::migrate::Migrator = sqlx::migrate!("../../migrations");

/// Build a Postgres connection pool.
///
/// # Errors
/// Returns [`crate::DbError`] if the pool cannot establish an initial connection.
pub async fn connect(
    url: &str,
    max_connections: u32,
    acquire_timeout_secs: u64,
) -> DbResult<PgPool> {
    let pool = PgPoolOptions::new()
        .max_connections(max_connections)
        .acquire_timeout(Duration::from_secs(acquire_timeout_secs))
        .connect(url)
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
