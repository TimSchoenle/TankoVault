//! The three-way merge's common-ancestor snapshot (design v2 §B.2/§B.3).

use crate::error::DbResult;
use sqlx::{FromRow, PgExecutor};
use tankovault_domain::SeriesId;
use time::OffsetDateTime;

/// The "common ancestor" snapshot recorded at the last successful reconciliation of a mapped
/// series, so the engine can tell which side(s) actually changed since (design v2 §B.3).
#[derive(Debug, Clone, FromRow)]
pub struct SyncSnapshot {
    /// Local progress as it stood when both sides last agreed, `None` before the
    /// first reconciliation.
    pub last_synced_local_progress: Option<f64>,
    /// Remote progress at that same moment.
    pub last_synced_remote_progress: Option<f64>,
    /// Local watch status at that same moment.
    pub last_synced_local_status: Option<String>,
    /// Remote watch status at that same moment.
    pub last_synced_remote_status: Option<String>,
    /// When that agreement was recorded, `None` before the first reconciliation.
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

/// Fetch the freshest three-way-merge snapshot across every series mapped to `external_id` —
/// the *linked group*'s common ancestor.
///
/// A group's members are one work as far as the provider is concerned, so their ancestors are
/// written together and agree. They can still diverge across the moment a duplicate joins the
/// group, and the newest of them is the one that describes what the group last agreed with the
/// remote; the never-synced member's empty snapshot is not an ancestor, it is the absence of one.
///
/// # Errors
/// [`crate::DbError::Sqlx`] only; an unmapped external id is `Ok(None)`.
pub async fn get_group_snapshot<'e, E: PgExecutor<'e>>(
    exec: E,
    provider: &str,
    external_id: &str,
) -> DbResult<Option<SyncSnapshot>> {
    let row = sqlx::query_as!(
        SyncSnapshot,
        "SELECT last_synced_local_progress, last_synced_remote_progress, \
                last_synced_local_status, last_synced_remote_status, last_synced_at \
         FROM sync_mappings WHERE provider = $1 AND external_id = $2 \
         ORDER BY last_synced_at DESC NULLS LAST LIMIT 1",
        provider,
        external_id,
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
    /// The mapped series.
    pub series_id: SeriesId,
    /// Which external tracker, as a slug.
    pub provider: &'a str,
    /// Local progress both sides now agree on.
    pub local_progress: f64,
    /// Remote progress both sides now agree on. Equal to the local figure unless the
    /// tracker rounds differently.
    pub remote_progress: f64,
    /// Local watch status both sides now agree on.
    pub local_status: &'a str,
    /// Remote watch status both sides now agree on.
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
