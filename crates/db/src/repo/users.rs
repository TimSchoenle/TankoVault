//! User accounts and rotating refresh tokens.
//!
//! The `auth` crate owns hashing; this layer stores only the argon2id `password_hash`
//! and the SHA-256 `token_hash`, never plaintext secrets.
//!
//! What a user is *allowed* to do is not here: permission grants live in
//! [`crate::repo::permissions`] and the operator-facing administration of accounts lives in
//! [`crate::repo::user_admin`]. This module is identity and session plumbing only.

use crate::error::{DbError, DbResult};
use sqlx::{FromRow, PgExecutor};
use tankovault_domain::{AccountStatus, User, UserId};
use time::OffsetDateTime;
use uuid::Uuid;

#[derive(FromRow)]
struct UserRow {
    id: Uuid,
    email: String,
    username: String,
    status: AccountStatus,
    created_at: OffsetDateTime,
}

impl From<UserRow> for User {
    fn from(r: UserRow) -> Self {
        Self {
            id: UserId::from_uuid(r.id),
            email: r.email,
            username: r.username,
            status: r.status,
            created_at: r.created_at,
        }
    }
}

/// A user together with the stored password hash (login verification only).
pub struct Credentials {
    pub user: User,
    pub password_hash: String,
    /// Whether the account's email address has been confirmed. `false` blocks sign-in until
    /// the user clicks the emailed confirmation link (see `email_verification_tokens`).
    pub email_verified: bool,
}

/// Create a user. `password_hash` is a pre-computed argon2id PHC string.
///
/// The account is created with **no permissions**, which is the only safe default: a
/// registration endpoint must not be able to mint privilege. Grants are added afterwards by
/// someone holding [`Permission::UsersPermissions`](tankovault_domain::Permission), via
/// [`crate::repo::permissions::replace`].
///
/// # Errors
/// [`DbError::Conflict`] if the email or username is taken.
pub async fn create<'e, E: PgExecutor<'e>>(
    exec: E,
    email: &str,
    username: &str,
    password_hash: &str,
) -> DbResult<User> {
    let id = UserId::new();
    let row = sqlx::query_as!(
        UserRow,
        "INSERT INTO users (id, email, username, password_hash) \
         VALUES ($1,$2,$3,$4) \
         RETURNING id, email, username, status AS \"status: AccountStatus\", created_at",
        id.as_uuid(),
        email,
        username,
        password_hash,
    )
    .fetch_one(exec)
    .await
    .map_err(|e| {
        let de = DbError::from(e);
        if de.is_unique_violation() {
            DbError::Conflict("email or username already registered".to_owned())
        } else {
            de
        }
    })?;
    Ok(row.into())
}

/// Look up credentials by email or username (login accepts either).
pub async fn find_credentials<'e, E: PgExecutor<'e>>(
    exec: E,
    login: &str,
) -> DbResult<Option<Credentials>> {
    #[derive(FromRow)]
    struct Row {
        id: Uuid,
        email: String,
        username: String,
        status: AccountStatus,
        created_at: OffsetDateTime,
        password_hash: String,
        email_verified: bool,
    }
    let row = sqlx::query_as!(
        Row,
        "SELECT id, email, username, status AS \"status: AccountStatus\", created_at, \
                password_hash, (email_verified_at IS NOT NULL) AS \"email_verified!\" \
         FROM users WHERE email = $1 OR username = $1",
        login,
    )
    .fetch_optional(exec)
    .await?;

    Ok(row.map(|r| Credentials {
        user: User {
            id: UserId::from_uuid(r.id),
            email: r.email,
            username: r.username,
            status: r.status,
            created_at: r.created_at,
        },
        password_hash: r.password_hash,
        email_verified: r.email_verified,
    }))
}

/// Fetch a user by id.
pub async fn get<'e, E: PgExecutor<'e>>(exec: E, id: UserId) -> DbResult<User> {
    let row = sqlx::query_as!(
        UserRow,
        "SELECT id, email, username, status AS \"status: AccountStatus\", created_at \
         FROM users WHERE id = $1",
        id.as_uuid(),
    )
    .fetch_optional(exec)
    .await?;
    Ok(row.ok_or(DbError::NotFound)?.into())
}

/// Record a successful sign-in.
///
/// Separate from the credential lookup so a *failed* attempt cannot advance the timestamp:
/// "last login" that moves on every guess would be worse than not having it, both for the
/// operator reading the directory and for a user checking their own account.
pub async fn touch_last_login<'e, E: PgExecutor<'e>>(exec: E, id: UserId) -> DbResult<()> {
    sqlx::query!(
        "UPDATE users SET last_login_at = now() WHERE id = $1",
        id.as_uuid(),
    )
    .execute(exec)
    .await?;
    Ok(())
}

/// The account state the authorization layer needs on every authenticated request.
///
/// Fetched with the permission grants in one round trip (see
/// [`crate::repo::permissions::resolve`]) because both are needed together and neither is
/// useful alone: a grant set without the status would authorize a suspended account.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AccountState {
    pub status: AccountStatus,
}

/// Read just the authorization-relevant account state.
pub async fn account_state<'e, E: PgExecutor<'e>>(
    exec: E,
    id: UserId,
) -> DbResult<Option<AccountState>> {
    let status = sqlx::query_scalar!(
        "SELECT status AS \"status: AccountStatus\" FROM users WHERE id = $1",
        id.as_uuid(),
    )
    .fetch_optional(exec)
    .await?;
    Ok(status.map(|status| AccountState { status }))
}

// ---------------------------------------------------------------------------
// Refresh tokens (rotation + reuse detection)
// ---------------------------------------------------------------------------

/// A stored refresh-token record (hash only).
pub struct RefreshRecord {
    pub id: Uuid,
    pub user_id: UserId,
    pub family_id: Uuid,
    pub expires_at: OffsetDateTime,
    pub revoked_at: Option<OffsetDateTime>,
}

/// Persist a freshly issued refresh token (as its SHA-256 hash).
pub async fn insert_refresh<'e, E: PgExecutor<'e>>(
    exec: E,
    user_id: UserId,
    family_id: Uuid,
    token_hash: &str,
    expires_at: OffsetDateTime,
) -> DbResult<()> {
    sqlx::query!(
        "INSERT INTO refresh_tokens (id, user_id, family_id, token_hash, expires_at) \
         VALUES ($1,$2,$3,$4,$5)",
        Uuid::now_v7(),
        user_id.as_uuid(),
        family_id,
        token_hash,
        expires_at,
    )
    .execute(exec)
    .await?;
    Ok(())
}

/// Find a refresh token by its hash (regardless of revocation, for reuse detection).
pub async fn find_refresh<'e, E: PgExecutor<'e>>(
    exec: E,
    token_hash: &str,
) -> DbResult<Option<RefreshRecord>> {
    #[derive(FromRow)]
    struct Row {
        id: Uuid,
        user_id: Uuid,
        family_id: Uuid,
        expires_at: OffsetDateTime,
        revoked_at: Option<OffsetDateTime>,
    }
    let row = sqlx::query_as!(
        Row,
        "SELECT id, user_id, family_id, expires_at, revoked_at FROM refresh_tokens \
         WHERE token_hash = $1",
        token_hash,
    )
    .fetch_optional(exec)
    .await?;
    Ok(row.map(|r| RefreshRecord {
        id: r.id,
        user_id: UserId::from_uuid(r.user_id),
        family_id: r.family_id,
        expires_at: r.expires_at,
        revoked_at: r.revoked_at,
    }))
}

/// Revoke a single token by id (normal rotation).
pub async fn revoke_token<'e, E: PgExecutor<'e>>(exec: E, id: Uuid) -> DbResult<()> {
    sqlx::query!(
        "UPDATE refresh_tokens SET revoked_at = now() WHERE id = $1 AND revoked_at IS NULL",
        id,
    )
    .execute(exec)
    .await?;
    Ok(())
}

/// Revoke an entire token family (reuse detected → invalidate the lineage).
pub async fn revoke_family<'e, E: PgExecutor<'e>>(exec: E, family_id: Uuid) -> DbResult<()> {
    sqlx::query!(
        "UPDATE refresh_tokens SET revoked_at = now() WHERE family_id = $1 AND revoked_at IS NULL",
        family_id,
    )
    .execute(exec)
    .await?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Password reset (migration 0015)
// ---------------------------------------------------------------------------

/// Look up a user by email address (the forgot-password entry point).
///
/// Returns `None` when no account matches, letting the handler respond identically whether
/// or not the address is registered (avoids account enumeration).
pub async fn find_by_email<'e, E: PgExecutor<'e>>(exec: E, email: &str) -> DbResult<Option<User>> {
    let row = sqlx::query_as!(
        UserRow,
        "SELECT id, email, username, status AS \"status: AccountStatus\", created_at \
         FROM users WHERE email = $1",
        email,
    )
    .fetch_optional(exec)
    .await?;
    Ok(row.map(Into::into))
}

/// A stored password-reset token record (hash only; the plaintext lives only in the email).
pub struct PasswordResetRecord {
    pub id: Uuid,
    pub user_id: UserId,
    pub expires_at: OffsetDateTime,
    pub used_at: Option<OffsetDateTime>,
}

/// Persist a freshly issued password-reset token as its SHA-256 hash.
pub async fn insert_password_reset<'e, E: PgExecutor<'e>>(
    exec: E,
    user_id: UserId,
    token_hash: &str,
    expires_at: OffsetDateTime,
) -> DbResult<()> {
    sqlx::query!(
        "INSERT INTO password_reset_tokens (id, user_id, token_hash, expires_at) \
         VALUES ($1,$2,$3,$4)",
        Uuid::now_v7(),
        user_id.as_uuid(),
        token_hash,
        expires_at,
    )
    .execute(exec)
    .await?;
    Ok(())
}

/// Find a password-reset token by its hash, regardless of expiry/use, so the caller can
/// distinguish an unknown token from an expired or already-consumed one.
pub async fn find_password_reset<'e, E: PgExecutor<'e>>(
    exec: E,
    token_hash: &str,
) -> DbResult<Option<PasswordResetRecord>> {
    #[derive(FromRow)]
    struct Row {
        id: Uuid,
        user_id: Uuid,
        expires_at: OffsetDateTime,
        used_at: Option<OffsetDateTime>,
    }
    let row = sqlx::query_as!(
        Row,
        "SELECT id, user_id, expires_at, used_at FROM password_reset_tokens \
         WHERE token_hash = $1",
        token_hash,
    )
    .fetch_optional(exec)
    .await?;
    Ok(row.map(|r| PasswordResetRecord {
        id: r.id,
        user_id: UserId::from_uuid(r.user_id),
        expires_at: r.expires_at,
        used_at: r.used_at,
    }))
}

/// Atomically mark a reset token consumed (single-use). Returns the number of rows updated:
/// `0` means it was already used, which the caller must treat as a failed reset.
pub async fn consume_password_reset<'e, E: PgExecutor<'e>>(exec: E, id: Uuid) -> DbResult<u64> {
    let result = sqlx::query!(
        "UPDATE password_reset_tokens SET used_at = now() WHERE id = $1 AND used_at IS NULL",
        id,
    )
    .execute(exec)
    .await?;
    Ok(result.rows_affected())
}

/// Replace a user's password hash (pre-computed argon2id PHC string).
pub async fn update_password<'e, E: PgExecutor<'e>>(
    exec: E,
    id: UserId,
    password_hash: &str,
) -> DbResult<()> {
    let result = sqlx::query!(
        "UPDATE users SET password_hash = $2 WHERE id = $1",
        id.as_uuid(),
        password_hash,
    )
    .execute(exec)
    .await?;
    if result.rows_affected() == 0 {
        return Err(DbError::NotFound);
    }
    Ok(())
}

/// Revoke every live refresh token for a user — used after a password reset so any stolen
/// session is invalidated along with the changed credential.
pub async fn revoke_all_for_user<'e, E: PgExecutor<'e>>(exec: E, user_id: UserId) -> DbResult<()> {
    sqlx::query!(
        "UPDATE refresh_tokens SET revoked_at = now() WHERE user_id = $1 AND revoked_at IS NULL",
        user_id.as_uuid(),
    )
    .execute(exec)
    .await?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Email verification (migration 0016)
// ---------------------------------------------------------------------------

/// Look up a user by email together with whether their address is already confirmed.
///
/// Used by the resend-confirmation flow, which must respond identically whether or not the
/// address exists (so it can't be used to enumerate accounts) yet still needs the verified
/// flag and the user's identity to compose a fresh confirmation email.
pub async fn find_by_email_with_verification<'e, E: PgExecutor<'e>>(
    exec: E,
    email: &str,
) -> DbResult<Option<(User, bool)>> {
    #[derive(FromRow)]
    struct Row {
        id: Uuid,
        email: String,
        username: String,
        status: AccountStatus,
        created_at: OffsetDateTime,
        email_verified: bool,
    }
    let row = sqlx::query_as!(
        Row,
        "SELECT id, email, username, status AS \"status: AccountStatus\", created_at, \
                (email_verified_at IS NOT NULL) AS \"email_verified!\" \
         FROM users WHERE email = $1",
        email,
    )
    .fetch_optional(exec)
    .await?;
    Ok(row.map(|r| {
        (
            User {
                id: UserId::from_uuid(r.id),
                email: r.email,
                username: r.username,
                status: r.status,
                created_at: r.created_at,
            },
            r.email_verified,
        )
    }))
}

/// A stored email-verification token record (hash only; the plaintext lives only in the
/// email). Mirrors [`PasswordResetRecord`].
pub struct EmailVerificationRecord {
    pub id: Uuid,
    pub user_id: UserId,
    pub expires_at: OffsetDateTime,
    pub used_at: Option<OffsetDateTime>,
}

/// Persist a freshly issued email-verification token as its SHA-256 hash.
pub async fn insert_email_verification<'e, E: PgExecutor<'e>>(
    exec: E,
    user_id: UserId,
    token_hash: &str,
    expires_at: OffsetDateTime,
) -> DbResult<()> {
    sqlx::query!(
        "INSERT INTO email_verification_tokens (id, user_id, token_hash, expires_at) \
         VALUES ($1,$2,$3,$4)",
        Uuid::now_v7(),
        user_id.as_uuid(),
        token_hash,
        expires_at,
    )
    .execute(exec)
    .await?;
    Ok(())
}

/// Find an email-verification token by its hash, regardless of expiry/use, so the caller can
/// distinguish an unknown token from an expired or already-consumed one.
pub async fn find_email_verification<'e, E: PgExecutor<'e>>(
    exec: E,
    token_hash: &str,
) -> DbResult<Option<EmailVerificationRecord>> {
    #[derive(FromRow)]
    struct Row {
        id: Uuid,
        user_id: Uuid,
        expires_at: OffsetDateTime,
        used_at: Option<OffsetDateTime>,
    }
    let row = sqlx::query_as!(
        Row,
        "SELECT id, user_id, expires_at, used_at FROM email_verification_tokens \
         WHERE token_hash = $1",
        token_hash,
    )
    .fetch_optional(exec)
    .await?;
    Ok(row.map(|r| EmailVerificationRecord {
        id: r.id,
        user_id: UserId::from_uuid(r.user_id),
        expires_at: r.expires_at,
        used_at: r.used_at,
    }))
}

/// Atomically mark a verification token consumed (single-use). Returns the number of rows
/// updated: `0` means it was already used, which the caller must treat as a failed attempt.
pub async fn consume_email_verification<'e, E: PgExecutor<'e>>(exec: E, id: Uuid) -> DbResult<u64> {
    let result = sqlx::query!(
        "UPDATE email_verification_tokens SET used_at = now() WHERE id = $1 AND used_at IS NULL",
        id,
    )
    .execute(exec)
    .await?;
    Ok(result.rows_affected())
}

/// Mark a user's email address confirmed. Idempotent: re-confirming leaves the original
/// timestamp untouched so the "verified since" instant stays stable.
pub async fn mark_email_verified<'e, E: PgExecutor<'e>>(exec: E, user_id: UserId) -> DbResult<()> {
    sqlx::query!(
        "UPDATE users SET email_verified_at = COALESCE(email_verified_at, now()) WHERE id = $1",
        user_id.as_uuid(),
    )
    .execute(exec)
    .await?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Account settings (frontend §9.4)
// ---------------------------------------------------------------------------

/// Apply a username and/or email change.
///
/// A **changed** email clears `email_verified_at`, so the new address inherits nothing from
/// the old one. Previously it did: an attacker holding a 15-minute access token could point
/// the account at their own address, which arrived already "verified", then drive a password
/// reset to it and lock the owner out of an account whose recovery address they no longer
/// controlled. `COALESCE` on the same-value case keeps a no-op PATCH from forcing a
/// re-verification for nothing.
pub async fn update_profile<'e, E: PgExecutor<'e>>(
    exec: E,
    id: UserId,
    username: Option<&str>,
    email: Option<&str>,
) -> DbResult<User> {
    let row = sqlx::query_as!(
        UserRow,
        "UPDATE users SET \
            username = COALESCE($2, username), \
            email = COALESCE($3, email), \
            email_verified_at = CASE \
                WHEN $3 IS NOT NULL AND $3 <> email THEN NULL \
                ELSE email_verified_at \
            END \
         WHERE id = $1 \
         RETURNING id, email, username, status AS \"status: AccountStatus\", created_at",
        id.as_uuid(),
        username,
        email,
    )
    .fetch_one(exec)
    .await
    .map_err(|e| {
        let de = DbError::from(e);
        if de.is_unique_violation() {
            DbError::Conflict("email or username already registered".to_owned())
        } else {
            de
        }
    })?;
    Ok(row.into())
}

/// An active login session, derived from a live (unrevoked, unexpired) refresh token
/// (frontend §9.4 `GET /v1/me/sessions`). Only non-secret metadata is exposed.
#[derive(Debug, Clone)]
pub struct SessionInfo {
    pub id: Uuid,
    pub family_id: Uuid,
    pub created_at: OffsetDateTime,
    pub expires_at: OffsetDateTime,
}

/// List a user's active sessions (live refresh tokens), newest first.
pub async fn list_sessions<'e, E: PgExecutor<'e>>(
    exec: E,
    user_id: UserId,
) -> DbResult<Vec<SessionInfo>> {
    #[derive(FromRow)]
    struct Row {
        id: Uuid,
        family_id: Uuid,
        created_at: OffsetDateTime,
        expires_at: OffsetDateTime,
    }
    let rows = sqlx::query_as!(
        Row,
        "SELECT id, family_id, created_at, expires_at FROM refresh_tokens \
         WHERE user_id = $1 AND revoked_at IS NULL AND expires_at > now() \
         ORDER BY created_at DESC",
        user_id.as_uuid(),
    )
    .fetch_all(exec)
    .await?;
    Ok(rows
        .into_iter()
        .map(|r| SessionInfo {
            id: r.id,
            family_id: r.family_id,
            created_at: r.created_at,
            expires_at: r.expires_at,
        })
        .collect())
}

/// Revoke one of the user's own sessions (its whole rotation family), scoped to ownership.
/// Returns the number of tokens revoked (0 if the session id was not the caller's).
pub async fn revoke_session<'e, E: PgExecutor<'e>>(
    exec: E,
    user_id: UserId,
    session_id: Uuid,
) -> DbResult<u64> {
    let result = sqlx::query!(
        "UPDATE refresh_tokens SET revoked_at = now() \
         WHERE revoked_at IS NULL AND family_id = ( \
             SELECT family_id FROM refresh_tokens WHERE id = $2 AND user_id = $1 \
         )",
        user_id.as_uuid(),
        session_id,
    )
    .execute(exec)
    .await?;
    Ok(result.rows_affected())
}

/// Read a user's notification preferences JSON (frontend §9.4). `{}` means "defaults".
pub async fn get_notification_prefs<'e, E: PgExecutor<'e>>(
    exec: E,
    id: UserId,
) -> DbResult<serde_json::Value> {
    let prefs = sqlx::query_scalar!(
        "SELECT notification_prefs AS \"notification_prefs: serde_json::Value\" FROM users WHERE id = $1",
        id.as_uuid(),
    )
    .fetch_optional(exec)
    .await?;
    Ok(prefs.unwrap_or_else(|| serde_json::json!({})))
}

/// Replace a user's notification preferences JSON (frontend §9.4).
pub async fn set_notification_prefs<'e, E: PgExecutor<'e>>(
    exec: E,
    id: UserId,
    prefs: &serde_json::Value,
) -> DbResult<()> {
    let result = sqlx::query!(
        "UPDATE users SET notification_prefs = $2 WHERE id = $1",
        id.as_uuid(),
        prefs,
    )
    .execute(exec)
    .await?;
    if result.rows_affected() == 0 {
        return Err(DbError::NotFound);
    }
    Ok(())
}

/// Revoke **every** live session for a user, returning how many tokens were invalidated.
///
/// Distinct from [`revoke_all_for_user`], which exists for the password-reset path and
/// discards the count: an operator forcing a sign-out needs to be told whether there was
/// anything to sign out of.
pub async fn revoke_all_sessions<'e, E: PgExecutor<'e>>(exec: E, user_id: UserId) -> DbResult<u64> {
    let result = sqlx::query!(
        "UPDATE refresh_tokens SET revoked_at = now() \
         WHERE user_id = $1 AND revoked_at IS NULL",
        user_id.as_uuid(),
    )
    .execute(exec)
    .await?;
    Ok(result.rows_affected())
}
