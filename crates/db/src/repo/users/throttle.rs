//! Per-account failure counts for credential checks, and the lock they trip.
//!
//! The policy (how many failures, how long a lock) belongs to the caller; this layer only
//! counts, locks and clears, each in one statement so concurrent guesses are all counted.

use crate::error::DbResult;
use sqlx::PgExecutor;
use tankovault_domain::UserId;

/// Which credential check a count belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ThrottleKind {
    /// The account password, at sign-in or step-up.
    Password,
    /// A TOTP or recovery code, at the second sign-in leg or step-up.
    SecondFactor,
}

impl ThrottleKind {
    /// Returns the persisted string, stable because it is a `CHECK` constraint's vocabulary.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Password => "password",
            Self::SecondFactor => "second_factor",
        }
    }

    /// Every variant, for the test that reconciles this vocabulary against the migration.
    pub const ALL: [Self; 2] = [Self::Password, Self::SecondFactor];
}

/// How a caller wants failures counted and locks sized.
#[derive(Debug, Clone, Copy)]
pub struct ThrottlePolicy {
    /// Failures inside one window that trip a lock.
    pub max_failures: i32,
    /// Length of the counting window, in seconds.
    pub window_secs: f64,
    /// Length of the first lock, in seconds; each further lock doubles it.
    pub base_lock_secs: f64,
    /// How many doublings the lock may grow by, so a victim cannot be locked out indefinitely.
    pub max_doublings: i32,
}

/// Returns whether `kind` is locked for `user_id` right now.
///
/// # Errors
/// [`crate::DbError::Sqlx`] only.
pub async fn is_locked<'e, E: PgExecutor<'e>>(
    exec: E,
    user_id: UserId,
    kind: ThrottleKind,
) -> DbResult<bool> {
    let locked = sqlx::query_scalar!(
        "SELECT EXISTS (SELECT 1 FROM auth_throttle \
                        WHERE user_id = $1 AND kind = $2 AND locked_until > now()) AS \"locked!\"",
        user_id.as_uuid(),
        kind.as_str(),
    )
    .fetch_one(exec)
    .await?;
    Ok(locked)
}

/// Counts one failure of `kind` against `user_id`, locking the check once the policy's limit is
/// reached inside its window. Returns whether this failure tripped the lock.
///
/// The lock's `failures >= max_failures` guard means two failures racing past the limit lock
/// once: the first resets the count the second would have matched.
///
/// # Errors
/// [`crate::DbError::Sqlx`] only.
pub async fn record_failure(
    pool: &sqlx::PgPool,
    user_id: UserId,
    kind: ThrottleKind,
    policy: ThrottlePolicy,
) -> DbResult<bool> {
    let failures = sqlx::query_scalar!(
        "INSERT INTO auth_throttle AS t (user_id, kind, failures, window_started_at) \
         VALUES ($1, $2, 1, now()) \
         ON CONFLICT (user_id, kind) DO UPDATE SET \
           failures = CASE WHEN t.window_started_at <= now() - make_interval(secs => $3) \
                           THEN 1 ELSE t.failures + 1 END, \
           window_started_at = CASE WHEN t.window_started_at <= now() - make_interval(secs => $3) \
                                    THEN now() ELSE t.window_started_at END \
         RETURNING failures",
        user_id.as_uuid(),
        kind.as_str(),
        policy.window_secs,
    )
    .fetch_one(pool)
    .await?;
    if failures < policy.max_failures {
        return Ok(false);
    }

    let locked = sqlx::query!(
        "UPDATE auth_throttle \
         SET locked_until = now() + make_interval( \
               secs => $3 * power(2, least(lockouts, $4))::float8), \
             lockouts = lockouts + 1, failures = 0, window_started_at = now() \
         WHERE user_id = $1 AND kind = $2 AND failures >= $5",
        user_id.as_uuid(),
        kind.as_str(),
        policy.base_lock_secs,
        policy.max_doublings,
        policy.max_failures,
    )
    .execute(pool)
    .await?;
    Ok(locked.rows_affected() > 0)
}

/// Forgets every count of `kinds` for `user_id`, after the check succeeded.
///
/// # Errors
/// [`crate::DbError::Sqlx`] only.
pub async fn clear<'e, E: PgExecutor<'e>>(
    exec: E,
    user_id: UserId,
    kinds: &[ThrottleKind],
) -> DbResult<u64> {
    let kinds: Vec<String> = kinds.iter().map(|k| k.as_str().to_owned()).collect();
    let result = sqlx::query!(
        "DELETE FROM auth_throttle WHERE user_id = $1 AND kind = ANY($2)",
        user_id.as_uuid(),
        &kinds,
    )
    .execute(exec)
    .await?;
    Ok(result.rows_affected())
}

#[cfg(test)]
mod tests {
    use super::ThrottleKind;

    /// Bug this pins: a kind the `CHECK` constraint does not know makes every failed sign-in
    /// fail to record, surfacing as a 500 on a wrong password.
    #[test]
    fn the_persisted_kinds_match_the_check_constraint() {
        const MIGRATION: &str = include_str!("../../../../../migrations/0070_auth_throttle.up.sql");
        for kind in ThrottleKind::ALL {
            assert!(
                MIGRATION.contains(&format!("'{}'", kind.as_str())),
                "`{}` is not in the migration's CHECK vocabulary",
                kind.as_str()
            );
        }
    }
}
