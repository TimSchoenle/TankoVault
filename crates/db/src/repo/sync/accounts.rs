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
///
/// # Errors
/// [`crate::DbError::Sqlx`] only — no other variant is reachable. A `user_id` that no longer
/// exists is a foreign-key violation and so a 500, not [`crate::DbError::NotFound`]. Note what
/// the `DO UPDATE` list does *not* touch: `auto_sync_enabled`, `conflict_policy`,
/// `external_username`, `last_synced_at` and `last_error` survive a re-link, so refreshing a
/// token does not reset the user's settings.
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
///
/// # Errors
/// [`crate::DbError::Sqlx`] only — no other variant is reachable. An unlinked provider and an
/// unknown user are both `Ok(None)`, not [`crate::DbError::NotFound`] — "not linked" is the
/// state every account starts in and the status endpoint's ordinary answer. `Ok(None)` must not
/// be produced from a failure either: the sync engine reads it as "this user has no account
/// here" and skips the provider silently.
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
///
/// # Errors
/// [`crate::DbError::Sqlx`] only — no other variant is reachable. There is no
/// [`crate::DbError::NotFound`] for an account that is not linked: the `UPDATE` matches nothing
/// and this returns `Ok(())`, so a settings write against an unlinked provider reports success
/// and persists nothing. Callers that expose this to a user must establish the account exists
/// first — the same shape as `tracking::set_sync_excluded` (OPS-2.2d). `conflict_policy` is
/// stored verbatim; the closed vocabulary is enforced by `ConflictPolicy` at the edge, not here.
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
///
/// # Errors
/// [`crate::DbError::Sqlx`] only — no other variant is reachable. `Ok(())` whether it seeded, was
/// declined by the guard, or matched no account at all — the count is deliberately discarded,
/// because all three are the same outcome for the caller: the account now carries a policy it
/// did not choose to change. The guard is the column's own default value, so "the user has not
/// chosen" is inferred from `conflict_policy = 'newest_wins'` rather than recorded: a user who
/// deliberately picks `newest_wins` is indistinguishable from one who has never chosen, and a
/// later re-link re-seeds them to the service default.
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
///
/// # Errors
/// [`crate::DbError::Sqlx`] only — no other variant is reachable. A deployment where nobody has
/// linked an account is an empty `Vec`, which the scheduler treats as "nothing to do" — so this
/// is one to propagate rather than default: an empty list produced by a failure would stop
/// automatic sync for everyone while every tick still reported success.
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
///
/// # Errors
/// [`crate::DbError::Sqlx`] only — no other variant is reachable. An account that has since been
/// unlinked matches nothing and is `Ok(())`, not [`crate::DbError::NotFound`]: a sync that
/// finished after the user disconnected has nothing left to stamp, and failing it would turn a
/// completed sync into a reported error. Clearing `last_error` is part of this statement rather
/// than a separate call, so a success cannot record its timestamp while leaving the previous
/// failure on display.
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
///
/// # Errors
/// [`crate::DbError::Sqlx`] only — no other variant is reachable. An unlinked account is `Ok(())`
/// with nothing written. This is on the failure path already, so callers log rather than
/// propagate: losing the record of *why* a sync failed must not replace the failure itself.
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
///
/// # Errors
/// [`crate::DbError::Sqlx`] only — no other variant is reachable. An account that was not linked
/// is `Ok(false)`, not [`crate::DbError::NotFound`]; the desired end state — no stored tokens for
/// this provider — holds either way, which is why unlink is safe to retry. Never default a
/// failure to `true`: this is what removes a user's OAuth ciphertext, and reporting a
/// disconnection that did not happen leaves live tokens behind under a UI that says otherwise.
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
