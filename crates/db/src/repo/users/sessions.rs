//! Sessions as the user sees them, and the three ways they end.
//!
//! A *session* is a rotation family, not a token: refreshing mints a new row, so revoking only
//! the row on screen would sign the user out for exactly one request cycle. Every read and
//! every write here is scoped to the owning user — [`revoke_session`] through a subquery
//! rather than the outer `WHERE`, which is the easiest scope in the module to lose.
//!
//! [`revoke_all_for_user`] sits here rather than with the password reset that calls it: it and
//! [`revoke_all_sessions`] are the same statement differing only in whether the count comes
//! back, and apart they are two rows waiting to be edited one at a time.

use crate::error::DbResult;
use sqlx::{FromRow, PgExecutor};
use tankovault_domain::UserId;
use time::OffsetDateTime;
use uuid::Uuid;

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
///
/// # Errors
/// [`crate::DbError::Sqlx`] only — no other variant is reachable. A user with no live session
/// gets an empty `Vec`, not [`crate::DbError::NotFound`].
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
///
/// # Errors
/// [`crate::DbError::Sqlx`] only — no other variant is reachable. A `session_id` belonging to
/// another user is `Ok(0)`, not [`crate::DbError::NotFound`]: the ownership subquery makes the
/// two cases identical on purpose, so probing ids cannot map out other people's sessions.
/// A caller wanting a 404 must decide that from the count.
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

/// Revoke **every** live session for a user, returning how many tokens were invalidated.
///
/// Distinct from [`revoke_all_for_user`], which exists for the password-reset path and
/// discards the count: an operator forcing a sign-out needs to be told whether there was
/// anything to sign out of.
///
/// # Errors
/// [`crate::DbError::Sqlx`] only — no other variant is reachable. "Nothing was live" is
/// `Ok(0)`.
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

/// Revoke every live refresh token for a user — used after a password reset so any stolen
/// session is invalidated along with the changed credential.
///
/// # Errors
/// [`crate::DbError::Sqlx`] only — no other variant is reachable. Callers **must** propagate:
/// this runs after the password has already changed, so swallowing the failure would leave a
/// stolen session alive against a credential its holder no longer knows.
pub async fn revoke_all_for_user<'e, E: PgExecutor<'e>>(exec: E, user_id: UserId) -> DbResult<()> {
    sqlx::query!(
        "UPDATE refresh_tokens SET revoked_at = now() WHERE user_id = $1 AND revoked_at IS NULL",
        user_id.as_uuid(),
    )
    .execute(exec)
    .await?;
    Ok(())
}
