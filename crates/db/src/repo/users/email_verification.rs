//! Email confirmation on sign-up (migration 0016).
//!
//! Structurally the twin of [`super::password_reset`] — hashed single-use token, a lookup that
//! does not judge, an atomic consume — with one addition: [`mark_email_verified`] is the write
//! that unblocks sign-in, and it is idempotent so replaying a confirmation link cannot reset
//! the instant the address was first confirmed.

use super::CiText;
use crate::error::DbResult;
use sqlx::{FromRow, PgExecutor};
use tankovault_domain::{AccountStatus, User, UserId};
use time::OffsetDateTime;
use uuid::Uuid;

/// Look up a user by email together with whether their address is already confirmed.
///
/// Used by the resend-confirmation flow, which must respond identically whether or not the
/// address exists (so it can't be used to enumerate accounts) yet still needs the verified
/// flag and the user's identity to compose a fresh confirmation email.
///
/// # Errors
/// [`crate::DbError::Sqlx`] only — no other variant is reachable. An unregistered address is
/// `Ok(None)` rather than [`crate::DbError::NotFound`], for the anti-enumeration reason above.
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

/// A stored email-verification token record (hash only; the plaintext lives only in the
/// email). Mirrors
/// [`PasswordResetRecord`](super::password_reset::PasswordResetRecord).
pub struct EmailVerificationRecord {
    pub id: Uuid,
    pub user_id: UserId,
    pub expires_at: OffsetDateTime,
    pub used_at: Option<OffsetDateTime>,
}

/// Persist a freshly issued email-verification token as its SHA-256 hash.
///
/// # Errors
/// [`crate::DbError::Sqlx`] only — no other variant is reachable. `token_hash` is `UNIQUE`;
/// a duplicate stays a driver error for the reason given on
/// [`super::refresh_tokens::insert_refresh`].
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
///
/// # Errors
/// [`crate::DbError::Sqlx`] only — no other variant is reachable. An unknown token is
/// `Ok(None)`; expiry and prior use are fields on the record, not errors.
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
///
/// # Errors
/// [`crate::DbError::Sqlx`] only — no other variant is reachable. Losing the race is `Ok(0)`,
/// not an error; the count is what enforces single use.
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
///
/// # Errors
/// [`crate::DbError::Sqlx`] only — no other variant is reachable. A `user_id` matching no row
/// updates nothing and still returns `Ok(())`, so this cannot report that it failed to unblock
/// sign-in for an account erased mid-flow.
pub async fn mark_email_verified<'e, E: PgExecutor<'e>>(exec: E, user_id: UserId) -> DbResult<()> {
    sqlx::query!(
        "UPDATE users SET email_verified_at = COALESCE(email_verified_at, now()) WHERE id = $1",
        user_id.as_uuid(),
    )
    .execute(exec)
    .await?;
    Ok(())
}

/// Whether a user's email address has been confirmed.
///
/// The one-column read for sign-in paths that already know *who* is signing in and so cannot
/// use [`find_by_email_with_verification`] or `find_credentials`, both of which resolve an
/// account from a typed identifier. Passkey sign-in is that case: the account arrives from the
/// authenticator, not from a form field, and the confirmation gate still has to apply — leaving
/// it off would make registering a passkey a way around email confirmation entirely.
///
/// # Errors
/// [`crate::DbError::Sqlx`] only — no other variant is reachable. A `user_id` matching no row is
/// `Ok(false)`, which is the safe answer for a gate: an account that does not exist has not
/// confirmed anything.
pub async fn is_email_verified<'e, E: PgExecutor<'e>>(exec: E, user_id: UserId) -> DbResult<bool> {
    let verified = sqlx::query_scalar!(
        "SELECT (email_verified_at IS NOT NULL) AS \"verified!\" FROM users WHERE id = $1",
        user_id.as_uuid(),
    )
    .fetch_optional(exec)
    .await?;
    Ok(verified.unwrap_or(false))
}
