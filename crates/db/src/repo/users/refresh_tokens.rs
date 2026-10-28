//! Rotating refresh tokens (hash only) and the reuse-detection primitives over them. Lookups do
//! not filter revoked or expired rows; that judgement is the caller's.

use crate::error::DbResult;
use sqlx::{FromRow, PgExecutor};
use tankovault_domain::UserId;
use time::OffsetDateTime;
use uuid::Uuid;

/// A stored refresh-token record (hash only).
pub struct RefreshRecord {
    /// The stored row. The token itself is held only as a hash.
    pub id: Uuid,
    /// Whose session it belongs to.
    pub user_id: UserId,
    /// The rotation chain. Reusing a spent token revokes the whole family.
    pub family_id: Uuid,
    /// When the token stops being accepted.
    pub expires_at: OffsetDateTime,
    /// When it was revoked, `None` while it still stands.
    pub revoked_at: Option<OffsetDateTime>,
}

/// Persist a freshly issued refresh token (as its SHA-256 hash).
///
/// A duplicate `token_hash` is a generator fault (500), not a client-triggerable
/// [`crate::DbError::Conflict`] (409).
///
/// # Errors
/// [`crate::DbError::Sqlx`] only.
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

/// Finds a refresh token by its hash, regardless of revocation, for reuse detection.
///
/// # Errors
/// [`crate::DbError::Sqlx`] only; unknown, revoked and expired all return the same `Ok`/`None`
/// shape.
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

/// Revokes a single token by id (normal rotation); `true` when this call revoked it.
///
/// A compare-and-swap: of two concurrent calls for one token exactly one sees `true`, because
/// the second blocks on the row lock and then finds `revoked_at` already set. Run inside the
/// transaction that inserts the successor, so the loser sees that successor once it unblocks.
///
/// # Errors
/// [`crate::DbError::Sqlx`] only; unknown/already-revoked is `Ok(false)`.
pub async fn revoke_token<'e, E: PgExecutor<'e>>(exec: E, id: Uuid) -> DbResult<bool> {
    let done = sqlx::query!(
        "UPDATE refresh_tokens SET revoked_at = now() WHERE id = $1 AND revoked_at IS NULL",
        id,
    )
    .execute(exec)
    .await?;
    Ok(done.rows_affected() == 1)
}

/// Revokes an entire token family when reuse is detected, returning how many tokens were still
/// standing.
///
/// The count is the liveness test for the rotation grace path: taken by the same statement that
/// collapses the family, two racing retries cannot both see a live lineage and both reissue.
/// Callers must propagate the error, or a compromised lineage stays usable.
///
/// # Errors
/// [`crate::DbError::Sqlx`] only.
pub async fn revoke_family<'e, E: PgExecutor<'e>>(exec: E, family_id: Uuid) -> DbResult<u64> {
    let done = sqlx::query!(
        "UPDATE refresh_tokens SET revoked_at = now() WHERE family_id = $1 AND revoked_at IS NULL",
        family_id,
    )
    .execute(exec)
    .await?;
    Ok(done.rows_affected())
}
