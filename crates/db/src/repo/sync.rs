//! External-sync persistence (design §15): OAuth accounts and canonical-series
//! mappings for a third-party provider such as `AniList`.
//!
//! Token columns hold **ciphertext only** — the sync service seals them with
//! [`tankovault_auth::SecretBox`] before they reach this layer, so nothing here ever handles
//! plaintext credentials. The `provider` column is the external service key (e.g.
//! `"anilist"`), mirroring the shape used by [`tracking`](super::tracking) entries.

use crate::error::DbResult;
use tankovault_domain::{SeriesId, UserId};
use sqlx::{FromRow, PgExecutor};
use time::OffsetDateTime;
use uuid::Uuid;

/// A stored external-provider account. `access_token`/`refresh_token` are AES-GCM
/// ciphertext (see module docs); callers decrypt with the sync service's data key.
#[derive(Debug, Clone)]
pub struct ExternalAccount {
    pub user_id: UserId,
    pub provider: String,
    pub access_token: Vec<u8>,
    pub refresh_token: Option<Vec<u8>>,
    pub expires_at: Option<OffsetDateTime>,
}

/// Insert or replace a user's account for `provider`. Idempotent on `(user_id, provider)`,
/// so re-linking (e.g. a token refresh) overwrites the prior ciphertext in place.
pub async fn upsert_account<'e, E: PgExecutor<'e>>(
    exec: E,
    user_id: UserId,
    provider: &str,
    access_token: &[u8],
    refresh_token: Option<&[u8]>,
    expires_at: Option<OffsetDateTime>,
) -> DbResult<()> {
    sqlx::query(
        "INSERT INTO external_accounts \
            (user_id, provider, access_token, refresh_token, expires_at) \
         VALUES ($1,$2,$3,$4,$5) \
         ON CONFLICT (user_id, provider) DO UPDATE \
            SET access_token  = EXCLUDED.access_token, \
                refresh_token = EXCLUDED.refresh_token, \
                expires_at    = EXCLUDED.expires_at",
    )
    .bind(user_id.as_uuid())
    .bind(provider)
    .bind(access_token)
    .bind(refresh_token)
    .bind(expires_at)
    .execute(exec)
    .await?;
    Ok(())
}

/// Fetch a user's account for `provider`, if linked.
pub async fn get_account<'e, E: PgExecutor<'e>>(
    exec: E,
    user_id: UserId,
    provider: &str,
) -> DbResult<Option<ExternalAccount>> {
    #[derive(FromRow)]
    struct Row {
        user_id: Uuid,
        provider: String,
        access_token: Vec<u8>,
        refresh_token: Option<Vec<u8>>,
        expires_at: Option<OffsetDateTime>,
    }
    let row: Option<Row> = sqlx::query_as(
        "SELECT user_id, provider, access_token, refresh_token, expires_at \
         FROM external_accounts WHERE user_id = $1 AND provider = $2",
    )
    .bind(user_id.as_uuid())
    .bind(provider)
    .fetch_optional(exec)
    .await?;
    Ok(row.map(|r| ExternalAccount {
        user_id: UserId::from_uuid(r.user_id),
        provider: r.provider,
        access_token: r.access_token,
        refresh_token: r.refresh_token,
        expires_at: r.expires_at,
    }))
}

/// Unlink a user's account for `provider`. Returns `true` if a row was removed.
pub async fn delete_account<'e, E: PgExecutor<'e>>(
    exec: E,
    user_id: UserId,
    provider: &str,
) -> DbResult<bool> {
    let result = sqlx::query("DELETE FROM external_accounts WHERE user_id = $1 AND provider = $2")
        .bind(user_id.as_uuid())
        .bind(provider)
        .execute(exec)
        .await?;
    Ok(result.rows_affected() > 0)
}

/// Record (or refresh) the mapping between a canonical series and its external id at
/// `provider`. Idempotent on `(series_id, provider)`.
pub async fn upsert_mapping<'e, E: PgExecutor<'e>>(
    exec: E,
    series_id: SeriesId,
    provider: &str,
    external_id: &str,
) -> DbResult<()> {
    sqlx::query(
        "INSERT INTO sync_mappings (series_id, provider, external_id) \
         VALUES ($1,$2,$3) \
         ON CONFLICT (series_id, provider) DO UPDATE SET external_id = EXCLUDED.external_id",
    )
    .bind(series_id.as_uuid())
    .bind(provider)
    .bind(external_id)
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
    let id: Option<Uuid> = sqlx::query_scalar(
        "SELECT series_id FROM sync_mappings WHERE provider = $1 AND external_id = $2",
    )
    .bind(provider)
    .bind(external_id)
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
    let ext: Option<String> = sqlx::query_scalar(
        "SELECT external_id FROM sync_mappings WHERE series_id = $1 AND provider = $2",
    )
    .bind(series_id.as_uuid())
    .bind(provider)
    .fetch_optional(exec)
    .await?;
    Ok(ext)
}
