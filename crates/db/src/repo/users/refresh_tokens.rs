//! Rotating refresh tokens (hash only) and the reuse-detection primitives over them. Lookups
//! deliberately do not filter revoked/expired rows — that judgement is the caller's.

use crate::error::DbResult;
use sqlx::{FromRow, PgExecutor};
use tankovault_domain::UserId;
use time::OffsetDateTime;
use uuid::Uuid;

/// A stored refresh-token record (hash only).
pub struct RefreshRecord {
    pub id: Uuid,
    pub user_id: UserId,
    pub family_id: Uuid,
    pub expires_at: OffsetDateTime,
    pub revoked_at: Option<OffsetDateTime>,
}

/// Persist a freshly issued refresh token (as its SHA-256 hash).
///
/// # Errors
/// [`crate::DbError::Sqlx`] only. A duplicate `token_hash` is a generator fault (500), not a
/// client-triggerable [`crate::DbError::Conflict`] (409).
pub async fn insert_refresh<'e, E: PgExecutor<'e>>(
    exec: E,
    user_id: UserId,
    family_id: Uuid,
    token_hash: &str,
    expires_at: OffsetDateTime,
) -> DbResult<()> {
    sqlx::query!(
        "INSERT INTO refresh_tokens (id, user_id, family_id, token_hash, expires_at) \
         VALUES ($1,$2,$3,$4,$5)",
        Uuid::now_v7(),
        user_id.as_uuid(),
        family_id,
        token_hash,
        expires_at,
    )
    .execute(exec)
    .await?;
    Ok(())
}

/// Find a refresh token by its hash (regardless of revocation, for reuse detection).
///
/// # Errors
/// [`crate::DbError::Sqlx`] only. Unknown, revoked and expired all return the same `Ok`/`None`
/// shape — filtering is the caller's job.
pub async fn find_refresh<'e, E: PgExecutor<'e>>(
    exec: E,
    token_hash: &str,
) -> DbResult<Option<RefreshRecord>> {
    #[derive(FromRow)]
    struct Row {
        id: Uuid,
        user_id: Uuid,
        family_id: Uuid,
        expires_at: OffsetDateTime,
        revoked_at: Option<OffsetDateTime>,
    }
    let row = sqlx::query_as!(
        Row,
        "SELECT id, user_id, family_id, expires_at, revoked_at FROM refresh_tokens \
         WHERE token_hash = $1",
        token_hash,
    )
    .fetch_optional(exec)
    .await?;
    Ok(row.map(|r| RefreshRecord {
        id: r.id,
        user_id: UserId::from_uuid(r.user_id),
        family_id: r.family_id,
        expires_at: r.expires_at,
        revoked_at: r.revoked_at,
    }))
}

/// Revoke a single token by id (normal rotation).
///
/// # Errors
/// [`crate::DbError::Sqlx`] only. Unknown/already-revoked is `Ok(())` (idempotent retry).
pub async fn revoke_token<'e, E: PgExecutor<'e>>(exec: E, id: Uuid) -> DbResult<()> {
    sqlx::query!(
        "UPDATE refresh_tokens SET revoked_at = now() WHERE id = $1 AND revoked_at IS NULL",
        id,
    )
    .execute(exec)
    .await?;
    Ok(())
}

/// Does `family_id` still hold a token that is usable right now (unrevoked, unexpired)?
///
/// Distinguishes an interrupted rotation from token theft: "presented token is revoked" alone
/// can't tell a lost-response retry from replay, but a live sibling means the lineage is still
/// running. Caller (`session.rs::refresh`) pairs this with a tight time bound.
///
/// # Errors
/// [`crate::DbError::Sqlx`] only.
pub async fn family_has_live_token<'e, E: PgExecutor<'e>>(
    exec: E,
    family_id: Uuid,
) -> DbResult<bool> {
    let live = sqlx::query_scalar!(
        "SELECT EXISTS( \
             SELECT 1 FROM refresh_tokens \
              WHERE family_id = $1 AND revoked_at IS NULL AND expires_at > now() \
         ) AS \"live!\"",
        family_id,
    )
    .fetch_one(exec)
    .await?;
    Ok(live)
}

/// Revoke an entire token family (reuse detected → invalidate the lineage).
///
/// # Errors
/// [`crate::DbError::Sqlx`] only. Callers must propagate — swallowing it leaves a compromised
/// lineage usable.
pub async fn revoke_family<'e, E: PgExecutor<'e>>(exec: E, family_id: Uuid) -> DbResult<()> {
    sqlx::query!(
        "UPDATE refresh_tokens SET revoked_at = now() WHERE family_id = $1 AND revoked_at IS NULL",
        family_id,
    )
    .execute(exec)
    .await?;
    Ok(())
}
