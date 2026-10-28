//! Permission grants: the persistence behind [`tankovault_domain::Permission`].
//!
//! Resolved from here on every authenticated request, not cached in an access token, so a
//! revocation takes effect on the next request and not at the token's expiry.
//!
//! Unrecognised stored tokens are dropped, not rejected; see
//! [`tankovault_domain::PermissionSet::from_tokens`].

use crate::error::DbResult;
use sqlx::{Connection as _, FromRow, PgExecutor};
use tankovault_domain::{AccountStatus, Permission, PermissionSet, UserId};
use time::OffsetDateTime;
use uuid::Uuid;

/// Everything the authorization layer needs about a principal, in one read.
#[derive(Debug, Clone)]
pub struct Principal {
    /// Whether the account may act at all. Checked before any permission is.
    pub status: AccountStatus,
    /// Every capability the account holds, unrecognised tokens already dropped.
    pub permissions: PermissionSet,
    /// Whether this account has opted into adult content *and* attested its age.
    ///
    /// Resolved here because catalogue reads, the hottest authenticated path, consult it, and
    /// this query already has the row open.
    ///
    /// Only half the answer: the deployment flag is the other half, and both must be true. See
    /// `services/api/src/content_gate.rs`.
    pub adult_opt_in: bool,
    /// Whether this account holds a usable second factor: a *confirmed* TOTP enrolment or at
    /// least one security key.
    ///
    /// Resolved here because `AuthUser::require`, which every privileged handler goes through,
    /// consults it, and this query already has the row open.
    ///
    /// An *unconfirmed* TOTP row does not count. The secret was issued and the user never proved
    /// they stored it; counting it would let a half-finished enrolment satisfy the requirement
    /// and then fail every sign-in.
    pub mfa_enrolled: bool,
}

/// Resolves a principal's account status and permission grants in one round trip.
///
/// A `LEFT JOIN`, so an account with no grants still yields a row. `None` means the account is
/// gone: a caller must fail closed on it and on an error, never treat it as a principal with no
/// permissions.
///
/// # Errors
/// `Sqlx` only.
pub async fn resolve<'e, E: PgExecutor<'e>>(
    exec: E,
    user_id: UserId,
) -> DbResult<Option<Principal>> {
    #[derive(FromRow)]
    struct Row {
        status: AccountStatus,
        permissions: Vec<String>,
        adult_opt_in: bool,
        mfa_enrolled: bool,
    }
    let row = sqlx::query_as!(
        Row,
        // The attestation is folded in here: `adult_opt_in` alone is the preference, and the
        // pair is the entitlement. A schema constraint keeps them consistent today; a caller
        // reading only the preference would be wrong the first time attestation gains an expiry.
        //
        // `GROUP BY u.id` alone: the id is the primary key, so Postgres derives the rest by
        // functional dependency, and a column added to the projection needs no second edit.
        "SELECT u.status AS \"status: AccountStatus\", \
                coalesce(array_agg(p.permission) FILTER (WHERE p.permission IS NOT NULL), \
                         '{}'::text[]) AS \"permissions!\", \
                (u.adult_opt_in AND u.age_attested_at IS NOT NULL) AS \"adult_opt_in!\", \
                (EXISTS (SELECT 1 FROM user_totp t \
                          WHERE t.user_id = u.id AND t.confirmed_at IS NOT NULL) \
                 OR EXISTS (SELECT 1 FROM user_webauthn_credentials c \
                             WHERE c.user_id = u.id AND c.purpose = 'security_key')) \
                  AS \"mfa_enrolled!\" \
         FROM users u \
         LEFT JOIN user_permissions p ON p.user_id = u.id \
         WHERE u.id = $1 \
         GROUP BY u.id",
        user_id.as_uuid(),
    )
    .fetch_optional(exec)
    .await?;

    Ok(row.map(|r| Principal {
        status: r.status,
        permissions: PermissionSet::from_tokens(&r.permissions, |token| {
            // Logged, not silently dropped, so drift between schema and binary shows.
            tracing::warn!(%token, user_id = %user_id.as_uuid(), "ignoring unknown permission grant");
        }),
        adult_opt_in: r.adult_opt_in,
        mfa_enrolled: r.mfa_enrolled,
    }))
}

/// A single grant, with its provenance, for the user-detail view.
#[derive(Debug, Clone, serde::Serialize)]
pub struct GrantRow {
    /// A string, not the enum, so a grant from a build with a capability this one lacks stays
    /// visible to an admin.
    pub permission: String,
    /// Whether this build recognises the token. `false` means the grant is inert.
    pub known: bool,
    /// When the grant was made.
    #[serde(with = "time::serde::rfc3339")]
    pub granted_at: OffsetDateTime,
    /// `None` for a migration-era grant or an erased administrator.
    pub granted_by: Option<String>,
}

/// Lists a user's grants with provenance, newest first.
///
/// # Errors
/// `Sqlx` only.
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

/// Replaces a user's entire grant set in one transaction, returning what changed.
///
/// A whole-set diff, so a checklist submit cannot interleave with itself. Unchanged grants keep
/// their `granted_at`, a concurrent identical grant is absorbed, and
/// [`Permission::SuperUser`] is inert in both directions.
///
/// # Errors
/// `Sqlx`, rolling back on the first failure; an unknown `user_id` fails the insert's foreign
/// key.
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

    // The super user grant belongs to the installer: it is filtered out of both sides of the
    // diff, so an edit can neither mint nor strip one. The editor's catalogue does not list it,
    // so every checklist submit omits it, and without this the first save on the owner's
    // account would silently demote them.
    let super_user = Permission::SuperUser.as_str();

    // Removes stored tokens the desired set lacks, unrecognised ones included, so inert grants
    // do not accumulate.
    let removed: Vec<String> = existing
        .iter()
        .filter(|token| token.as_str() != super_user && !desired_tokens.contains(&token.as_str()))
        .cloned()
        .collect();
    let added: Vec<String> = desired_tokens
        .iter()
        .filter(|token| **token != super_user && !existing.iter().any(|e| e == *token))
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
    /// Permission tokens the call granted.
    pub added: Vec<String>,
    /// Permission tokens the call revoked.
    pub removed: Vec<String>,
}

impl GrantDiff {
    /// Returns whether the call changed nothing, in which case there is no audit record worth
    /// writing.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.added.is_empty() && self.removed.is_empty()
    }
}

/// Grants a single permission idempotently, for seeding, where [`replace`]'s read of the
/// current set is unneeded.
///
/// Re-granting succeeds, so seeding is safe to re-run.
///
/// # Errors
/// `Sqlx` only.
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

/// Returns how many active accounts hold `permission`, not counting `ignoring`.
///
/// The lockout guard behind every refusal on this surface: revoking, suspending or erasing the
/// last holder of a capability leaves no way to grant anything back short of editing the
/// database. Suspended accounts do not count, because they cannot sign in to use the grant.
///
/// A super user counts as a holder of everything, matching [`PermissionSet::has`]. Counting
/// only the exact token would refuse a revocation the deployment owner could undo in a click.
/// A caller must propagate an error, since defaulting to zero permits the revocation this
/// guards against.
///
/// # Errors
/// `Sqlx` only.
pub async fn other_active_holders<'e, E: PgExecutor<'e>>(
    exec: E,
    permission: Permission,
    ignoring: UserId,
) -> DbResult<i64> {
    let count = sqlx::query_scalar!(
        // DISTINCT because an account can hold both the exact grant and the super user one.
        "SELECT count(DISTINCT p.user_id) AS \"count!\" FROM user_permissions p \
         JOIN users u ON u.id = p.user_id \
         WHERE p.permission IN ($1, $3) AND p.user_id <> $2 AND u.status = 'active'",
        permission.as_str(),
        ignoring.as_uuid(),
        Permission::SuperUser.as_str(),
    )
    .fetch_one(exec)
    .await?;
    Ok(count)
}

/// Grants [`Permission::SuperUser`] to `user_id`, and only if it is the deployment's *first*
/// account, the one the bootstrap migrator has just created.
///
/// The two conditions that make the grant unforgeable live in the database, not in the caller:
/// this insert is a no-op unless no other account exists, and a partial unique index refuses a
/// second super user however the row is written.
///
/// `false` means the conditions did not hold and nothing was written, the normal outcome of
/// re-running an install job.
///
/// # Errors
/// `Sqlx` only.
pub async fn claim_super_user<'e, E: PgExecutor<'e>>(exec: E, user_id: UserId) -> DbResult<bool> {
    let result = sqlx::query!(
        "INSERT INTO user_permissions (user_id, permission) \
         SELECT $1::uuid, $2::text \
         WHERE NOT EXISTS (SELECT 1 FROM users WHERE id <> $1::uuid) \
         ON CONFLICT DO NOTHING",
        user_id.as_uuid(),
        Permission::SuperUser.as_str(),
    )
    .execute(exec)
    .await?;
    Ok(result.rows_affected() > 0)
}

/// Promotes the owner candidate when this deployment has no super user left.
///
/// Answers who was promoted, and `None` both when one already exists, which is the normal case,
/// and when there is no candidate to promote.
///
/// [`claim_super_user`] covers the install. A deployment whose users registered before the
/// seed job ran has no first account to promote, and erasing the owner drops the only row the
/// grant can live in. Nothing in the API can mint the grant, so neither state is recoverable by
/// hand.
///
/// The candidate is the **earliest active account that administers permissions**, the same
/// rule as migration `0042`, so a reconciled deployment and a freshly migrated one name the same
/// account. `users.permissions` is equivalent to full control, so this promotes nobody who
/// could not already grant themselves everything enumerable. Suspended accounts are skipped:
/// the grant is single-slot, and one that cannot sign in can do nothing with it.
///
/// Safe to call from every replica at boot: the `NOT EXISTS` decides, and the partial unique
/// index turns a racing loser's insert into a no-op. A caller must propagate an error, since
/// reading it as "already owned" leaves the deployment silently unowned.
///
/// # Errors
/// `Sqlx` only.
pub async fn ensure_super_user<'e, E: PgExecutor<'e>>(exec: E) -> DbResult<Option<UserId>> {
    let promoted: Option<Uuid> = sqlx::query_scalar!(
        "INSERT INTO user_permissions (user_id, permission) \
         SELECT u.id, $1::text FROM users u \
         WHERE u.status = 'active' \
           AND EXISTS (SELECT 1 FROM user_permissions p \
                        WHERE p.user_id = u.id AND p.permission = $2::text) \
           AND NOT EXISTS (SELECT 1 FROM user_permissions s WHERE s.permission = $1::text) \
         ORDER BY u.created_at, u.id \
         LIMIT 1 \
         ON CONFLICT DO NOTHING \
         RETURNING user_id",
        Permission::SuperUser.as_str(),
        Permission::UsersPermissions.as_str(),
    )
    .fetch_optional(exec)
    .await?;
    Ok(promoted.map(UserId::from_uuid))
}

/// Writes an explicit row for every grantable capability the deployment's super user is missing,
/// returning the tokens newly stored.
///
/// No authorization outcome changes: [`PermissionSet::has`] already answers true to everything
/// for the grant. The rows are for what reads the stored set literally: the permission editor's
/// checklist, the directory's count, and an operator auditing who can do what. The seed is
/// create-only, so without this the owner shows a set that falls further behind every release.
///
/// [`Permission::grantable`] is the source list, so [`Permission::SuperUser`] is excluded and
/// the single-super-user index is never contended. A deployment with no super user writes
/// nothing, which is why [`ensure_super_user`] runs first.
///
/// Safe to call from every replica at boot, and a no-op on every boot after the first:
/// `ON CONFLICT DO NOTHING` absorbs both the re-run and a concurrent identical insert.
///
/// # Errors
/// `Sqlx` only.
pub async fn grant_all_to_super_user<'e, E: PgExecutor<'e>>(exec: E) -> DbResult<Vec<String>> {
    let grantable: Vec<String> = Permission::grantable()
        .into_iter()
        .map(|p| p.as_str().to_owned())
        .collect();
    let added: Vec<String> = sqlx::query_scalar!(
        "INSERT INTO user_permissions (user_id, permission) \
         SELECT owner.user_id, token \
         FROM user_permissions owner \
         CROSS JOIN unnest($1::text[]) AS token \
         WHERE owner.permission = $2::text \
         ON CONFLICT (user_id, permission) DO NOTHING \
         RETURNING permission AS \"permission!\"",
        &grantable,
        Permission::SuperUser.as_str(),
    )
    .fetch_all(exec)
    .await?;
    Ok(added)
}

/// Returns the grant count of each user in a batch, for the directory's "permissions" column.
///
/// A user with no grants is absent from the result, so a caller defaults a missing id to `0`.
///
/// # Errors
/// `Sqlx` only.
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
