//! Self-service account settings: the identity a user may change about themselves, plus their
//! notification preferences. Session management lives in [`super::sessions`].

use super::CiText;
use super::credentials::UserRow;
use crate::error::{DbError, DbResult};
use sqlx::PgExecutor;
use tankovault_domain::{AccountStatus, User, UserId};

/// Apply a username and/or email change.
///
/// A changed email clears `email_verified_at` — prevents an attacker with a short-lived token
/// re-pointing the account to an already-"verified"-looking address. `$3 <> email` compares
/// through [`CiText`] so re-capitalising your own address isn't treated as a change.
///
/// # Errors
/// [`DbError::Conflict`] if the new email or username is taken. An unknown `id` is **not**
/// [`DbError::NotFound`] (arrives as `RowNotFound` inside [`DbError::Sqlx`]) — every caller
/// holds an authenticated id, so a miss means erasure mid-request.
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

/// Read a user's notification preferences JSON. `{}` means "defaults".
///
/// # Errors
/// [`DbError::Sqlx`] only. An unknown `id` yields `{}`, not [`DbError::NotFound`].
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

/// Replace a user's notification preferences JSON.
///
/// # Errors
/// [`DbError::NotFound`] — a 404 — when `id` matches no row. Otherwise [`DbError::Sqlx`].
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
