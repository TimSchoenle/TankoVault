//! Linked provider accounts and their automatic-sync settings.
//!
//! `access_token`/`refresh_token` hold **ciphertext only** — the sync service seals them with
//! `tankovault_auth::SecretBox` before they reach this layer.

use crate::error::DbResult;
use sqlx::{FromRow, PgExecutor};
use tankovault_domain::UserId;
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
    /// Whether automatic sync (reactive push + scheduled reconciliation) is enabled for this
    /// account (design v2 §B.2). Seeded `true` on link.
    pub auto_sync_enabled: bool,
    /// The persisted per-account conflict policy token (design v2 §B.3): one of
    /// `local_wins` | `remote_wins` | `newest_wins` | `ask_me`. Seeded from the service
    /// default on link.
    pub conflict_policy: String,
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
    sqlx::query!(
        "INSERT INTO external_accounts \
            (user_id, provider, access_token, refresh_token, expires_at) \
         VALUES ($1,$2,$3,$4,$5) \
         ON CONFLICT (user_id, provider) DO UPDATE \
            SET access_token  = EXCLUDED.access_token, \
                refresh_token = EXCLUDED.refresh_token, \
                expires_at    = EXCLUDED.expires_at",
        user_id.as_uuid(),
        provider,
        access_token,
        refresh_token,
        expires_at,
    )
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
        auto_sync_enabled: bool,
        conflict_policy: String,
    }
    let row = sqlx::query_as!(
        Row,
        "SELECT user_id, provider, access_token, refresh_token, expires_at, \
                external_username, last_synced_at, last_error, \
                auto_sync_enabled, conflict_policy \
         FROM external_accounts WHERE user_id = $1 AND provider = $2",
        user_id.as_uuid(),
        provider,
    )
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
        auto_sync_enabled: r.auto_sync_enabled,
        conflict_policy: r.conflict_policy,
    }))
}

/// Update a linked account's automatic-sync policy (design v2 §B.2/§B.6). Either field may be
/// left `None` to keep its current value. Seeds are set at link time via `seed_account_policy`.
pub async fn update_account_settings<'e, E: PgExecutor<'e>>(
    exec: E,
    user_id: UserId,
    provider: &str,
    auto_sync_enabled: Option<bool>,
    conflict_policy: Option<&str>,
) -> DbResult<()> {
    sqlx::query!(
        "UPDATE external_accounts \
         SET auto_sync_enabled = COALESCE($3, auto_sync_enabled), \
             conflict_policy   = COALESCE($4, conflict_policy) \
         WHERE user_id = $1 AND provider = $2",
        user_id.as_uuid(),
        provider,
        auto_sync_enabled,
        conflict_policy,
    )
    .execute(exec)
    .await?;
    Ok(())
}

/// Seed the per-account conflict policy from the service default the first time an account is
/// linked, without disturbing an existing user choice (design v2 §B.1: the env default is only
/// the seed). No-op once the user has set anything.
pub async fn seed_account_policy<'e, E: PgExecutor<'e>>(
    exec: E,
    user_id: UserId,
    provider: &str,
    default_policy: &str,
) -> DbResult<()> {
    sqlx::query!(
        "UPDATE external_accounts SET conflict_policy = $3 \
         WHERE user_id = $1 AND provider = $2 AND conflict_policy = 'newest_wins'",
        user_id.as_uuid(),
        provider,
        default_policy,
    )
    .execute(exec)
    .await?;
    Ok(())
}

/// Every linked account with automatic sync enabled, as `(user_id, provider)` pairs, for the
/// scheduled reconciliation loop (design v2 §B.4).
pub async fn list_auto_sync_accounts<'e, E: PgExecutor<'e>>(
    exec: E,
) -> DbResult<Vec<(UserId, String)>> {
    #[derive(FromRow)]
    struct Row {
        user_id: Uuid,
        provider: String,
    }
    let rows = sqlx::query_as!(
        Row,
        "SELECT user_id, provider FROM external_accounts \
         WHERE auto_sync_enabled ORDER BY user_id, provider",
    )
    .fetch_all(exec)
    .await?;
    Ok(rows
        .into_iter()
        .map(|r| (UserId::from_uuid(r.user_id), r.provider))
        .collect())
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
    sqlx::query!(
        "UPDATE external_accounts \
         SET external_username = COALESCE($3, external_username), last_synced_at = $4, \
             last_error = NULL \
         WHERE user_id = $1 AND provider = $2",
        user_id.as_uuid(),
        provider,
        username,
        synced_at,
    )
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
    sqlx::query!(
        "UPDATE external_accounts SET last_error = $3 WHERE user_id = $1 AND provider = $2",
        user_id.as_uuid(),
        provider,
        error,
    )
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
    let result = sqlx::query!(
        "DELETE FROM external_accounts WHERE user_id = $1 AND provider = $2",
        user_id.as_uuid(),
        provider,
    )
    .execute(exec)
    .await?;
    Ok(result.rows_affected() > 0)
}
