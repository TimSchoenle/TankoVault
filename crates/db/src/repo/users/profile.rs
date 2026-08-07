//! Self-service account settings: the identity a user may change about themselves, plus their
//! notification preferences. Session management lives in [`super::sessions`].

use super::CiText;
use super::credentials::UserRow;
use crate::error::{DbError, DbResult};
use sqlx::PgExecutor;
use tankovault_domain::{AccountStatus, NotificationPrefs, User, UserId};
use time::OffsetDateTime;

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

/// A reader's stored answer on adult content.
#[derive(Debug, Clone, Copy)]
pub struct ContentPrefs {
    /// The preference itself. Meaningless without [`Self::age_attested_at`] — a schema
    /// constraint refuses the pair, so this cannot be true while that is `None`.
    pub adult_opt_in: bool,
    /// When the account confirmed it is of age, or `None` if it never has.
    ///
    /// Kept after the preference is switched back off, so a reader who opts out and later opts
    /// in again is not asked to attest twice. It is a fact about the account, not a component
    /// of the current setting.
    pub age_attested_at: Option<OffsetDateTime>,
}

/// Read a user's content preferences.
///
/// An unknown id yields the closed default rather than an error: every caller is deciding what
/// to *show*, and the safe answer to "who is this?" is "nothing gated".
///
/// # Errors
/// [`DbError::Sqlx`] only.
pub async fn get_content_prefs<'e, E: PgExecutor<'e>>(
    exec: E,
    id: UserId,
) -> DbResult<ContentPrefs> {
    let row = sqlx::query!(
        "SELECT adult_opt_in, age_attested_at FROM users WHERE id = $1",
        id.as_uuid(),
    )
    .fetch_optional(exec)
    .await?;
    Ok(row.map_or(
        ContentPrefs {
            adult_opt_in: false,
            age_attested_at: None,
        },
        |r| ContentPrefs {
            adult_opt_in: r.adult_opt_in,
            age_attested_at: r.age_attested_at,
        },
    ))
}

/// Record a reader's adult-content decision, stamping the attestation on the way in.
///
/// `attesting` is the caller's confirmation *in this request* that they are of age. Opting in
/// requires either that or a stamp from a previous one; `age_attested_at` is written once and
/// never moved, so the record says when the account first attested rather than when it last
/// toggled a switch.
///
/// Opting *out* never needs an attestation and never clears one.
///
/// # Errors
/// [`DbError::NotFound`] when `id` matches no row. [`DbError::Conflict`] when opting in without
/// an attestation, either in this request or on record — the same condition the schema's
/// `users_adult_opt_in_requires_attestation` refuses, checked here so the caller gets a
/// meaningful status rather than a constraint violation. Otherwise [`DbError::Sqlx`].
pub async fn set_content_prefs<'e, E: PgExecutor<'e>>(
    exec: E,
    id: UserId,
    adult_opt_in: bool,
    attesting: bool,
) -> DbResult<ContentPrefs> {
    // One statement, not read-then-write: two requests toggling the preference concurrently
    // would otherwise interleave between the check and the update, and the losing one could
    // write an opt-in whose attestation it never saw.
    let row = sqlx::query!(
        "UPDATE users SET \
            age_attested_at = CASE WHEN $3 THEN COALESCE(age_attested_at, now()) \
                                   ELSE age_attested_at END, \
            adult_opt_in = ($2 AND ($3 OR age_attested_at IS NOT NULL)) \
         WHERE id = $1 \
         RETURNING adult_opt_in, age_attested_at",
        id.as_uuid(),
        adult_opt_in,
        attesting,
    )
    .fetch_optional(exec)
    .await?
    .ok_or(DbError::NotFound)?;

    // Asked to opt in, and the row says otherwise: the only way that happens is a missing
    // attestation. Reported rather than returned quietly, or the caller shows a switch that
    // silently springs back.
    if adult_opt_in && !row.adult_opt_in {
        return Err(DbError::Conflict(
            "opting into adult content requires an age attestation".to_owned(),
        ));
    }
    Ok(ContentPrefs {
        adult_opt_in: row.adult_opt_in,
        age_attested_at: row.age_attested_at,
    })
}

/// Read a user's notification preferences.
///
/// An unknown id, an empty document and a document this build cannot parse all yield
/// [`NotificationPrefs::default`]. Failing here would cost the reader the notification the
/// preferences were only ever meant to shape.
///
/// # Errors
/// [`DbError::Sqlx`] only.
pub async fn get_notification_prefs<'e, E: PgExecutor<'e>>(
    exec: E,
    id: UserId,
) -> DbResult<NotificationPrefs> {
    let stored = sqlx::query_scalar!(
        "SELECT notification_prefs AS \"notification_prefs: serde_json::Value\" FROM users WHERE id = $1",
        id.as_uuid(),
    )
    .fetch_optional(exec)
    .await?;
    Ok(stored
        .and_then(|value| serde_json::from_value(value).ok())
        .unwrap_or_default())
}

/// Replace a user's notification preferences.
///
/// # Errors
/// [`DbError::NotFound`] — a 404 — when `id` matches no row. [`DbError::Serialization`] if the
/// document cannot be encoded. Otherwise [`DbError::Sqlx`].
pub async fn set_notification_prefs<'e, E: PgExecutor<'e>>(
    exec: E,
    id: UserId,
    prefs: &NotificationPrefs,
) -> DbResult<()> {
    let document = serde_json::to_value(prefs)?;
    let result = sqlx::query!(
        "UPDATE users SET notification_prefs = $2 WHERE id = $1",
        id.as_uuid(),
        document,
    )
    .execute(exec)
    .await?;
    if result.rows_affected() == 0 {
        return Err(DbError::NotFound);
    }
    Ok(())
}
