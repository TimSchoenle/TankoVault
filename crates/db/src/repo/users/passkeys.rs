//! Passkeys (`WebAuthn` credentials) and the short-lived ceremony state that registers or
//! asserts them. Credential and ceremony state are opaque `webauthn-rs` JSON; every write is
//! scoped to a user id in the statement itself.

use crate::error::{DbError, DbResult};
use sqlx::{FromRow, PgExecutor};
use tankovault_domain::UserId;
use time::OffsetDateTime;
use uuid::Uuid;

/// A registered passkey, as the account page and the authenticator both need it.
#[derive(Debug, Clone)]
pub struct PasskeyRecord {
    pub id: Uuid,
    pub user_id: UserId,
    pub credential_id: Vec<u8>,
    /// Serialised `webauthn_rs::prelude::Passkey`; opaque here.
    pub credential: serde_json::Value,
    pub label: String,
    pub created_at: OffsetDateTime,
    pub last_used_at: Option<OffsetDateTime>,
}

/// The row shape shared by every read below.
#[derive(FromRow)]
struct Row {
    id: Uuid,
    user_id: Uuid,
    credential_id: Vec<u8>,
    credential: serde_json::Value,
    label: String,
    created_at: OffsetDateTime,
    last_used_at: Option<OffsetDateTime>,
}

impl From<Row> for PasskeyRecord {
    fn from(r: Row) -> Self {
        Self {
            id: r.id,
            user_id: UserId::from_uuid(r.user_id),
            credential_id: r.credential_id,
            credential: r.credential,
            label: r.label,
            created_at: r.created_at,
            last_used_at: r.last_used_at,
        }
    }
}

/// Every passkey registered to a user, newest first.
///
/// # Errors
/// [`DbError::Sqlx`] only. No passkeys is an empty `Vec`, not [`DbError::NotFound`].
pub async fn list_for_user<'e, E: PgExecutor<'e>>(
    exec: E,
    user_id: UserId,
) -> DbResult<Vec<PasskeyRecord>> {
    let rows = sqlx::query_as!(
        Row,
        "SELECT id, user_id, credential_id, credential, label, created_at, last_used_at \
         FROM user_passkeys WHERE user_id = $1 ORDER BY created_at DESC",
        user_id.as_uuid(),
    )
    .fetch_all(exec)
    .await?;
    Ok(rows.into_iter().map(Into::into).collect())
}

/// The credential ids already registered to a user, fed to `start_passkey_registration` as its
/// exclude list so re-registering the same authenticator is rejected at the prompt.
///
/// # Errors
/// [`DbError::Sqlx`] only. No passkeys is `Ok(vec![])`.
pub async fn credential_ids_for_user<'e, E: PgExecutor<'e>>(
    exec: E,
    user_id: UserId,
) -> DbResult<Vec<Vec<u8>>> {
    let rows = sqlx::query!(
        "SELECT credential_id FROM user_passkeys WHERE user_id = $1",
        user_id.as_uuid(),
    )
    .fetch_all(exec)
    .await?;
    Ok(rows.into_iter().map(|r| r.credential_id).collect())
}

/// One passkey by the credential id the authenticator presented. Not scoped to a user — a
/// discoverable assertion resolves from the credential; the caller must check the returned
/// `user_id` against the authenticator's user handle.
///
/// # Errors
/// [`DbError::Sqlx`] only. An unknown id is `Ok(None)`, not [`DbError::NotFound`].
pub async fn find_by_credential_id<'e, E: PgExecutor<'e>>(
    exec: E,
    credential_id: &[u8],
) -> DbResult<Option<PasskeyRecord>> {
    let row = sqlx::query_as!(
        Row,
        "SELECT id, user_id, credential_id, credential, label, created_at, last_used_at \
         FROM user_passkeys WHERE credential_id = $1",
        credential_id,
    )
    .fetch_optional(exec)
    .await?;
    Ok(row.map(Into::into))
}

/// Register a verified credential against a user.
///
/// # Errors
/// [`DbError::Conflict`] if this credential id is already registered to any account — the
/// uniqueness is global so an attacker cannot claim an observed credential id. [`DbError::Sqlx`]
/// otherwise.
pub async fn insert<'e, E: PgExecutor<'e>>(
    exec: E,
    user_id: UserId,
    credential_id: &[u8],
    credential: &serde_json::Value,
    label: &str,
) -> DbResult<PasskeyRecord> {
    let row = sqlx::query_as!(
        Row,
        "INSERT INTO user_passkeys (user_id, credential_id, credential, label) \
         VALUES ($1,$2,$3,$4) \
         RETURNING id, user_id, credential_id, credential, label, created_at, last_used_at",
        user_id.as_uuid(),
        credential_id,
        credential,
        label,
    )
    .fetch_one(exec)
    .await
    .map_err(|e| {
        let de = DbError::from(e);
        if de.is_unique_violation() {
            DbError::Conflict("this passkey is already registered".to_owned())
        } else {
            de
        }
    })?;
    Ok(row.into())
}

/// Record a successful assertion: stamp `last_used_at` and store the credential
/// `Passkey::update_credential` handed back, wholesale.
///
/// # Errors
/// [`DbError::Sqlx`] only. A deleted credential id is `Ok(())` — the sign-in already succeeded.
pub async fn record_use<'e, E: PgExecutor<'e>>(
    exec: E,
    credential_id: &[u8],
    credential: &serde_json::Value,
) -> DbResult<()> {
    sqlx::query!(
        "UPDATE user_passkeys SET credential = $2, last_used_at = now() WHERE credential_id = $1",
        credential_id,
        credential,
    )
    .execute(exec)
    .await?;
    Ok(())
}

/// Rename one of the caller's own passkeys. Returns rows changed (0 if the id was not theirs).
///
/// # Errors
/// [`DbError::Sqlx`] only. A foreign id is `Ok(0)`, not [`DbError::NotFound`] — prevents
/// probing other users' key ids.
pub async fn rename<'e, E: PgExecutor<'e>>(
    exec: E,
    user_id: UserId,
    id: Uuid,
    label: &str,
) -> DbResult<u64> {
    let result = sqlx::query!(
        "UPDATE user_passkeys SET label = $3 WHERE id = $2 AND user_id = $1",
        user_id.as_uuid(),
        id,
        label,
    )
    .execute(exec)
    .await?;
    Ok(result.rows_affected())
}

/// Delete one of the caller's own passkeys. Returns rows removed.
///
/// # Errors
/// [`DbError::Sqlx`] only. See [`rename`] for why a foreign id is `Ok(0)`.
pub async fn delete<'e, E: PgExecutor<'e>>(exec: E, user_id: UserId, id: Uuid) -> DbResult<u64> {
    let result = sqlx::query!(
        "DELETE FROM user_passkeys WHERE id = $2 AND user_id = $1",
        user_id.as_uuid(),
        id,
    )
    .execute(exec)
    .await?;
    Ok(result.rows_affected())
}

// ---------------------------------------------------------------------------
// Ceremonies
// ---------------------------------------------------------------------------

/// Which leg of the protocol a stored ceremony belongs to — kept apart so a challenge minted
/// for one purpose cannot complete the other.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CeremonyKind {
    /// Registering a new credential against a known account.
    Register,
    /// A discoverable sign-in, where the account is not known until the response arrives.
    Authenticate,
}

impl CeremonyKind {
    /// The persisted string. Stable — it is a `CHECK` constraint's vocabulary.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Register => "register",
            Self::Authenticate => "authenticate",
        }
    }
}

/// A ceremony recovered from the store, ready to be handed back to `webauthn-rs`.
#[derive(Debug, Clone)]
pub struct Ceremony {
    /// `None` for a discoverable sign-in, which does not know who is signing in yet.
    pub user_id: Option<UserId>,
    /// The serialised `PasskeyRegistration` / `DiscoverableAuthentication`.
    pub state: serde_json::Value,
}

/// Store an in-flight ceremony under a caller-chosen id, to expire at `expires_at`.
///
/// # Errors
/// [`DbError::Sqlx`] only. A primary-key collision on the random id is a 500.
pub async fn insert_ceremony<'e, E: PgExecutor<'e>>(
    exec: E,
    id: Uuid,
    user_id: Option<UserId>,
    kind: CeremonyKind,
    state: &serde_json::Value,
    expires_at: OffsetDateTime,
) -> DbResult<()> {
    sqlx::query!(
        "INSERT INTO webauthn_ceremonies (id, user_id, kind, state, expires_at) \
         VALUES ($1,$2,$3,$4,$5)",
        id,
        user_id.map(UserId::as_uuid),
        kind.as_str(),
        state,
        expires_at,
    )
    .execute(exec)
    .await?;
    Ok(())
}

/// Consume a ceremony: fetch and delete it in one `DELETE ... RETURNING` statement, so a
/// challenge can never survive its use and be replayed.
///
/// # Errors
/// [`DbError::Sqlx`] only. Unknown, consumed, expired or wrong-`kind` all collapse to `Ok(None)`
/// — telling them apart would let a client probe ceremony ids.
pub async fn take_ceremony<'e, E: PgExecutor<'e>>(
    exec: E,
    id: Uuid,
    kind: CeremonyKind,
) -> DbResult<Option<Ceremony>> {
    #[derive(FromRow)]
    struct CeremonyRow {
        user_id: Option<Uuid>,
        state: serde_json::Value,
    }
    let row = sqlx::query_as!(
        CeremonyRow,
        "DELETE FROM webauthn_ceremonies \
         WHERE id = $1 AND kind = $2 AND expires_at > now() \
         RETURNING user_id, state",
        id,
        kind.as_str(),
    )
    .fetch_optional(exec)
    .await?;
    Ok(row.map(|r| Ceremony {
        user_id: r.user_id.map(UserId::from_uuid),
        state: r.state,
    }))
}

/// Delete every ceremony whose deadline has passed (abandoned-ceremony sweep), returning the
/// count removed. Reclaims space only; [`take_ceremony`] already filters on expiry.
///
/// # Errors
/// [`DbError::Sqlx`] only. Nothing to delete is `Ok(0)`.
pub async fn prune_expired_ceremonies<'e, E: PgExecutor<'e>>(exec: E) -> DbResult<u64> {
    let result = sqlx::query!("DELETE FROM webauthn_ceremonies WHERE expires_at <= now()")
        .execute(exec)
        .await?;
    Ok(result.rows_affected())
}

#[cfg(test)]
mod tests {
    use super::CeremonyKind;

    /// Bug this pins: renaming a `CeremonyKind` string without updating the CHECK constraint
    /// makes every `INSERT` fail and every `take_ceremony` silently read as "expired".
    #[test]
    fn the_persisted_kinds_match_the_check_constraint() {
        const MIGRATION: &str = include_str!("../../../../../migrations/0022_passkeys.up.sql");

        assert_eq!(CeremonyKind::Register.as_str(), "register");
        assert_eq!(CeremonyKind::Authenticate.as_str(), "authenticate");

        for kind in [CeremonyKind::Register, CeremonyKind::Authenticate] {
            assert!(
                MIGRATION.contains(&format!("'{}'", kind.as_str())),
                "`{}` is not in the migration's CHECK vocabulary",
                kind.as_str()
            );
        }
    }
}
