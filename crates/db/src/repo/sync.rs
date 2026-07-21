//! External-sync persistence (design §15): OAuth accounts and canonical-series
//! mappings for a third-party provider such as `AniList`.
//!
//! Token columns hold **ciphertext only** — the sync service seals them with
//! [`tankovault_auth::SecretBox`] before they reach this layer, so nothing here ever handles
//! plaintext credentials. The `provider` column is the external service key (e.g.
//! `"anilist"`), mirroring the shape used by [`tracking`](super::tracking) entries.

use crate::error::DbResult;
use serde::Serialize;
use sqlx::{FromRow, PgExecutor};
use tankovault_domain::{SeriesId, UserId};
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
    /// The provider's display name for the linked account (e.g. an `AniList` username), kept
    /// current on link and on every sync so the UI can show "Connected as X" without an
    /// extra round-trip.
    pub external_username: Option<String>,
    /// When this account last completed a pull or push.
    pub last_synced_at: Option<OffsetDateTime>,
    /// The most recent sync failure message, if any. Cleared on the next successful sync
    /// (`mark_synced`); set by `record_sync_error`. Admin-visible only (design: Sync console
    /// tab) — never surfaced on the user-facing status endpoint.
    pub last_error: Option<String>,
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
        external_username: Option<String>,
        last_synced_at: Option<OffsetDateTime>,
        last_error: Option<String>,
    }
    let row: Option<Row> = sqlx::query_as(
        "SELECT user_id, provider, access_token, refresh_token, expires_at, \
                external_username, last_synced_at, last_error \
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
        external_username: r.external_username,
        last_synced_at: r.last_synced_at,
        last_error: r.last_error,
    }))
}

/// Record a fresh sync timestamp and (when known) the provider's display name for a linked
/// account. Called after linking (captures the username) and after every pull/push (bumps
/// `last_synced_at`), so the UI can render "Connected as X - last sync Ym ago" without ever
/// calling the external provider on page load. A `None` username leaves the stored one as-is.
pub async fn mark_synced<'e, E: PgExecutor<'e>>(
    exec: E,
    user_id: UserId,
    provider: &str,
    username: Option<&str>,
    synced_at: OffsetDateTime,
) -> DbResult<()> {
    sqlx::query(
        "UPDATE external_accounts \
         SET external_username = COALESCE($3, external_username), last_synced_at = $4, \
             last_error = NULL \
         WHERE user_id = $1 AND provider = $2",
    )
    .bind(user_id.as_uuid())
    .bind(provider)
    .bind(username)
    .bind(synced_at)
    .execute(exec)
    .await?;
    Ok(())
}

/// Record a sync failure for a linked account (admin Sync console tab). Overwritten by the
/// next successful `mark_synced`, which clears it back to `NULL`.
pub async fn record_sync_error<'e, E: PgExecutor<'e>>(
    exec: E,
    user_id: UserId,
    provider: &str,
    error: &str,
) -> DbResult<()> {
    sqlx::query(
        "UPDATE external_accounts SET last_error = $3 WHERE user_id = $1 AND provider = $2",
    )
    .bind(user_id.as_uuid())
    .bind(provider)
    .bind(error)
    .execute(exec)
    .await?;
    Ok(())
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
         ON CONFLICT (series_id, provider) DO UPDATE \
            SET external_id = EXCLUDED.external_id, updated_at = now()",
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

/// List the provider slugs a user has linked an account for. Used by the targeted single-series
/// sync push to fan out only to providers the user actually has, without probing the whole
/// provider registry.
pub async fn list_linked_providers<'e, E: PgExecutor<'e>>(
    exec: E,
    user_id: UserId,
) -> DbResult<Vec<String>> {
    let providers: Vec<String> = sqlx::query_scalar(
        "SELECT provider FROM external_accounts WHERE user_id = $1 ORDER BY provider",
    )
    .bind(user_id.as_uuid())
    .fetch_all(exec)
    .await?;
    Ok(providers)
}

/// Remove a series↔external mapping for `provider`. Returns `true` if a row was removed.
/// The next pull/push re-resolves the series from scratch (title match or search).
pub async fn delete_mapping<'e, E: PgExecutor<'e>>(
    exec: E,
    series_id: SeriesId,
    provider: &str,
) -> DbResult<bool> {
    let result = sqlx::query("DELETE FROM sync_mappings WHERE series_id = $1 AND provider = $2")
        .bind(series_id.as_uuid())
        .bind(provider)
        .execute(exec)
        .await?;
    Ok(result.rows_affected() > 0)
}

/// One row of the admin Sync console's "Linked accounts" table.
#[derive(Debug, Clone, Serialize, FromRow)]
pub struct AdminAccountRow {
    pub user_id: Uuid,
    pub username: String,
    pub provider: String,
    pub external_username: Option<String>,
    #[serde(with = "time::serde::rfc3339::option")]
    pub last_synced_at: Option<OffsetDateTime>,
    pub last_error: Option<String>,
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
}

/// All linked external accounts across every user, newest-error-first then most-recently
/// synced (design: admin Sync console tab).
pub async fn admin_list_accounts<'e, E: PgExecutor<'e>>(
    exec: E,
    limit: i64,
) -> DbResult<Vec<AdminAccountRow>> {
    let rows = sqlx::query_as::<_, AdminAccountRow>(
        "SELECT ea.user_id, u.username, ea.provider, ea.external_username, \
                ea.last_synced_at, ea.last_error, ea.created_at \
         FROM external_accounts ea JOIN users u ON u.id = ea.user_id \
         ORDER BY (ea.last_error IS NOT NULL) DESC, ea.last_synced_at DESC NULLS LAST \
         LIMIT $1",
    )
    .bind(limit)
    .fetch_all(exec)
    .await?;
    Ok(rows)
}

/// One row of the admin Sync console's "Series mappings" table.
#[derive(Debug, Clone, Serialize, FromRow)]
pub struct AdminMappingRow {
    pub series_id: Uuid,
    pub series_title: String,
    pub provider: String,
    pub external_id: String,
    #[serde(with = "time::serde::rfc3339")]
    pub updated_at: OffsetDateTime,
}

/// All series↔external mappings across every provider, most recently updated first.
pub async fn admin_list_mappings<'e, E: PgExecutor<'e>>(
    exec: E,
    limit: i64,
) -> DbResult<Vec<AdminMappingRow>> {
    let rows = sqlx::query_as::<_, AdminMappingRow>(
        "SELECT sm.series_id, s.canonical_title AS series_title, sm.provider, \
                sm.external_id, sm.updated_at \
         FROM sync_mappings sm JOIN series s ON s.id = sm.series_id \
         ORDER BY sm.updated_at DESC \
         LIMIT $1",
    )
    .bind(limit)
    .fetch_all(exec)
    .await?;
    Ok(rows)
}
