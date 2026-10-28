//! Single-use `state` values for an in-flight OAuth link, stored as the digest of their handle.

use crate::error::DbResult;
use sqlx::PgExecutor;
use tankovault_domain::UserId;
use time::OffsetDateTime;

/// Records a link `state` for `user_id` at `provider`, storing only its handle's digest.
///
/// # Errors
/// [`crate::DbError::Sqlx`] only.
pub async fn insert_link_state<'e, E: PgExecutor<'e>>(
    exec: E,
    user_id: UserId,
    provider: &str,
    token_hash: &str,
    expires_at: OffsetDateTime,
) -> DbResult<()> {
    sqlx::query!(
        "INSERT INTO oauth_link_states (token_hash, user_id, provider, expires_at) \
         VALUES ($1, $2, $3, $4)",
        token_hash,
        user_id.as_uuid(),
        provider,
        expires_at,
    )
    .execute(exec)
    .await?;
    Ok(())
}

/// Spends a live link `state` minted for this user and provider; `false` when there is none.
///
/// A match is deleted in the same statement, so of two callbacks racing on one `state` exactly
/// one sees `true`. A `state` presented by the wrong account or for the wrong provider is left
/// in place: the session that started the link can still finish it.
///
/// # Errors
/// [`crate::DbError::Sqlx`] only.
pub async fn consume_link_state<'e, E: PgExecutor<'e>>(
    exec: E,
    user_id: UserId,
    provider: &str,
    token_hash: &str,
) -> DbResult<bool> {
    let result = sqlx::query!(
        "DELETE FROM oauth_link_states \
         WHERE token_hash = $1 AND user_id = $2 AND provider = $3 AND expires_at > now()",
        token_hash,
        user_id.as_uuid(),
        provider,
    )
    .execute(exec)
    .await?;
    Ok(result.rows_affected() == 1)
}

/// Deletes expired link states; returns how many were removed.
///
/// # Errors
/// [`crate::DbError::Sqlx`] only.
pub async fn prune_expired_link_states<'e, E: PgExecutor<'e>>(exec: E) -> DbResult<u64> {
    let result = sqlx::query!("DELETE FROM oauth_link_states WHERE expires_at <= now()")
        .execute(exec)
        .await?;
    Ok(result.rows_affected())
}
