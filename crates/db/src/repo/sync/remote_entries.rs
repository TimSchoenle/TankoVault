//! Snapshots of what a provider's list actually held (design §15), kept for every fetched
//! entry whether or not it matched — the unmatched ones are the queue the admin console works.

use crate::error::DbResult;
use sqlx::PgExecutor;
use tankovault_domain::{SeriesId, UserId};
use time::OffsetDateTime;
use uuid::Uuid;

/// One remote-entry snapshot to persist, as produced by the resolve pass of a reconciliation.
#[derive(Debug, Clone)]
pub struct FetchedRemoteEntry {
    /// The tracker's own id for the entry.
    pub external_id: String,
    /// Title as the tracker spells it, which is what the matcher scores on.
    pub title: String,
    /// Tracking status as the tracker spells it.
    pub status: String,
    /// Chapters read, on the tracker's scale.
    pub progress: f64,
    /// Medium as the tracker spells it.
    pub content_type: String,
    /// Year the tracker gives, `None` when it gives none.
    pub start_year: Option<i32>,
    /// When the tracker says the entry last changed, which is what a three-way
    /// merge compares against the stored snapshot.
    pub updated_at: OffsetDateTime,
    /// The canonical series this entry resolved to, or `None` for the unmatched queue.
    pub series_id: Option<SeriesId>,
}

/// Upsert every fetched remote entry for one account in one statement, not one round trip per
/// entry.
///
/// `DISTINCT ON` is required: `ON CONFLICT DO UPDATE` aborts if a statement touches one row
/// twice, and an untrusted provider list can repeat an `external_id`.
///
/// # Errors
/// [`crate::DbError::Sqlx`] only; an empty `entries` is `Ok(())` with no round trip.
pub async fn upsert_remote_entries<'e, E: PgExecutor<'e>>(
    exec: E,
    user_id: UserId,
    provider: &str,
    entries: &[FetchedRemoteEntry],
) -> DbResult<()> {
    if entries.is_empty() {
        return Ok(());
    }
    let external_ids: Vec<String> = entries.iter().map(|e| e.external_id.clone()).collect();
    let titles: Vec<String> = entries.iter().map(|e| e.title.clone()).collect();
    let statuses: Vec<String> = entries.iter().map(|e| e.status.clone()).collect();
    let progresses: Vec<f64> = entries.iter().map(|e| e.progress).collect();
    let content_types: Vec<String> = entries.iter().map(|e| e.content_type.clone()).collect();
    // `Vec<Option<i32>>` cannot be bound to `int4[]` by sqlx, so the nullable columns travel as
    // parallel "present" flags plus a non-null value array.
    let start_years: Vec<i32> = entries.iter().map(|e| e.start_year.unwrap_or(0)).collect();
    let start_year_present: Vec<bool> = entries.iter().map(|e| e.start_year.is_some()).collect();
    let updated_ats: Vec<OffsetDateTime> = entries.iter().map(|e| e.updated_at).collect();
    let series_ids: Vec<Uuid> = entries
        .iter()
        .map(|e| e.series_id.map_or_else(Uuid::nil, SeriesId::as_uuid))
        .collect();
    let series_present: Vec<bool> = entries.iter().map(|e| e.series_id.is_some()).collect();

    sqlx::query!(
        "INSERT INTO sync_remote_entries \
           (user_id, provider, external_id, title, status, progress, content_type, \
            start_year, updated_at, series_id, fetched_at) \
         SELECT DISTINCT ON (external_id) \
                $1, $2, external_id, title, status, progress, content_type, \
                CASE WHEN year_present THEN start_year END, \
                updated_at, \
                CASE WHEN series_present THEN series_id END, \
                now() \
         FROM UNNEST($3::text[], $4::text[], $5::text[], $6::float8[], $7::text[], \
                     $8::int4[], $9::bool[], $10::timestamptz[], $11::uuid[], $12::bool[]) \
              AS t(external_id, title, status, progress, content_type, \
                   start_year, year_present, updated_at, series_id, series_present) \
         ON CONFLICT (user_id, provider, external_id) DO UPDATE SET \
            title = EXCLUDED.title, status = EXCLUDED.status, progress = EXCLUDED.progress, \
            content_type = EXCLUDED.content_type, start_year = EXCLUDED.start_year, \
            updated_at = EXCLUDED.updated_at, series_id = EXCLUDED.series_id, \
            fetched_at = now()",
        user_id.as_uuid(),
        provider,
        &external_ids,
        &titles,
        &statuses,
        &progresses,
        &content_types,
        &start_years,
        &start_year_present,
        &updated_ats,
        &series_ids,
        &series_present,
    )
    .execute(exec)
    .await?;
    Ok(())
}
