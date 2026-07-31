//! The forgot-password lifecycle (migration 0015) and the password column it writes.
//!
//! A reset token is stored as a SHA-256 hash; the plaintext exists only in the email. The
//! lookup answers "is this token known" and nothing else, so the handler can respond
//! identically for an unknown, an expired and an already-consumed token — which is what stops
//! the endpoint disclosing whether a reset was ever requested. Single-use is enforced by
//! [`consume_password_reset`]'s atomic `used_at` flip, not by the read.

use super::CiText;
use super::credentials::UserRow;
use crate::error::{DbError, DbResult};
use sqlx::{FromRow, PgExecutor};
use tankovault_domain::{AccountStatus, User, UserId};
use time::OffsetDateTime;
use uuid::Uuid;

/// Look up a user by email address (the forgot-password entry point).
///
/// Returns `None` when no account matches, letting the handler respond identically whether
/// or not the address is registered (avoids account enumeration).
///
/// That silence is exactly why the [`CiText`] binding matters here: a case-sensitive match
/// answers `None` for an address that *is* registered, and the endpoint is designed to say
/// nothing about the difference — so the user is simply never sent the reset mail and is told
/// that it was sent.
///
/// # Errors
/// [`DbError::Sqlx`] only — no other variant is reachable. An unregistered address is
/// `Ok(None)`; returning [`DbError::NotFound`] here would hand the caller a distinguishable
/// response and undo the anti-enumeration property described above.
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
/// [`DbError::Sqlx`] only — no other variant is reachable. `token_hash` is `UNIQUE`, but a
/// duplicate stays a driver error rather than [`DbError::Conflict`] for the reason given on
/// [`super::refresh_tokens::insert_refresh`]: the value is server-generated randomness, so a
/// collision is a generator fault and belongs in a 500.
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
///
/// # Errors
/// [`DbError::Sqlx`] only — no other variant is reachable. An unknown token is `Ok(None)`, and
/// expiry and prior use are fields on the returned record rather than errors, precisely so the
/// three cases are the caller's to collapse into one identical response.
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
///
/// # Errors
/// [`DbError::Sqlx`] only — no other variant is reachable. Losing the race is `Ok(0)`, not an
/// error: the `used_at IS NULL` predicate is what makes the token single-use, so the count is
/// the security-relevant answer and must not be discarded.
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
/// [`DbError::NotFound`] — a 404 — when `id` matches no row. The check is not decoration: a
/// reset that reported success while storing nothing would leave the old password live and
/// the user certain it had changed. Otherwise [`DbError::Sqlx`].
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
