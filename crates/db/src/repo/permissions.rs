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
    /// Whether this account has opted into adult content *and* attested its age.
    ///
    /// Resolved here rather than read separately because it is consulted on catalogue reads —
    /// the hottest authenticated path there is — and a second round trip per browse request to
    /// answer one boolean is not worth it when this query already has the row open.
    ///
    /// Only half the answer: the deployment flag is the other half, and both must be true. See
    /// `services/api/src/content_gate.rs`.
    pub adult_opt_in: bool,
    /// Whether this account holds a usable second factor — a *confirmed* TOTP enrolment or at
    /// least one security key.
    ///
    /// Resolved here for the same reason [`Self::adult_opt_in`] is: it is consulted by
    /// `AuthUser::require`, which every privileged handler funnels through, and a second round
    /// trip per privileged request to answer one boolean is not worth it when this query already
    /// has the row open.
    ///
    /// An *unconfirmed* TOTP row does not count. It means the secret was issued and the user
    /// never proved they stored it; counting it would let a half-finished enrolment satisfy the
    /// requirement and then fail every sign-in.
    pub mfa_enrolled: bool,
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
        adult_opt_in: bool,
        mfa_enrolled: bool,
    }
    let row = sqlx::query_as!(
        Row,
        // The attestation is folded in here, not compared by the caller: `adult_opt_in` alone is
        // the preference, and the pair is the entitlement. A caller reading only the preference
        // would be right today — a schema constraint keeps them consistent — and wrong the first
        // time attestation gains an expiry.
        //
        // `GROUP BY u.id` rather than the projected columns: the id is the table's primary key,
        // so Postgres derives the rest by functional dependency, and a column added to the
        // projection later does not have to be added here too — which is how the grouped list
        // and the projected list drift apart.
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
            // Logged, not silently dropped — surfaces schema/binary drift.
            tracing::warn!(%token, user_id = %user_id.as_uuid(), "ignoring unknown permission grant");
        }),
        adult_opt_in: r.adult_opt_in,
        mfa_enrolled: r.mfa_enrolled,
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
/// itself. Unchanged grants keep their `granted_at`. [`Permission::SuperUser`] is inert here in
/// both directions — see the comment on the diff.
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

    // The super user grant belongs to the installer, not to this path: it is filtered out of
    // both sides of the diff, so an edit can neither mint one nor strip one. Stripping is the
    // case that matters — the editor's catalogue does not list it, so *every* checklist submit
    // omits it, and without this the first save against the deployment owner's account would
    // silently demote them.
    let super_user = Permission::SuperUser.as_str();

    // Removes stored tokens the desired set lacks, including unrecognised ones — clears
    // inert grants instead of letting them accumulate.
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
/// A super user counts as a holder of everything, matching [`PermissionSet::has`]. Counting
/// only the exact token would refuse a revocation the deployment owner could undo in a click.
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

/// Grant [`Permission::SuperUser`] to `user_id`, and only if it is the deployment's *first*
/// account — the one the bootstrap migrator has just created.
///
/// The two conditions that make the grant unforgeable live in the database, not in the caller:
/// this insert is a no-op unless no other account exists, and a partial unique index refuses a
/// second super user however the row is written.
///
/// # Errors
/// `Sqlx` only. `Ok(false)` means the conditions did not hold and nothing was written, which is
/// the normal outcome of re-running an install job.
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

/// Make sure this deployment still has a super user, promoting the owner candidate when it does
/// not. Returns who was promoted, or `None` when one already exists (the normal case) or there
/// is no candidate.
///
/// [`claim_super_user`] covers the install: the first account of an empty database takes
/// ownership. It cannot cover everything after that, and the gap is not theoretical — a
/// deployment whose users registered before the seed job ran never had a first account to
/// promote, and erasing the owner drops the only row the grant can live in. Neither state is
/// recoverable by hand, because nothing in the API can mint the grant; the deployment would keep
/// serving with no account that outlives the next capability the codebase gains.
///
/// The candidate is migration `0042`'s rule, kept identical on purpose so a reconciled
/// deployment and a freshly migrated one name the same account: the **earliest account that
/// still administers permissions**. `users.permissions` is documented as equivalent to full
/// control, so this promotes nobody who could not already grant themselves everything
/// enumerable. Suspended accounts are skipped — they cannot sign in, and the grant is
/// single-slot, so promoting one would spend the deployment's only ownership on an account that
/// can do nothing with it.
///
/// Safe to call from every replica at boot: the `NOT EXISTS` decides, and the partial unique
/// index settles a race by turning the loser's insert into a no-op.
///
/// # Errors
/// `Sqlx` only. Must propagate — treating a failed reconciliation as "already owned" is what
/// would leave the deployment unowned silently.
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

/// Write an explicit row for every grantable capability the deployment's super user is missing,
/// returning the tokens newly stored.
///
/// This changes no authorization outcome: [`PermissionSet::has`] already answers true to
/// everything for the grant, and it keeps doing so for capabilities added after this ran. What
/// it fixes is everything that reads the stored set *literally* rather than through that
/// implication — the permission editor renders the owner's checklist from these rows, the
/// directory counts them, and an operator auditing "who can do what" reads rows too. Without it
/// the owner is displayed holding a set that silently falls further behind the codebase every
/// release: the seed is create-only, so nothing ever tops up an account created before a
/// capability existed, and the console gives no sign that the gap is cosmetic.
///
/// [`Permission::grantable`] is the source list, so [`Permission::SuperUser`] is excluded and
/// the single-super-user index is never contended. A deployment with no super user writes
/// nothing — [`ensure_super_user`] runs first for that reason.
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
