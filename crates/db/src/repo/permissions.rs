//! Permission grants — the persistence behind [`tankovault_domain::Permission`]. Resolved from
//! here on every authenticated request rather than cached in an access token, so a revocation
//! takes effect immediately.
//!
//! Unrecognised stored tokens are dropped, not rejected — see
//! [`tankovault_domain::PermissionSet::from_tokens`].

use crate::error::DbResult;
use sqlx::{Connection as _, FromRow, PgExecutor};
use tankovault_domain::{AccountStatus, Permission, PermissionSet, UserId};
use time::OffsetDateTime;
use uuid::Uuid;

/// Everything the authorization layer needs about a principal, in one read.
#[derive(Debug, Clone)]
pub struct Principal {
    pub status: AccountStatus,
    pub permissions: PermissionSet,
}

/// Resolve a principal's account status and permission grants in one round trip (`LEFT JOIN`,
/// so a no-grants account still yields a row).
///
/// # Errors
/// `Sqlx` only. `Ok(None)` means the account is gone — callers must fail closed on both `Err`
/// and `Ok(None)`, not treat a deleted account as a principal with no permissions.
pub async fn resolve<'e, E: PgExecutor<'e>>(
    exec: E,
    user_id: UserId,
) -> DbResult<Option<Principal>> {
    #[derive(FromRow)]
    struct Row {
        status: AccountStatus,
        permissions: Vec<String>,
    }
    let row = sqlx::query_as!(
        Row,
        "SELECT u.status AS \"status: AccountStatus\", \
                coalesce(array_agg(p.permission) FILTER (WHERE p.permission IS NOT NULL), \
                         '{}'::text[]) AS \"permissions!\" \
         FROM users u \
         LEFT JOIN user_permissions p ON p.user_id = u.id \
         WHERE u.id = $1 \
         GROUP BY u.status",
        user_id.as_uuid(),
    )
    .fetch_optional(exec)
    .await?;

    Ok(row.map(|r| Principal {
        status: r.status,
        permissions: PermissionSet::from_tokens(&r.permissions, |token| {
            // Logged, not silently dropped — surfaces schema/binary drift.
            tracing::warn!(%token, user_id = %user_id.as_uuid(), "ignoring unknown permission grant");
        }),
    }))
}

/// A single grant, with its provenance, for the user-detail view.
#[derive(Debug, Clone, serde::Serialize)]
pub struct GrantRow {
    /// String, not the enum — a grant from a build with a capability this one lacks must stay
    /// visible to an admin.
    pub permission: String,
    /// Whether this build recognises the token. `false` means the grant is inert.
    pub known: bool,
    #[serde(with = "time::serde::rfc3339")]
    pub granted_at: OffsetDateTime,
    /// `None` for a migration-era grant or an erased administrator.
    pub granted_by: Option<String>,
}

/// List a user's grants with provenance, newest first.
///
/// # Errors
/// `Sqlx` only; unknown user or no grants is `Ok(vec![])`. An unparseable token comes back
/// `known: false`, not an error.
pub async fn list_for_user<'e, E: PgExecutor<'e>>(
    exec: E,
    user_id: UserId,
) -> DbResult<Vec<GrantRow>> {
    #[derive(FromRow)]
    struct Row {
        permission: String,
        granted_at: OffsetDateTime,
        granted_by: Option<String>,
    }
    let rows = sqlx::query_as!(
        Row,
        "SELECT p.permission, p.granted_at, g.username AS \"granted_by?: String\" \
         FROM user_permissions p \
         LEFT JOIN users g ON g.id = p.granted_by \
         WHERE p.user_id = $1 \
         ORDER BY p.granted_at DESC, p.permission",
        user_id.as_uuid(),
    )
    .fetch_all(exec)
    .await?;

    Ok(rows
        .into_iter()
        .map(|r| GrantRow {
            known: r.permission.parse::<Permission>().is_ok(),
            permission: r.permission,
            granted_at: r.granted_at,
            granted_by: r.granted_by,
        })
        .collect())
}

/// Replace a user's entire grant set in one transaction, returning what changed.
///
/// Whole-set diff, not add/remove calls, so a UI checklist submit can't interleave with
/// itself. Unchanged grants keep their `granted_at`.
///
/// # Errors
/// `Sqlx` only; rolls back on first failure. A concurrent identical grant is absorbed
/// (`ON CONFLICT DO NOTHING`), not Conflict; an unknown `user_id` fails the insert's FK (500).
pub async fn replace(
    conn: &mut sqlx::PgConnection,
    user_id: UserId,
    desired: &PermissionSet,
    granted_by: UserId,
) -> DbResult<GrantDiff> {
    let mut tx = conn.begin().await?;

    let existing: Vec<String> = sqlx::query_scalar!(
        "SELECT permission FROM user_permissions WHERE user_id = $1 FOR UPDATE",
        user_id.as_uuid(),
    )
    .fetch_all(&mut *tx)
    .await?;

    let desired_tokens: Vec<&str> = desired.tokens();

    // Removes stored tokens the desired set lacks, including unrecognised ones — clears
    // inert grants instead of letting them accumulate.
    let removed: Vec<String> = existing
        .iter()
        .filter(|token| !desired_tokens.contains(&token.as_str()))
        .cloned()
        .collect();
    let added: Vec<String> = desired_tokens
        .iter()
        .filter(|token| !existing.iter().any(|e| e == *token))
        .map(|t| (*t).to_owned())
        .collect();

    if !removed.is_empty() {
        sqlx::query!(
            "DELETE FROM user_permissions WHERE user_id = $1 AND permission = ANY($2)",
            user_id.as_uuid(),
            &removed,
        )
        .execute(&mut *tx)
        .await?;
    }

    if !added.is_empty() {
        sqlx::query!(
            "INSERT INTO user_permissions (user_id, permission, granted_by) \
             SELECT $1, token, $3 FROM unnest($2::text[]) AS token \
             ON CONFLICT (user_id, permission) DO NOTHING",
            user_id.as_uuid(),
            &added,
            granted_by.as_uuid(),
        )
        .execute(&mut *tx)
        .await?;
    }

    tx.commit().await?;
    Ok(GrantDiff { added, removed })
}

/// What [`replace`] changed, for the audit trail (more useful post-incident than "set
/// permissions to X").
#[derive(Debug, Clone, Default, serde::Serialize)]
pub struct GrantDiff {
    pub added: Vec<String>,
    pub removed: Vec<String>,
}

impl GrantDiff {
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.added.is_empty() && self.removed.is_empty()
    }
}

/// Grant a single permission idempotently — for seeding/bootstrap, where [`replace`]'s
/// read-current-set-first cost is unneeded.
///
/// # Errors
/// `Sqlx` only; re-granting is `Ok(())` (`ON CONFLICT DO NOTHING`), so seeding is safe to
/// re-run.
pub async fn grant<'e, E: PgExecutor<'e>>(
    exec: E,
    user_id: UserId,
    permission: Permission,
    granted_by: Option<UserId>,
) -> DbResult<()> {
    sqlx::query!(
        "INSERT INTO user_permissions (user_id, permission, granted_by) VALUES ($1,$2,$3) \
         ON CONFLICT (user_id, permission) DO NOTHING",
        user_id.as_uuid(),
        permission.as_str(),
        granted_by.map(UserId::as_uuid),
    )
    .execute(exec)
    .await?;
    Ok(())
}

/// How many **active** accounts hold `permission`, excluding `ignoring` — the lockout guard
/// behind every "you cannot do that" refusal: revoking/suspending/erasing the last holder of a
/// capability would leave no way to grant anything back except editing the database directly.
/// Suspended accounts don't count; they can't sign in.
///
/// # Errors
/// `Sqlx` only; `count(*)` always returns a row, so `Ok(0)` means nobody else holds it. Must
/// propagate, not default to zero — that would permit the exact revocation this guards against.
pub async fn other_active_holders<'e, E: PgExecutor<'e>>(
    exec: E,
    permission: Permission,
    ignoring: UserId,
) -> DbResult<i64> {
    let count = sqlx::query_scalar!(
        "SELECT count(*) AS \"count!\" FROM user_permissions p \
         JOIN users u ON u.id = p.user_id \
         WHERE p.permission = $1 AND p.user_id <> $2 AND u.status = 'active'",
        permission.as_str(),
        ignoring.as_uuid(),
    )
    .fetch_one(exec)
    .await?;
    Ok(count)
}

/// Grant counts for a batch of users (avoids an N+1 per-row subquery), for the directory's
/// "permissions" column.
///
/// # Errors
/// `Sqlx` only; users with no grants are absent from the result, not `0` — callers must
/// default a missing id themselves.
pub async fn counts_for<'e, E: PgExecutor<'e>>(
    exec: E,
    user_ids: &[Uuid],
) -> DbResult<Vec<(Uuid, i64)>> {
    #[derive(FromRow)]
    struct Row {
        user_id: Uuid,
        granted: i64,
    }
    let rows = sqlx::query_as!(
        Row,
        "SELECT user_id, count(*) AS \"granted!\" FROM user_permissions \
         WHERE user_id = ANY($1) GROUP BY user_id",
        user_ids,
    )
    .fetch_all(exec)
    .await?;
    Ok(rows.into_iter().map(|r| (r.user_id, r.granted)).collect())
}
