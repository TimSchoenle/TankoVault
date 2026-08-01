//! The forgot-password lifecycle and the password column it writes. A reset token is stored as
//! a SHA-256 hash; single-use is enforced by [`consume_password_reset`]'s atomic `used_at` flip.

use super::CiText;
use super::credentials::UserRow;
use crate::error::{DbError, DbResult};
use sqlx::{FromRow, PgExecutor};
use tankovault_domain::{AccountStatus, User, UserId};
use time::OffsetDateTime;
use uuid::Uuid;

/// Look up a user by email address (the forgot-password entry point). `None` lets the handler
/// respond identically whether or not the address is registered.
///
/// # Errors
/// [`DbError::Sqlx`] only. An unregistered address is `Ok(None)`, not [`DbError::NotFound`] —
/// preserves the anti-enumeration response.
pub async fn find_by_email<'e, E: PgExecutor<'e>>(exec: E, email: &str) -> DbResult<Option<User>> {
    let row = sqlx::query_as!(
        UserRow,
        "SELECT id, email, username, status AS \"status: AccountStatus\", created_at \
         FROM users WHERE email = $1",
        CiText(email) as _,
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
///
/// # Errors
/// [`DbError::Sqlx`] only. A duplicate `token_hash` is a generator fault (500), not a
/// client-triggerable [`DbError::Conflict`] (409).
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

/// Find a password-reset token by its hash, regardless of expiry/use.
///
/// # Errors
/// [`DbError::Sqlx`] only. An unknown token is `Ok(None)`; expiry/use are record fields.
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

/// Atomically mark a reset token consumed (single-use). `0` rows means already used.
///
/// # Errors
/// [`DbError::Sqlx`] only. Losing the race is `Ok(0)`, not an error.
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
///
/// # Errors
/// [`DbError::NotFound`] — a 404 — when `id` matches no row, so a reset never reports success
/// while leaving the old password live. Otherwise [`DbError::Sqlx`].
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
