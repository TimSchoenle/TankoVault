//! Self-service account settings (frontend §9.4): the identity a user may change about
//! themselves, plus their notification preferences.
//!
//! [`update_profile`] carries SEC-4's rule that a *changed* email address loses its
//! verification. The sessions half of the old "account settings" section lives in
//! [`super::sessions`].

use super::CiText;
use super::credentials::UserRow;
use crate::error::{DbError, DbResult};
use sqlx::PgExecutor;
use tankovault_domain::{AccountStatus, User, UserId};

/// Apply a username and/or email change.
///
/// A **changed** email clears `email_verified_at`, so the new address inherits nothing from
/// the old one. Previously it did: an attacker holding a 15-minute access token could point
/// the account at their own address, which arrived already "verified", then drive a password
/// reset to it and lock the owner out of an account whose recovery address they no longer
/// controlled. `COALESCE` on the same-value case keeps a no-op PATCH from forcing a
/// re-verification for nothing.
///
/// "Same value" is decided by `$3 <> email`, which is a comparison and therefore needs the
/// [`CiText`] binding: bound as `text`, re-capitalising your own address counted as moving to
/// a new one and mailed you a confirmation link for the mailbox you were already using.
///
/// # Errors
/// [`DbError::Conflict`] if the new email or username is already registered — the unique
/// violation is translated here so the API answers 409 rather than 500.
///
/// An `id` that matches no row is **not** [`DbError::NotFound`]: this is a `fetch_one`, so it
/// arrives as the driver's `RowNotFound` inside [`DbError::Sqlx`] and the API maps it to 500.
/// That is tolerable only because every caller holds an authenticated id, so the case means
/// the account was erased mid-request. Note the asymmetry with [`set_notification_prefs`],
/// which does return [`DbError::NotFound`] for the same condition.
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
        username.map(CiText) as _,
        email.map(CiText) as _,
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

/// Read a user's notification preferences JSON (frontend §9.4). `{}` means "defaults".
///
/// # Errors
/// [`DbError::Sqlx`] only — no other variant is reachable. An unknown `id` yields `{}`, the
/// same answer as a user who has never set a preference, rather than [`DbError::NotFound`];
/// the reader has nothing to do differently in the two cases.
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
///
/// # Errors
/// [`DbError::NotFound`] — a 404 — when `id` matches no row, because a write that silently
/// stored nothing is worse than a rejection. Otherwise [`DbError::Sqlx`].
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
