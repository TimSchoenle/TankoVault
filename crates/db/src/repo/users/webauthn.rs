//! `WebAuthn` credentials — passkeys and second-factor security keys — and the short-lived
//! ceremony state that registers or asserts them. Credential and ceremony state are opaque
//! `webauthn-rs` JSON; every write is scoped to a user id in the statement itself.
//!
//! Both kinds live in one table, and every read here is scoped by [`CredentialPurpose`]. That
//! is not tidiness: an unscoped read would let a security key answer a passkey assertion, which
//! turns a second factor into a standalone sign-in credential.
//! `migrations/0043_multi_factor_auth.up.sql` carries the full argument.

use crate::error::{DbError, DbResult};
use sqlx::{FromRow, PgExecutor};
use tankovault_domain::UserId;
use time::OffsetDateTime;
use uuid::Uuid;

/// Which leg of authentication a stored credential serves.
///
/// A single authenticator may hold at most one of these per account: the table's global
/// `UNIQUE (credential_id)` sees to it, because a device registered as both would satisfy the
/// password leg and the second-factor leg with one touch.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CredentialPurpose {
    /// A discoverable, first-factor sign-in credential.
    Passkey,
    /// A second factor, presented only after a password has already verified.
    SecurityKey,
}

impl CredentialPurpose {
    /// The persisted string. Stable — it is a `CHECK` constraint's vocabulary.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Passkey => "passkey",
            Self::SecurityKey => "security_key",
        }
    }
}

/// A registered credential, as the account page and the authenticator both need it.
///
/// Carries no `purpose`: every read below is already scoped by one, so the caller knows which
/// it asked for, and a field that could disagree with the query is a field that will.
#[derive(Debug, Clone)]
pub struct CredentialRecord {
    pub id: Uuid,
    pub user_id: UserId,
    pub credential_id: Vec<u8>,
    /// Serialised `webauthn_rs::prelude::Passkey` or `SecurityKey`; opaque here.
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

impl From<Row> for CredentialRecord {
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

/// Every credential of one purpose registered to a user, newest first.
///
/// # Errors
/// [`DbError::Sqlx`] only. None registered is an empty `Vec`, not [`DbError::NotFound`].
pub async fn list_for_user<'e, E: PgExecutor<'e>>(
    exec: E,
    user_id: UserId,
    purpose: CredentialPurpose,
) -> DbResult<Vec<CredentialRecord>> {
    let rows = sqlx::query_as!(
        Row,
        "SELECT id, user_id, credential_id, credential, label, created_at, last_used_at \
         FROM user_webauthn_credentials WHERE user_id = $1 AND purpose = $2 \
         ORDER BY created_at DESC",
        user_id.as_uuid(),
        purpose.as_str(),
    )
    .fetch_all(exec)
    .await?;
    Ok(rows.into_iter().map(Into::into).collect())
}

/// Whether a user holds at least one security key.
///
/// Half of "does this account have a second factor"; the other half is a confirmed TOTP row
/// (`crate::repo::users::mfa::is_enrolled` answers the whole question).
///
/// # Errors
/// [`DbError::Sqlx`] only.
pub async fn has_security_key<'e, E: PgExecutor<'e>>(exec: E, user_id: UserId) -> DbResult<bool> {
    let row = sqlx::query!(
        "SELECT EXISTS (\
           SELECT 1 FROM user_webauthn_credentials \
           WHERE user_id = $1 AND purpose = 'security_key'\
         ) AS \"present!\"",
        user_id.as_uuid(),
    )
    .fetch_one(exec)
    .await?;
    Ok(row.present)
}

/// Every credential id already registered to a user, **across both purposes**, fed to a
/// registration ceremony as its exclude list.
///
/// Deliberately unscoped, unlike every other read here. The exclude list exists to stop one
/// authenticator holding two credentials on one account, and the case that matters is exactly
/// the cross-purpose one: a `YubiKey` registered as a passkey must not also become that account's
/// second factor, or a single touch clears both legs.
///
/// # Errors
/// [`DbError::Sqlx`] only. None registered is `Ok(vec![])`.
pub async fn credential_ids_for_user<'e, E: PgExecutor<'e>>(
    exec: E,
    user_id: UserId,
) -> DbResult<Vec<Vec<u8>>> {
    let rows = sqlx::query!(
        "SELECT credential_id FROM user_webauthn_credentials WHERE user_id = $1",
        user_id.as_uuid(),
    )
    .fetch_all(exec)
    .await?;
    Ok(rows.into_iter().map(|r| r.credential_id).collect())
}

/// One credential by the id the authenticator presented, of the expected purpose.
///
/// Not scoped to a user — a discoverable assertion resolves from the credential; the caller
/// must check the returned `user_id` against the authenticator's user handle. It *is* scoped to
/// a purpose, and that is load-bearing: without it a security key would resolve a passkey
/// sign-in and become a first factor on its own.
///
/// # Errors
/// [`DbError::Sqlx`] only. An unknown id, or one of the other purpose, is `Ok(None)`.
pub async fn find_by_credential_id<'e, E: PgExecutor<'e>>(
    exec: E,
    credential_id: &[u8],
    purpose: CredentialPurpose,
) -> DbResult<Option<CredentialRecord>> {
    let row = sqlx::query_as!(
        Row,
        "SELECT id, user_id, credential_id, credential, label, created_at, last_used_at \
         FROM user_webauthn_credentials WHERE credential_id = $1 AND purpose = $2",
        credential_id,
        purpose.as_str(),
    )
    .fetch_optional(exec)
    .await?;
    Ok(row.map(Into::into))
}

/// Register a verified credential against a user.
///
/// # Errors
/// [`DbError::Conflict`] if this credential id is already registered to any account, for either
/// purpose — the uniqueness is global so an attacker cannot claim an observed credential id, and
/// so one authenticator cannot serve as both factors. [`DbError::Sqlx`] otherwise.
pub async fn insert<'e, E: PgExecutor<'e>>(
    exec: E,
    user_id: UserId,
    credential_id: &[u8],
    credential: &serde_json::Value,
    label: &str,
    purpose: CredentialPurpose,
) -> DbResult<CredentialRecord> {
    let row = sqlx::query_as!(
        Row,
        "INSERT INTO user_webauthn_credentials \
           (user_id, credential_id, credential, label, purpose) \
         VALUES ($1,$2,$3,$4,$5) \
         RETURNING id, user_id, credential_id, credential, label, created_at, last_used_at",
        user_id.as_uuid(),
        credential_id,
        credential,
        label,
        purpose.as_str(),
    )
    .fetch_one(exec)
    .await
    .map_err(|e| {
        let de = DbError::from(e);
        if de.is_unique_violation() {
            DbError::Conflict("this authenticator is already registered".to_owned())
        } else {
            de
        }
    })?;
    Ok(row.into())
}

/// Record a successful assertion: stamp `last_used_at` and store the credential
/// `update_credential` handed back, wholesale.
///
/// Not scoped by purpose — the credential id is globally unique, so it identifies exactly one
/// row of exactly one purpose, and the caller has already resolved that row to get here.
///
/// # Errors
/// [`DbError::Sqlx`] only. A deleted credential id is `Ok(())` — the assertion already succeeded.
pub async fn record_use<'e, E: PgExecutor<'e>>(
    exec: E,
    credential_id: &[u8],
    credential: &serde_json::Value,
) -> DbResult<()> {
    sqlx::query!(
        "UPDATE user_webauthn_credentials SET credential = $2, last_used_at = now() \
         WHERE credential_id = $1",
        credential_id,
        credential,
    )
    .execute(exec)
    .await?;
    Ok(())
}

/// Rename one of the caller's own credentials. Returns rows changed (0 if the id was not theirs,
/// or not of this purpose).
///
/// # Errors
/// [`DbError::Sqlx`] only. A foreign id is `Ok(0)`, not [`DbError::NotFound`] — prevents
/// probing other users' key ids.
pub async fn rename<'e, E: PgExecutor<'e>>(
    exec: E,
    user_id: UserId,
    id: Uuid,
    label: &str,
    purpose: CredentialPurpose,
) -> DbResult<u64> {
    let result = sqlx::query!(
        "UPDATE user_webauthn_credentials SET label = $3 \
         WHERE id = $2 AND user_id = $1 AND purpose = $4",
        user_id.as_uuid(),
        id,
        label,
        purpose.as_str(),
    )
    .execute(exec)
    .await?;
    Ok(result.rows_affected())
}

/// Delete one of the caller's own credentials. Returns rows removed.
///
/// Scoped by purpose so the account page's two lists cannot reach into each other: "revoke this
/// passkey" must not be able to remove the security key that gates passkey enrolment.
///
/// # Errors
/// [`DbError::Sqlx`] only. See [`rename`] for why a foreign id is `Ok(0)`.
pub async fn delete<'e, E: PgExecutor<'e>>(
    exec: E,
    user_id: UserId,
    id: Uuid,
    purpose: CredentialPurpose,
) -> DbResult<u64> {
    let result = sqlx::query!(
        "DELETE FROM user_webauthn_credentials \
         WHERE id = $2 AND user_id = $1 AND purpose = $3",
        user_id.as_uuid(),
        id,
        purpose.as_str(),
    )
    .execute(exec)
    .await?;
    Ok(result.rows_affected())
}

// ---------------------------------------------------------------------------
// Ceremonies
// ---------------------------------------------------------------------------

/// Which leg of the protocol a stored ceremony belongs to — kept apart so a challenge minted
/// for one purpose cannot complete another.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CeremonyKind {
    /// Registering a new passkey against a known account.
    Register,
    /// A discoverable sign-in, where the account is not known until the response arrives.
    Authenticate,
    /// Registering a second-factor security key against a known account.
    RegisterSecurityKey,
    /// Asserting a second-factor security key, either to finish a sign-in or to step up.
    AuthenticateSecurityKey,
}

impl CeremonyKind {
    /// The persisted string. Stable — it is a `CHECK` constraint's vocabulary.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Register => "register",
            Self::Authenticate => "authenticate",
            Self::RegisterSecurityKey => "register_security_key",
            Self::AuthenticateSecurityKey => "authenticate_security_key",
        }
    }

    /// Every variant, for callers that enumerate and for the test that reconciles this
    /// vocabulary against the migration's `CHECK`.
    pub const ALL: [Self; 4] = [
        Self::Register,
        Self::Authenticate,
        Self::RegisterSecurityKey,
        Self::AuthenticateSecurityKey,
    ];
}

/// A ceremony recovered from the store, ready to be handed back to `webauthn-rs`.
#[derive(Debug, Clone)]
pub struct Ceremony {
    /// `None` for a discoverable sign-in, which does not know who is signing in yet.
    pub user_id: Option<UserId>,
    /// The serialised registration or authentication state.
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
    use super::{CeremonyKind, CredentialPurpose};

    /// Every persisted vocabulary word appears in a `CHECK` constraint somewhere in the
    /// migration set.
    ///
    /// Bug this pins: renaming a `CeremonyKind` string without updating the CHECK makes every
    /// `INSERT` fail and every `take_ceremony` silently read as "expired" — a sign-in that
    /// stops working with no error naming the cause. `CredentialPurpose` was added to the same
    /// test because it has the identical failure mode, and a worse one: a purpose string the
    /// CHECK rejects makes registration fail, while a purpose string that *reads* differently
    /// from what was written makes every scoped read return nothing, which the account page
    /// renders as "you have no passkeys".
    #[test]
    fn the_persisted_vocabulary_matches_the_check_constraints() {
        const PASSKEYS: &str = include_str!("../../../../../migrations/0022_passkeys.up.sql");
        const MFA: &str = include_str!("../../../../../migrations/0043_multi_factor_auth.up.sql");

        for kind in CeremonyKind::ALL {
            assert!(
                PASSKEYS.contains(&format!("'{}'", kind.as_str()))
                    || MFA.contains(&format!("'{}'", kind.as_str())),
                "`{}` is not in any migration's CHECK vocabulary",
                kind.as_str()
            );
        }

        for purpose in [CredentialPurpose::Passkey, CredentialPurpose::SecurityKey] {
            assert!(
                MFA.contains(&format!("'{}'", purpose.as_str())),
                "`{}` is not in the migration's CHECK vocabulary",
                purpose.as_str()
            );
        }
    }
}
