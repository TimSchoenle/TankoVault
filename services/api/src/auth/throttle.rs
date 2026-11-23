//! Owns the per-account limit on credential guessing: password at sign-in and step-up, and the
//! second factor at the sign-in second leg and step-up.
//!
//! The IP-keyed rate limit cannot bound a guessing campaign spread across many addresses; this
//! counts failures against the account instead. A locked check answers exactly as a wrong
//! credential does, so the lock reveals nothing about the account to whoever tripped it.

use tankovault_db::repo::users::throttle::{self, ThrottleKind, ThrottlePolicy};
use tankovault_domain::UserId;

use crate::error::ApiResult;
use crate::state::AppState;

/// Ten wrong passwords in fifteen minutes lock the password for fifteen minutes, doubling to an
/// hour. The cap matters: anyone can trip this lock on someone else's account.
const PASSWORD: ThrottlePolicy = ThrottlePolicy {
    max_failures: 10,
    window_secs: 900.0,
    base_lock_secs: 900.0,
    max_doublings: 2,
};

/// Tighter than the password: whoever reaches the second factor already holds the password or a
/// session, and a six-digit code has far less entropy.
const SECOND_FACTOR: ThrottlePolicy = ThrottlePolicy {
    max_failures: 5,
    window_secs: 900.0,
    base_lock_secs: 900.0,
    max_doublings: 2,
};

const fn policy(kind: ThrottleKind) -> ThrottlePolicy {
    match kind {
        ThrottleKind::Password => PASSWORD,
        ThrottleKind::SecondFactor => SECOND_FACTOR,
    }
}

/// Returns whether `kind` is locked for `user_id`.
///
/// # Errors
/// Returns [`crate::error::ApiError::Internal`] on a database failure.
pub(crate) async fn is_locked(
    state: &AppState,
    user_id: UserId,
    kind: ThrottleKind,
) -> ApiResult<bool> {
    Ok(throttle::is_locked(&state.pool, user_id, kind).await?)
}

/// Counts one failed `kind` check against `user_id`.
///
/// # Errors
/// Returns [`crate::error::ApiError::Internal`] on a database failure: a guess that could not be
/// counted is refused, not let through.
pub(crate) async fn record_failure(
    state: &AppState,
    user_id: UserId,
    kind: ThrottleKind,
) -> ApiResult<()> {
    if throttle::record_failure(&state.pool, user_id, kind, policy(kind)).await? {
        tracing::warn!(
            user_id = %user_id.as_uuid(),
            kind = kind.as_str(),
            "credential check locked after repeated failures"
        );
    }
    Ok(())
}

/// Forgets the failures of `kinds` once the check succeeded.
///
/// # Errors
/// Returns [`crate::error::ApiError::Internal`] on a database failure.
pub(crate) async fn clear(
    state: &AppState,
    user_id: UserId,
    kinds: &[ThrottleKind],
) -> ApiResult<()> {
    throttle::clear(&state.pool, user_id, kinds).await?;
    Ok(())
}
