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
