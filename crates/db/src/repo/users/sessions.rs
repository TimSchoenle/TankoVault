//! Sessions as the user sees them, and the three ways they end. A *session* is a rotation
//! family, not a token, so every read/write here is scoped to the whole family.

use crate::error::DbResult;
use sqlx::{FromRow, PgExecutor};
use tankovault_domain::UserId;
use time::OffsetDateTime;
use uuid::Uuid;

/// An active login session, derived from a live (unrevoked, unexpired) refresh token. Only
/// non-secret metadata is exposed.
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
/// [`crate::DbError::Sqlx`] only. No live session is an empty `Vec`, not [`crate::DbError::NotFound`].
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

/// Revoke one of the user's own sessions (its whole rotation family). Returns tokens revoked
/// (0 if the session id was not the caller's).
///
/// # Errors
/// [`crate::DbError::Sqlx`] only. A foreign `session_id` is `Ok(0)`, not [`crate::DbError::NotFound`]
/// — prevents probing other users' session ids.
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

/// Revoke every live session for a user, returning tokens invalidated. Distinct from
/// [`revoke_all_for_user`] only in that it reports the count, for an operator-forced sign-out.
///
/// # Errors
/// [`crate::DbError::Sqlx`] only. Nothing live is `Ok(0)`.
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

/// Revoke every live refresh token for a user — used after a password reset so a stolen
/// session doesn't survive the changed credential.
///
/// # Errors
/// [`crate::DbError::Sqlx`] only. Callers must propagate: swallowing it leaves a stolen
/// session alive.
pub async fn revoke_all_for_user<'e, E: PgExecutor<'e>>(exec: E, user_id: UserId) -> DbResult<()> {
    sqlx::query!(
        "UPDATE refresh_tokens SET revoked_at = now() WHERE user_id = $1 AND revoked_at IS NULL",
        user_id.as_uuid(),
    )
    .execute(exec)
    .await?;
    Ok(())
}
