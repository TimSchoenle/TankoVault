//! Permission grants — the persistence behind [`tankovault_domain::Permission`].
//!
//! # Why this is read on every authenticated request
//!
//! The obvious alternative is to stamp the grant set into the access token at sign-in. That
//! is one fewer query per request, and it is wrong for the operation this system needs to
//! support: an administrator who has just discovered a compromised account revokes its
//! permissions and expects them gone. With claims in the token they persist for the whole
//! remaining access-token lifetime, and there is no in-band way to shorten that — revoking
//! the refresh family does not invalidate an access token already issued.
//!
//! So authorization resolves from here, every time. The cost is one index lookup on the
//! `user_permissions` primary key, joined to the owning account's status ([`resolve`] fetches
//! both in a single round trip). In exchange, revocation is immediate and there is exactly one
//! authority on what a principal may do.
//!
//! Unrecognised stored tokens are dropped rather than rejected — see
//! [`tankovault_domain::PermissionSet::from_tokens`] for why that direction is the safe one.

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

/// Resolve a principal's account status and permission grants.
///
/// Returns `None` when the user no longer exists — a token outliving its account, which must
/// be treated as unauthenticated rather than as a principal with no permissions (the latter
/// would let a deleted account keep reading its own now-nonexistent data).
///
/// The two facts come back together from one statement: `LEFT JOIN`, so an account with no
/// grants still yields a row and is distinguishable from an account that is gone.
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
            // A grant this build cannot interpret. Logged rather than silently ignored: it
            // means the schema and the binary disagree, which an operator should know about.
            tracing::warn!(%token, user_id = %user_id.as_uuid(), "ignoring unknown permission grant");
        }),
    }))
}

/// A single grant, with its provenance, for the user-detail view.
#[derive(Debug, Clone, serde::Serialize, utoipa::ToSchema)]
pub struct GrantRow {
    /// The permission token. A string rather than the enum because a row surviving from a
    /// build that had a capability this one does not must still be *visible* to an
    /// administrator — that is precisely when they need to see and remove it.
    pub permission: String,
    /// Whether this build recognises the token. `false` means the grant is inert.
    pub known: bool,
    #[serde(with = "time::serde::rfc3339")]
    #[schema(value_type = String)]
    pub granted_at: OffsetDateTime,
    /// Who granted it, or `None` for a grant made by the migration from the old role model
    /// or by an administrator since erased.
    pub granted_by: Option<String>,
}

/// List a user's grants with provenance, newest first.
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

/// Replace a user's entire grant set, returning what changed.
///
/// Whole-set replacement rather than add/remove calls: the admin UI edits a checklist and
/// submits it, and a diff computed here from one authoritative "after" state cannot produce
/// the interleaving that two concurrent add/remove sequences can. Runs in one transaction so
/// a principal is never observed mid-edit with neither the old nor the new set.
///
/// Deleting *all* rows for the user and re-inserting would lose `granted_at` provenance on
/// every unchanged grant, so unchanged rows are left in place: only the genuine additions and
/// removals are written.
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

    // Any stored token the desired set does not contain is removed — including tokens this
    // build does not recognise, which is how an inert grant left over from another version
    // gets cleaned up rather than accumulating forever.
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

/// What [`replace`] actually changed. Recorded in the audit trail: "set permissions to X" is
/// far less useful after an incident than "added `users.delete`, removed nothing".
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

/// Grant a single permission, idempotently. Used by seeding and bootstrap paths, where the
/// caller knows exactly one capability is being added and a whole-set replace would need to
/// read the current set first.
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

/// How many **active** accounts hold `permission`, excluding `ignoring`.
///
/// The guard behind every "you cannot do that" refusal in user administration: revoking the
/// last `users.permissions` grant, suspending its last holder or erasing them would leave the
/// deployment with no way to grant anything, recoverable only by editing the database. The
/// `ignoring` parameter is the account about to be changed, so the caller asks "would anyone
/// else still hold it?" in one query rather than counting and subtracting.
///
/// Suspended accounts are not counted: a suspended administrator cannot sign in, so they are
/// not a recovery path.
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

/// The grant counts for a batch of users, for the directory listing's "permissions" column.
///
/// A batch read rather than a count per row: the directory lists up to a page of users and a
/// per-row subquery is the N+1 this repository has removed elsewhere.
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
