//! Email confirmation on sign-up: a hashed single-use token, mirroring
//! [`super::password_reset`]'s shape.

use super::CiText;
use crate::error::DbResult;
use sqlx::{FromRow, PgExecutor};
use tankovault_domain::{AccountStatus, User, UserId};
use time::OffsetDateTime;
use uuid::Uuid;

/// Looks up a user by email together with whether their address is already confirmed, for the
/// resend-confirmation flow.
///
/// An unregistered address is `None`, which avoids leaking account existence.
///
/// # Errors
/// [`crate::DbError::Sqlx`] only.
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
        CiText(email) as _,
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

/// A stored email-verification token record (hash only). Mirrors
/// [`PasswordResetRecord`](super::password_reset::PasswordResetRecord).
pub struct EmailVerificationRecord {
    /// Primary key of the token row, not the token.
    pub id: Uuid,
    /// The account the token confirms.
    pub user_id: UserId,
    /// After this instant the token is refused. Expiry is checked on use, not swept.
    pub expires_at: OffsetDateTime,
    /// When it was redeemed. `Some` makes it single-use: a replay is refused.
    pub used_at: Option<OffsetDateTime>,
}

/// Persist a freshly issued email-verification token as its SHA-256 hash.
///
/// A duplicate `token_hash` is a generator fault (500), not a client-triggerable
/// [`crate::DbError::Conflict`] (409).
///
/// # Errors
/// [`crate::DbError::Sqlx`] only.
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

/// Finds an email-verification token by its hash, regardless of expiry/use.
///
/// Expiry and use are record fields for the caller to check.
///
/// # Errors
/// [`crate::DbError::Sqlx`] only.
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

/// Atomically mark a verification token consumed (single-use). `0` rows means already used.
///
/// `0` means another request won the race.
///
/// # Errors
/// [`crate::DbError::Sqlx`] only.
pub async fn consume_email_verification<'e, E: PgExecutor<'e>>(exec: E, id: Uuid) -> DbResult<u64> {
    let result = sqlx::query!(
        "UPDATE email_verification_tokens SET used_at = now() WHERE id = $1 AND used_at IS NULL",
        id,
    )
    .execute(exec)
    .await?;
    Ok(result.rows_affected())
}

/// Marks a user's email address confirmed. Idempotent: re-confirming leaves the original timestamp
/// untouched so the "verified since" instant stays stable.
///
/// # Errors
/// [`crate::DbError::Sqlx`] only.
pub async fn mark_email_verified<'e, E: PgExecutor<'e>>(exec: E, user_id: UserId) -> DbResult<()> {
    sqlx::query!(
        "UPDATE users SET email_verified_at = COALESCE(email_verified_at, now()) WHERE id = $1",
        user_id.as_uuid(),
    )
    .execute(exec)
    .await?;
    Ok(())
}

/// Returns whether a user's email address has been confirmed.
///
/// The one-column read for sign-in paths, such as passkeys, that already know who is signing in.
/// An unknown `user_id` is `false`, the safe default for a gate.
///
/// # Errors
/// [`crate::DbError::Sqlx`] only.
pub async fn is_email_verified<'e, E: PgExecutor<'e>>(exec: E, user_id: UserId) -> DbResult<bool> {
    let verified = sqlx::query_scalar!(
        "SELECT (email_verified_at IS NOT NULL) AS \"verified!\" FROM users WHERE id = $1",
        user_id.as_uuid(),
    )
    .fetch_optional(exec)
    .await?;
    Ok(verified.unwrap_or(false))
}
