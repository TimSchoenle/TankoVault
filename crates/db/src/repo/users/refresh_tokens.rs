//! Rotating refresh tokens and the reuse-detection primitives over them.
//!
//! Only hashes are stored. A *family* is one rotation lineage: normal rotation revokes a
//! single token, and presenting an already-revoked one is the signature of a compromised
//! lineage, so [`revoke_family`] takes the whole family out. It is only the *signature*, not
//! proof — an interrupted rotation looks identical from here — which is why
//! [`family_has_live_token`] exists and why the judgement is made at the call site
//! (`services/api/src/auth/session.rs`) with a time bound this layer knows nothing about. The
//! lookups deliberately do **not** filter
//! revoked or expired rows — that judgement belongs to the caller, and hiding a revoked token
//! would make a replayed one indistinguishable from an unknown one, which is precisely the
//! case reuse detection exists to catch.

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
/// [`crate::DbError::Sqlx`] only — no other variant is reachable. A `token_hash` that already
/// exists is a unique violation left as a driver error rather than translated to
/// [`crate::DbError::Conflict`]: the value is 256 bits of server-generated randomness, so a
/// collision is a fault in the generator and must surface as a 500, never as a 409 a client
/// could learn to trigger.
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
/// [`crate::DbError::Sqlx`] only — no other variant is reachable. An unknown hash is
/// `Ok(None)`, and it must stay indistinguishable from a revoked or expired one at this layer;
/// see the module docs for why the filtering is the caller's job.
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
/// [`crate::DbError::Sqlx`] only — no other variant is reachable. An unknown or
/// already-revoked id is `Ok(())`, which makes rotation idempotent under a retry.
pub async fn revoke_token<'e, E: PgExecutor<'e>>(exec: E, id: Uuid) -> DbResult<()> {
    sqlx::query!(
        "UPDATE refresh_tokens SET revoked_at = now() WHERE id = $1 AND revoked_at IS NULL",
        id,
    )
    .execute(exec)
    .await?;
    Ok(())
}

/// Does `family_id` still hold a token that is usable right now — unrevoked and unexpired?
///
/// This is the second half of the test that separates an *interrupted* rotation from token
/// theft (`services/api/src/auth/session.rs::refresh`). On its own, "the presented token is
/// revoked" cannot tell the two apart: rotation revokes the old token and issues the new one
/// server-side, but the client only learns the new value if the response reaches it. A lost
/// response, or a second request that raced the first, leaves a perfectly honest client
/// holding a value the server has already retired.
///
/// A live sibling is what makes the difference. It means the lineage is still running — some
/// party successfully took delivery of the successor — so the presenter is racing that
/// rotation rather than replaying a lineage that has already been shut down. Paired with a
/// tight time bound at the call site, that is a *narrow* window, and it is deliberately not a
/// substitute for reuse detection: see the caller for what happens after.
///
/// # Errors
/// [`crate::DbError::Sqlx`] only — no other variant is reachable.
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
/// [`crate::DbError::Sqlx`] only — no other variant is reachable. An unknown `family_id` is
/// `Ok(())`. Callers **must** propagate: this is the response to detected token reuse, and a
/// swallowed failure leaves a compromised lineage usable.
pub async fn revoke_family<'e, E: PgExecutor<'e>>(exec: E, family_id: Uuid) -> DbResult<()> {
    sqlx::query!(
        "UPDATE refresh_tokens SET revoked_at = now() WHERE family_id = $1 AND revoked_at IS NULL",
        family_id,
    )
    .execute(exec)
    .await?;
    Ok(())
}
