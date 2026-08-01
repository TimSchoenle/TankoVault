//! The three-way merge's common-ancestor snapshot (design v2 §B.2/§B.3).

use crate::error::DbResult;
use sqlx::{FromRow, PgExecutor};
use tankovault_domain::SeriesId;
use time::OffsetDateTime;

/// The "common ancestor" snapshot recorded at the last successful reconciliation of a mapped
/// series, so the engine can tell which side(s) actually changed since (design v2 §B.3).
#[derive(Debug, Clone, FromRow)]
pub struct SyncSnapshot {
    pub last_synced_local_progress: Option<f64>,
    pub last_synced_remote_progress: Option<f64>,
    pub last_synced_local_status: Option<String>,
    pub last_synced_remote_status: Option<String>,
    pub last_synced_at: Option<OffsetDateTime>,
}

/// Fetch the stored three-way-merge snapshot for a mapped series, if any.
///
/// # Errors
/// [`crate::DbError::Sqlx`] only; unmapped is `Ok(None)`, never [`crate::DbError::NotFound`].
pub async fn get_snapshot<'e, E: PgExecutor<'e>>(
    exec: E,
    series_id: SeriesId,
    provider: &str,
) -> DbResult<Option<SyncSnapshot>> {
    let row = sqlx::query_as!(
        SyncSnapshot,
        "SELECT last_synced_local_progress, last_synced_remote_progress, \
                last_synced_local_status, last_synced_remote_status, last_synced_at \
         FROM sync_mappings WHERE series_id = $1 AND provider = $2",
        series_id.as_uuid(),
        provider,
    )
    .fetch_optional(exec)
    .await?;
    Ok(row)
}

/// The state both sides are known to agree on after a reconciliation.
///
/// A struct, not positional args: two adjacent `f64`s then two adjacent `&str`s transpose silently.
#[derive(Debug, Clone, Copy)]
pub struct AgreedSnapshot<'a> {
    pub series_id: SeriesId,
    pub provider: &'a str,
    pub local_progress: f64,
    pub remote_progress: f64,
    pub local_status: &'a str,
    pub remote_status: &'a str,
}

/// Record the agreed values as the new three-way-merge snapshot (design v2 §B.3). The
/// `sync_mappings` row must already exist.
///
/// # Errors
/// [`crate::DbError::Sqlx`] only; a missing row matches nothing and is silently `Ok(())`, so
/// reconciliation keeps re-deciding from no ancestor.
pub async fn record_snapshot<'e, E: PgExecutor<'e>>(
    exec: E,
    agreed: &AgreedSnapshot<'_>,
) -> DbResult<()> {
    let AgreedSnapshot {
        series_id,
        provider,
        local_progress,
        remote_progress,
        local_status,
        remote_status,
    } = *agreed;
    sqlx::query!(
        "UPDATE sync_mappings SET \
            last_synced_local_progress  = $3, \
            last_synced_remote_progress = $4, \
            last_synced_local_status    = $5, \
            last_synced_remote_status   = $6, \
            last_synced_at              = now() \
         WHERE series_id = $1 AND provider = $2",
        series_id.as_uuid(),
        provider,
        local_progress,
        remote_progress,
        local_status,
        remote_status,
    )
    .execute(exec)
    .await?;
    Ok(())
}
