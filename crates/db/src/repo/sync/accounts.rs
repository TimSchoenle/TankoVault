//! Linked provider accounts and their automatic-sync settings.
//!
//! `access_token` and `refresh_token` hold **ciphertext only**: the sync service seals them with
//! `tankovault_auth::Sealer` before they reach this layer.

use crate::error::DbResult;
use sqlx::{FromRow, PgExecutor};
use tankovault_domain::UserId;
use time::OffsetDateTime;
use uuid::Uuid;

/// A stored external-provider account. `access_token`/`refresh_token` are AES-GCM
/// ciphertext (see module docs); callers decrypt with the sync service's data key.
#[derive(Debug, Clone)]
pub struct ExternalAccount {
    /// The local account the link belongs to.
    pub user_id: UserId,
    /// Which external tracker, as a slug.
    pub provider: String,
    /// AES-GCM ciphertext. Decrypt with the sync service's data key; never logged.
    pub access_token: Vec<u8>,
    /// AES-GCM ciphertext, `None` for a provider that issues no refresh token.
    pub refresh_token: Option<Vec<u8>>,
    /// When the access token stops working, `None` when the provider states no expiry.
    pub expires_at: Option<OffsetDateTime>,
    /// Display name for the linked account, kept current so the UI can show "Connected as X".
    pub external_username: Option<String>,
    /// When this account last completed a pull or push.
    pub last_synced_at: Option<OffsetDateTime>,
    /// Most recent sync failure, if any; cleared on next success. Admin-visible only.
    pub last_error: Option<String>,
    /// Whether automatic sync is enabled (design v2 §B.2); seeded `true` on link.
    pub auto_sync_enabled: bool,
    /// Conflict policy token (design v2 §B.3): `local_wins`/`remote_wins`/`newest_wins`/`ask_me`.
    pub conflict_policy: String,
}

/// Inserts or replaces a user's account for `provider`. Idempotent on `(user_id, provider)`, so
/// re-linking (e.g. a token refresh) overwrites the prior ciphertext in place.
///
/// Re-linking leaves `auto_sync_enabled`, `conflict_policy` and other settings columns untouched.
///
/// # Errors
/// [`crate::DbError::Sqlx`] only.
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

/// Fetches a user's account for `provider`, if linked.
///
/// Never default a failure to `None`: the engine reads it as "skip this provider".
///
/// # Errors
/// [`crate::DbError::Sqlx`] only.
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

/// Updates a linked account's automatic-sync policy (design v2 §B.2/§B.6). `None` leaves a field
/// unchanged.
///
/// An unlinked provider matches nothing silently, so callers confirm the account exists first.
///
/// # Errors
/// [`crate::DbError::Sqlx`] only.
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

/// Seeds the per-account conflict policy from the service default, on first link only.
///
/// Does nothing once the user has chosen one. "Not yet chosen" is inferred from the column still
/// reading `newest_wins`, so a user who deliberately picks that value is seeded again if they
/// re-link (design v2 §B.1). Seeded, declined and no such account are the same outcome.
///
/// # Errors
/// [`crate::DbError::Sqlx`] only.
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

/// Returns every linked account with automatic sync enabled, as `(user_id, provider)` pairs, for
/// the scheduled reconciliation loop (design v2 §B.4).
///
/// Always propagate the error: an empty `Vec` from a failure would silently stop sync for
/// everyone.
///
/// # Errors
/// [`crate::DbError::Sqlx`] only.
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

/// Records a fresh sync timestamp and, if known, the provider's display name; clears `last_error`
/// in the same statement. A `None` username leaves the stored one as-is.
///
/// # Errors
/// [`crate::DbError::Sqlx`] only.
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

/// Records a sync failure for a linked account (admin Sync console tab). Overwritten by the next
/// successful `mark_synced`, which clears it back to `NULL`.
///
/// Callers on this already-failing path log the error instead of propagating it, so the original
/// failure is not lost.
///
/// # Errors
/// [`crate::DbError::Sqlx`] only.
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

/// Unlinks a user's account for `provider`, returning `true` if a row was removed.
///
/// Never default a failure to `true`: this removes OAuth ciphertext, and a false "disconnected"
/// leaves live tokens behind.
///
/// # Errors
/// [`crate::DbError::Sqlx`] only.
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
