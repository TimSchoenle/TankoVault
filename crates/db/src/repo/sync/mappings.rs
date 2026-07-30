//! The canonical series â‡† external id correspondence, cached so a later sync skips matching.

use crate::error::DbResult;
use sqlx::PgExecutor;
use tankovault_domain::{SeriesId, UserId};
use uuid::Uuid;

/// Record (or refresh) the mapping between a canonical series and its external id at
/// `provider`. Idempotent on `(series_id, provider)`.
pub async fn upsert_mapping<'e, E: PgExecutor<'e>>(
    exec: E,
    series_id: SeriesId,
    provider: &str,
    external_id: &str,
) -> DbResult<()> {
    sqlx::query!(
        "INSERT INTO sync_mappings (series_id, provider, external_id) \
         VALUES ($1,$2,$3) \
         ON CONFLICT (series_id, provider) DO UPDATE \
            SET external_id = EXCLUDED.external_id, updated_at = now()",
        series_id.as_uuid(),
        provider,
        external_id,
    )
    .execute(exec)
    .await?;
    Ok(())
}

/// Resolve a provider's external id to a canonical series, if already mapped. Used to
/// short-circuit title re-matching on subsequent syncs.
pub async fn mapping_series_for_external<'e, E: PgExecutor<'e>>(
    exec: E,
    provider: &str,
    external_id: &str,
) -> DbResult<Option<SeriesId>> {
    let id = sqlx::query_scalar!(
        "SELECT series_id FROM sync_mappings WHERE provider = $1 AND external_id = $2",
        provider,
        external_id,
    )
    .fetch_optional(exec)
    .await?;
    Ok(id.map(SeriesId::from_uuid))
}

/// Resolve a canonical series to its external id at `provider`, if mapped. Used by push
/// to target the correct remote entry.
pub async fn mapping_external_for_series<'e, E: PgExecutor<'e>>(
    exec: E,
    series_id: SeriesId,
    provider: &str,
) -> DbResult<Option<String>> {
    let ext = sqlx::query_scalar!(
        "SELECT external_id FROM sync_mappings WHERE series_id = $1 AND provider = $2",
        series_id.as_uuid(),
        provider,
    )
    .fetch_optional(exec)
    .await?;
    Ok(ext)
}

/// List the provider slugs a user has linked an account for. Used by the targeted single-series
/// sync push to fan out only to providers the user actually has, without probing the whole
/// provider registry.
pub async fn list_linked_providers<'e, E: PgExecutor<'e>>(
    exec: E,
    user_id: UserId,
) -> DbResult<Vec<String>> {
    let providers = sqlx::query_scalar!(
        "SELECT provider FROM external_accounts WHERE user_id = $1 ORDER BY provider",
        user_id.as_uuid(),
    )
    .fetch_all(exec)
    .await?;
    Ok(providers)
}

/// Remove a seriesâ†”external mapping for `provider`. Returns `true` if a row was removed.
/// The next pull/push re-resolves the series from scratch (title match or search).
pub async fn delete_mapping<'e, E: PgExecutor<'e>>(
    exec: E,
    series_id: SeriesId,
    provider: &str,
) -> DbResult<bool> {
    let result = sqlx::query!(
        "DELETE FROM sync_mappings WHERE series_id = $1 AND provider = $2",
        series_id.as_uuid(),
        provider,
    )
    .execute(exec)
    .await?;
    Ok(result.rows_affected() > 0)
}

/// Record (or refresh) the mapping for several series in one statement.
///
/// The batched form of [`upsert_mapping`] (PERF-13). `DISTINCT ON (series_id) â€¦ ORDER BY
/// series_id, ord DESC` reproduces the sequential loop's semantics precisely: `sync_mappings` is
/// keyed on `(series_id, provider)`, so when two remote ids resolve to one series the *last* one
/// wins, just as repeated `upsert_mapping` calls would have left it â€” and without it Postgres
/// would abort the statement for touching the same row twice.
pub async fn upsert_mappings<'e, E: PgExecutor<'e>>(
    exec: E,
    provider: &str,
    pairs: &[(SeriesId, String)],
) -> DbResult<()> {
    if pairs.is_empty() {
        return Ok(());
    }
    let series_ids: Vec<Uuid> = pairs.iter().map(|(s, _)| s.as_uuid()).collect();
    let external_ids: Vec<String> = pairs.iter().map(|(_, e)| e.clone()).collect();
    sqlx::query!(
        "INSERT INTO sync_mappings (series_id, provider, external_id) \
         SELECT DISTINCT ON (series_id) series_id, $1, external_id \
         FROM UNNEST($2::uuid[], $3::text[]) WITH ORDINALITY \
              AS t(series_id, external_id, ord) \
         ORDER BY series_id, ord DESC \
         ON CONFLICT (series_id, provider) DO UPDATE \
            SET external_id = EXCLUDED.external_id, updated_at = now()",
        provider,
        &series_ids,
        &external_ids,
    )
    .execute(exec)
    .await?;
    Ok(())
}
