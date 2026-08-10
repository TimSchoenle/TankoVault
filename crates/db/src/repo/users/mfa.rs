//! Second-factor state: TOTP enrolment, recovery codes, and the two short-lived grants the
//! flows issue — a pending sign-in and a step-up elevation.
//!
//! Security keys live in [`super::webauthn`], because they are `WebAuthn` credentials sharing a
//! table with passkeys. [`is_enrolled`] is the one place that answers "does this account have a
//! second factor at all" across both.
//!
//! Every secret here arrives already transformed: the TOTP secret is sealed ciphertext, every
//! handle and recovery code is a digest. This layer stores bytes and strings and holds no key.

use crate::error::DbResult;
use sqlx::{FromRow, PgExecutor};
use tankovault_domain::UserId;
use time::OffsetDateTime;
use uuid::Uuid;

// ---------------------------------------------------------------------------
// TOTP
// ---------------------------------------------------------------------------

/// A TOTP enrolment as the verifier needs it.
#[derive(Debug, Clone)]
pub struct TotpEnrolment {
    /// Sealed secret — `nonce || ciphertext-with-tag`, opened by the caller's `Sealer`.
    pub secret: Vec<u8>,
    /// `None` while the user has been shown the secret but has not yet proved they stored it.
    pub confirmed_at: Option<OffsetDateTime>,
    /// The last RFC 6238 step this secret was accepted at; the replay floor.
    pub last_step: Option<i64>,
}

/// Read the caller's TOTP enrolment, confirmed or not.
///
/// # Errors
/// [`crate::DbError::Sqlx`] only. No enrolment is `Ok(None)`.
pub async fn get_totp<'e, E: PgExecutor<'e>>(
    exec: E,
    user_id: UserId,
) -> DbResult<Option<TotpEnrolment>> {
    let row = sqlx::query_as!(
        TotpEnrolment,
        "SELECT secret, confirmed_at, last_step FROM user_totp WHERE user_id = $1",
        user_id.as_uuid(),
    )
    .fetch_optional(exec)
    .await?;
    Ok(row)
}

/// Begin (or restart) enrolment: store a freshly sealed secret, unconfirmed.
///
/// Overwrites any *existing* row, which is what restarting enrolment means — the user asked for
/// a new QR code because the old secret never reached their phone. The upsert is guarded so it
/// cannot silently replace a **confirmed** enrolment: swapping a working second factor for one
/// the caller has not yet proved possession of is how a session-stealing attacker would lock the
/// owner out. Returns `false` when the guard refused.
///
/// # Errors
/// [`crate::DbError::Sqlx`] only.
pub async fn begin_totp_enrolment<'e, E: PgExecutor<'e>>(
    exec: E,
    user_id: UserId,
    secret: &[u8],
    label: &str,
) -> DbResult<bool> {
    let result = sqlx::query!(
        "INSERT INTO user_totp (user_id, secret, label) VALUES ($1,$2,$3) \
         ON CONFLICT (user_id) DO UPDATE \
           SET secret = EXCLUDED.secret, label = EXCLUDED.label, \
               last_step = NULL, created_at = now() \
         WHERE user_totp.confirmed_at IS NULL",
        user_id.as_uuid(),
        secret,
        label,
    )
    .execute(exec)
    .await?;
    Ok(result.rows_affected() > 0)
}

/// Confirm an enrolment, stamping the step the proving code was accepted at.
///
/// Guarded on `confirmed_at IS NULL` so a replayed confirmation cannot reset `last_step` and
/// re-open the replay window on an already-live enrolment. Returns rows changed.
///
/// # Errors
/// [`crate::DbError::Sqlx`] only.
pub async fn confirm_totp<'e, E: PgExecutor<'e>>(
    exec: E,
    user_id: UserId,
    accepted_step: i64,
) -> DbResult<u64> {
    let result = sqlx::query!(
        "UPDATE user_totp SET confirmed_at = now(), last_step = $2 \
         WHERE user_id = $1 AND confirmed_at IS NULL",
        user_id.as_uuid(),
        accepted_step,
    )
    .execute(exec)
    .await?;
    Ok(result.rows_affected())
}

/// Advance the replay floor after a successful verification.
///
/// `last_step < $2` is the whole guard: two requests carrying the same code can race here, and
/// without it the later one would happily write a step it already lost the race for. With it,
/// exactly one advances and the other's own verification has already been checked against the
/// value it read. Returns rows changed — `0` means another request got there first.
///
/// # Errors
/// [`crate::DbError::Sqlx`] only.
pub async fn advance_totp_step<'e, E: PgExecutor<'e>>(
    exec: E,
    user_id: UserId,
    step: i64,
) -> DbResult<u64> {
    let result = sqlx::query!(
        "UPDATE user_totp SET last_step = $2 \
         WHERE user_id = $1 AND (last_step IS NULL OR last_step < $2)",
        user_id.as_uuid(),
        step,
    )
    .execute(exec)
    .await?;
    Ok(result.rows_affected())
}

/// Remove the caller's TOTP enrolment. Returns rows removed.
///
/// # Errors
/// [`crate::DbError::Sqlx`] only. No enrolment is `Ok(0)`.
pub async fn delete_totp<'e, E: PgExecutor<'e>>(exec: E, user_id: UserId) -> DbResult<u64> {
    let result = sqlx::query!(
        "DELETE FROM user_totp WHERE user_id = $1",
        user_id.as_uuid()
    )
    .execute(exec)
    .await?;
    Ok(result.rows_affected())
}

/// Whether this account holds any usable second factor.
///
/// The single definition of "enrolled", used by the passkey gate and by the privileged-account
/// requirement. A *confirmed* TOTP row or at least one security key; an unconfirmed TOTP row is
/// not a factor, because nothing has proved the user can produce a code from it.
///
/// # Errors
/// [`crate::DbError::Sqlx`] only.
pub async fn is_enrolled<'e, E: PgExecutor<'e>>(exec: E, user_id: UserId) -> DbResult<bool> {
    let row = sqlx::query!(
        "SELECT (EXISTS (SELECT 1 FROM user_totp \
                          WHERE user_id = $1 AND confirmed_at IS NOT NULL) \
                 OR EXISTS (SELECT 1 FROM user_webauthn_credentials \
                             WHERE user_id = $1 AND purpose = 'security_key')) AS \"enrolled!\"",
        user_id.as_uuid(),
    )
    .fetch_one(exec)
    .await?;
    Ok(row.enrolled)
}

// ---------------------------------------------------------------------------
// Recovery codes
// ---------------------------------------------------------------------------

/// Replace the caller's recovery-code set with `hashes`, returning how many were stored.
///
/// Replace, never append: a set is issued and displayed as a set, and appending would leave the
/// user holding two printouts with no way to tell which codes are still live.
///
/// # Errors
/// [`crate::DbError::Sqlx`] only. Rolls back on first failure, so a partial set is never stored
/// — a user shown ten codes of which six were saved would find four "wrong" ones in a lockout.
pub async fn replace_recovery_codes(
    conn: &mut sqlx::PgConnection,
    user_id: UserId,
    hashes: &[String],
) -> DbResult<u64> {
    use sqlx::Connection as _;
    let mut tx = conn.begin().await?;

    sqlx::query!(
        "DELETE FROM user_recovery_codes WHERE user_id = $1",
        user_id.as_uuid(),
    )
    .execute(&mut *tx)
    .await?;

    let result = sqlx::query!(
        "INSERT INTO user_recovery_codes (user_id, code_hash) \
         SELECT $1, hash FROM unnest($2::text[]) AS hash",
        user_id.as_uuid(),
        hashes,
    )
    .execute(&mut *tx)
    .await?;

    tx.commit().await?;
    Ok(result.rows_affected())
}

/// Consume one unused recovery code, if `code_hash` matches one of this user's.
///
/// A single `UPDATE … RETURNING` guarded on `used_at IS NULL`, so two requests presenting the
/// same code cannot both succeed: the row-level lock serialises them and the loser's guard no
/// longer holds. Returns `true` when a code was consumed.
///
/// # Errors
/// [`crate::DbError::Sqlx`] only. A wrong or already-used code is `Ok(false)`.
pub async fn consume_recovery_code<'e, E: PgExecutor<'e>>(
    exec: E,
    user_id: UserId,
    code_hash: &str,
) -> DbResult<bool> {
    let result = sqlx::query!(
        "UPDATE user_recovery_codes SET used_at = now() \
         WHERE user_id = $1 AND code_hash = $2 AND used_at IS NULL",
        user_id.as_uuid(),
        code_hash,
    )
    .execute(exec)
    .await?;
    Ok(result.rows_affected() > 0)
}

/// How many of the caller's recovery codes remain unused.
///
/// # Errors
/// [`crate::DbError::Sqlx`] only.
pub async fn recovery_codes_remaining<'e, E: PgExecutor<'e>>(
    exec: E,
    user_id: UserId,
) -> DbResult<i64> {
    let count = sqlx::query_scalar!(
        "SELECT count(*) AS \"count!\" FROM user_recovery_codes \
         WHERE user_id = $1 AND used_at IS NULL",
        user_id.as_uuid(),
    )
    .fetch_one(exec)
    .await?;
    Ok(count)
}

// ---------------------------------------------------------------------------
// Pending sign-in
// ---------------------------------------------------------------------------

/// A pending sign-in, resolved from the handle its owner holds.
#[derive(Debug, Clone, FromRow)]
pub struct PendingChallenge {
    pub id: Uuid,
    pub user_id: UserId,
    /// How many factors have already been presented against this challenge.
    pub attempts: i32,
}

/// Open a pending sign-in for `user_id`, storing only the handle's digest.
///
/// # Errors
/// [`crate::DbError::Sqlx`] only.
pub async fn insert_challenge<'e, E: PgExecutor<'e>>(
    exec: E,
    user_id: UserId,
    token_hash: &str,
    expires_at: OffsetDateTime,
) -> DbResult<Uuid> {
    let id = sqlx::query_scalar!(
        "INSERT INTO mfa_challenges (user_id, token_hash, expires_at) VALUES ($1,$2,$3) \
         RETURNING id",
        user_id.as_uuid(),
        token_hash,
        expires_at,
    )
    .fetch_one(exec)
    .await?;
    Ok(id)
}

/// Resolve a live pending sign-in from its handle's digest, **charging an attempt**.
///
/// The increment is part of the read, not a separate call, and that is the point: a caller who
/// resolves a challenge and then abandons the request — because the code was wrong, because the
/// connection dropped, because they are scripting it — has still spent a guess. A separate
/// "count this failure" call is one an error path can skip, and the six-digit code it protects
/// only holds up if every guess is counted.
///
/// Expired or unknown is `Ok(None)`.
///
/// # Errors
/// [`crate::DbError::Sqlx`] only.
pub async fn charge_challenge_attempt<'e, E: PgExecutor<'e>>(
    exec: E,
    token_hash: &str,
) -> DbResult<Option<PendingChallenge>> {
    #[derive(FromRow)]
    struct Row {
        id: Uuid,
        user_id: Uuid,
        attempts: i32,
    }
    let row = sqlx::query_as!(
        Row,
        "UPDATE mfa_challenges SET attempts = attempts + 1 \
         WHERE token_hash = $1 AND expires_at > now() \
         RETURNING id, user_id, attempts",
        token_hash,
    )
    .fetch_optional(exec)
    .await?;
    Ok(row.map(|r| PendingChallenge {
        id: r.id,
        user_id: UserId::from_uuid(r.user_id),
        attempts: r.attempts,
    }))
}

/// Delete a pending sign-in — on success, or once its attempts are spent.
///
/// # Errors
/// [`crate::DbError::Sqlx`] only.
pub async fn delete_challenge<'e, E: PgExecutor<'e>>(exec: E, id: Uuid) -> DbResult<u64> {
    let result = sqlx::query!("DELETE FROM mfa_challenges WHERE id = $1", id)
        .execute(exec)
        .await?;
    Ok(result.rows_affected())
}

/// Delete every pending sign-in whose deadline has passed, returning the count removed.
///
/// Reclaims space only; [`charge_challenge_attempt`] already filters on expiry.
///
/// # Errors
/// [`crate::DbError::Sqlx`] only.
pub async fn prune_expired_challenges<'e, E: PgExecutor<'e>>(exec: E) -> DbResult<u64> {
    let result = sqlx::query!("DELETE FROM mfa_challenges WHERE expires_at <= now()")
        .execute(exec)
        .await?;
    Ok(result.rows_affected())
}

// ---------------------------------------------------------------------------
// Step-up grants
// ---------------------------------------------------------------------------

/// Which factor produced a step-up grant.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StepUpMethod {
    Totp,
    SecurityKey,
    RecoveryCode,
    /// The fallback for an account with no factor enrolled at all — refused once one exists.
    Password,
}

impl StepUpMethod {
    /// The persisted string. Stable — it is a `CHECK` constraint's vocabulary.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Totp => "totp",
            Self::SecurityKey => "security_key",
            Self::RecoveryCode => "recovery_code",
            Self::Password => "password",
        }
    }

    /// Every variant, for callers that enumerate and for the test that reconciles this
    /// vocabulary against the migration's `CHECK`.
    pub const ALL: [Self; 4] = [
        Self::Totp,
        Self::SecurityKey,
        Self::RecoveryCode,
        Self::Password,
    ];
}

/// Record a step-up elevation, storing only the token's digest.
///
/// # Errors
/// [`crate::DbError::Sqlx`] only.
pub async fn insert_step_up<'e, E: PgExecutor<'e>>(
    exec: E,
    user_id: UserId,
    token_hash: &str,
    method: StepUpMethod,
    expires_at: OffsetDateTime,
) -> DbResult<()> {
    sqlx::query!(
        "INSERT INTO step_up_grants (user_id, token_hash, method, expires_at) \
         VALUES ($1,$2,$3,$4)",
        user_id.as_uuid(),
        token_hash,
        method.as_str(),
        expires_at,
    )
    .execute(exec)
    .await?;
    Ok(())
}

/// Resolve a live step-up grant belonging to `user_id`, sliding its window forward.
///
/// Scoped to the user in the statement, not checked afterwards: a grant is bound to the account
/// that earned it, and a token presented alongside a *different* account's access token must
/// find nothing rather than find a row someone then forgets to compare.
///
/// Two deadlines, and both are load-bearing. `idle_until` is where a *used* grant's expiry moves
/// to, so an operator working through a console panel is asked once rather than every few
/// minutes; `alive_since` is the floor on `created_at`, which no amount of use can push, so the
/// sliding window cannot turn one confirmation this morning into a standing elevation this
/// evening. A grant past the floor is refused here whatever its `expires_at` says — the write is
/// an optimisation, the `WHERE` is the rule.
///
/// # Errors
/// [`crate::DbError::Sqlx`] only. Unknown, expired, revoked or past its absolute lifetime is
/// `Ok(None)`.
pub async fn renew_step_up<'e, E: PgExecutor<'e>>(
    exec: E,
    user_id: UserId,
    token_hash: &str,
    idle_until: OffsetDateTime,
    alive_since: OffsetDateTime,
) -> DbResult<Option<StepUpMethod>> {
    // `GREATEST` so a resolve never *shortens* a window someone else set; `LEAST` against the
    // absolute deadline (`created_at` plus the same span `alive_since` is behind `now()`) so the
    // stored expiry stays the grant's real death and the sweeper still collects it on time.
    let method = sqlx::query_scalar!(
        "UPDATE step_up_grants \
         SET expires_at = LEAST(GREATEST(expires_at, $3), created_at + (now() - $4)) \
         WHERE user_id = $1 AND token_hash = $2 \
           AND revoked_at IS NULL AND expires_at > now() AND created_at > $4 \
         RETURNING method",
        user_id.as_uuid(),
        token_hash,
        idle_until,
        alive_since,
    )
    .fetch_optional(exec)
    .await?;

    Ok(method.and_then(|m| match m.as_str() {
        "totp" => Some(StepUpMethod::Totp),
        "security_key" => Some(StepUpMethod::SecurityKey),
        "recovery_code" => Some(StepUpMethod::RecoveryCode),
        "password" => Some(StepUpMethod::Password),
        // Unreachable behind the CHECK constraint; fail closed rather than panic on a value a
        // future migration widened the vocabulary with.
        other => {
            tracing::warn!(method = %other, "ignoring step-up grant with an unknown method");
            None
        }
    }))
}

/// Revoke every live step-up grant this user holds, returning the count.
///
/// Called on password change, on sign-out, and whenever a factor is removed: an elevation
/// outliving the credential that earned it is an elevation the account's owner cannot end.
///
/// # Errors
/// [`crate::DbError::Sqlx`] only. Nothing live is `Ok(0)`.
pub async fn revoke_step_ups<'e, E: PgExecutor<'e>>(exec: E, user_id: UserId) -> DbResult<u64> {
    let result = sqlx::query!(
        "UPDATE step_up_grants SET revoked_at = now() \
         WHERE user_id = $1 AND revoked_at IS NULL",
        user_id.as_uuid(),
    )
    .execute(exec)
    .await?;
    Ok(result.rows_affected())
}

/// Delete every step-up grant past its deadline, returning the count removed.
///
/// # Errors
/// [`crate::DbError::Sqlx`] only.
pub async fn prune_expired_step_ups<'e, E: PgExecutor<'e>>(exec: E) -> DbResult<u64> {
    let result = sqlx::query!("DELETE FROM step_up_grants WHERE expires_at <= now()")
        .execute(exec)
        .await?;
    Ok(result.rows_affected())
}

#[cfg(test)]
mod tests {
    use super::StepUpMethod;

    /// Bug this pins: a method string the `CHECK` constraint does not know makes every step-up
    /// `INSERT` fail — so every sensitive action refuses, with the error surfacing as a 500 from
    /// the *elevation* endpoint rather than anything naming the vocabulary.
    #[test]
    fn the_persisted_methods_match_the_check_constraint() {
        const MIGRATION: &str =
            include_str!("../../../../../migrations/0043_multi_factor_auth.up.sql");
        for method in StepUpMethod::ALL {
            assert!(
                MIGRATION.contains(&format!("'{}'", method.as_str())),
                "`{}` is not in the migration's CHECK vocabulary",
                method.as_str()
            );
        }
    }
}
