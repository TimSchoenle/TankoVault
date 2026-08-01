//! Passkeys (`WebAuthn` credentials) and the short-lived ceremony state that registers or
//! asserts them.
//!
//! This module stores two opaque JSON documents and never inspects either. The credential and
//! the ceremony state are `webauthn-rs` types; their internal shape is that library's to
//! evolve, and a repository that reached into them would be asserting a schema it does not
//! own. What this layer *does* own is the lookup keys — the credential id and the user handle
//! — and the ownership scoping on every read and write, which is the part a caller can get
//! wrong silently.
//!
//! Every write here is scoped to a user id in the statement itself rather than checked by the
//! caller, for the reason [`super::sessions`] gives: a rename or a delete that forgot the
//! scope would let anyone edit anyone's credentials with no visible symptom.

use crate::error::{DbError, DbResult};
use sqlx::{FromRow, PgExecutor};
use tankovault_domain::UserId;
use time::OffsetDateTime;
use uuid::Uuid;

/// A registered passkey, as the account page and the authenticator both need it.
///
/// `credential` is the serialised `webauthn_rs::prelude::Passkey`; the caller deserialises it.
/// It is carried alongside the presentation fields rather than fetched separately because the
/// sign-in path needs the credential and the last-used timestamp in one round trip.
#[derive(Debug, Clone)]
pub struct PasskeyRecord {
    pub id: Uuid,
    pub user_id: UserId,
    pub credential_id: Vec<u8>,
    /// Serialised `webauthn_rs::prelude::Passkey`. Opaque here — see the module docs.
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
/// [`DbError::Sqlx`] only — no other variant is reachable. A user with no passkeys gets an
/// empty `Vec`, not [`DbError::NotFound`]: having none is the ordinary state of most accounts
/// and is what the account page renders as "no keys yet".
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

/// The credential ids already registered to a user.
///
/// Fed to `start_passkey_registration` as its exclude list, which is what makes an
/// authenticator that already holds a key for this account say so *at the prompt* instead of
/// silently minting a second one. Without it a user who taps "add a passkey" twice on the same
/// phone ends up with two indistinguishable rows.
///
/// # Errors
/// [`DbError::Sqlx`] only — no other variant is reachable. No passkeys is `Ok(vec![])`.
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

/// One passkey by the credential id the authenticator presented.
///
/// The sign-in lookup. Deliberately **not** scoped to a user: a discoverable assertion is
/// resolved *from* the credential, so there is no user id to scope by yet. The caller must
/// then check that the returned record's `user_id` matches the user handle the authenticator
/// also returned — the two are independent claims and only agreeing makes either trustworthy.
///
/// # Errors
/// [`DbError::Sqlx`] only — no other variant is reachable. An unknown credential id is
/// `Ok(None)`, not [`DbError::NotFound`]: an unregistered credential and a wrong signature must
/// be answered identically, and a distinct error variant here is how that distinction leaks out.
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
/// [`DbError::Conflict`] if this credential id is already registered — to *any* account, not
/// only this one. See `0022_passkeys.up.sql` for why the constraint is global: an authenticator
/// resolving to two accounts is ambiguous, and letting an attacker claim an observed credential
/// id is an account-takeover primitive. [`DbError::Sqlx`] otherwise.
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

/// Record a successful assertion: stamp `last_used_at` and store the credential the library
/// handed back.
///
/// Both halves in one statement because they are one event. The credential is rewritten
/// wholesale rather than patched, since what changed inside it (the signature counter, a
/// backup-state flag) is the library's business — `Passkey::update_credential` has already
/// decided, and this layer only persists the result.
///
/// # Errors
/// [`DbError::Sqlx`] only — no other variant is reachable. A credential id that no longer
/// exists (a key deleted between the assertion and this write) is `Ok(())`: the sign-in it
/// belongs to has already succeeded, and failing it here would sign the user out over a
/// bookkeeping race.
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

/// Rename one of the caller's own passkeys. Returns the number of rows changed (0 if the id
/// was not theirs).
///
/// # Errors
/// [`DbError::Sqlx`] only — no other variant is reachable. An id belonging to another user is
/// `Ok(0)`, not [`DbError::NotFound`]: the `user_id` predicate makes "not yours" and "does not
/// exist" indistinguishable on purpose, so probing ids cannot enumerate other people's keys.
/// The caller decides whether 0 means 404.
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

/// Delete one of the caller's own passkeys. Returns the number of rows removed.
///
/// # Errors
/// [`DbError::Sqlx`] only — no other variant is reachable. See [`rename`] for why a foreign id
/// is `Ok(0)` rather than an error.
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

/// Which leg of the protocol a stored ceremony belongs to.
///
/// The kinds are kept apart so a challenge issued for one purpose cannot complete the other.
/// Without the discriminator, a registration challenge — which any signed-in user can mint —
/// would be a valid input to the sign-in `finish`, and vice versa. The database enforces the
/// vocabulary too (`webauthn_ceremony_kind`), so a typo in a query cannot write a third value.
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
    /// The account that started it — `None` for a discoverable sign-in, which does not know
    /// who is signing in until the authenticator answers.
    pub user_id: Option<UserId>,
    /// The serialised `PasskeyRegistration` / `DiscoverableAuthentication`.
    pub state: serde_json::Value,
}

/// Store an in-flight ceremony under a caller-chosen id, to expire at `expires_at`.
///
/// # Errors
/// [`DbError::Sqlx`] only — no other variant is reachable. The id is minted by the caller and
/// is random, so a primary-key collision is not a case worth modelling separately: it would
/// arrive as `Sqlx` and be a 500, which is the correct answer to a UUID collision.
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

/// Consume a ceremony: fetch it and delete it in one statement.
///
/// **The delete is the security property, not a cleanup.** A challenge that survives its use is
/// a challenge that can be replayed, so this is a `DELETE ... RETURNING` rather than a `SELECT`
/// followed by a delete the caller might skip on an early return — and there are several early
/// returns on the path that calls this, one per way a response can fail verification.
///
/// The expiry is applied in the same statement for the same reason: a caller comparing
/// timestamps after the fact is a caller who can forget to.
///
/// # Errors
/// [`DbError::Sqlx`] only — no other variant is reachable. An unknown, already-consumed,
/// expired, or wrong-`kind` id is `Ok(None)`: those are four ways of saying "this challenge is
/// not live", they must be answered identically, and telling them apart would let a client
/// probe which ceremony ids exist.
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

/// Delete every ceremony whose deadline has passed, returning how many were removed.
///
/// The abandoned-ceremony sweep. Expired rows are already unusable — [`take_ceremony`] filters
/// on `expires_at` — so this reclaims space rather than enforcing anything, and the common case
/// it reclaims is a user who closed the browser at the authenticator prompt.
///
/// # Errors
/// [`DbError::Sqlx`] only — no other variant is reachable. Nothing to delete is `Ok(0)`.
pub async fn prune_expired_ceremonies<'e, E: PgExecutor<'e>>(exec: E) -> DbResult<u64> {
    let result = sqlx::query!("DELETE FROM webauthn_ceremonies WHERE expires_at <= now()")
        .execute(exec)
        .await?;
    Ok(result.rows_affected())
}

#[cfg(test)]
mod tests {
    use super::CeremonyKind;

    /// The persisted strings are the `webauthn_ceremony_kind` CHECK constraint's vocabulary.
    ///
    /// Renaming a variant is free; renaming its *string* makes every `INSERT` fail with a
    /// constraint violation and every `take_ceremony` return `None` — which the API reports as
    /// an expired challenge, so registration and sign-in would both simply stop working with a
    /// plausible-looking message and nothing pointing at the cause.
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
