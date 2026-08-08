//! Operator-facing user administration.
//!
//! Split from [`crate::repo::users`] deliberately: that module is the authentication path,
//! reached on every sign-in and every token refresh, and it must stay small enough to audit as
//! a security-critical surface. This module is the administration path — searching a
//! directory, editing someone else's identity, suspending an account — which has different
//! callers, different authorization, and no business being mixed into the login query.
//!
//! Nothing here grants or revokes permissions; that is [`crate::repo::permissions`]. Nothing
//! here erases an account; that is [`crate::repo::privacy::erase_user`], which is shared with
//! the self-service path so there is exactly one implementation of the cascade.

use crate::error::{DbError, DbResult};
use sqlx::{FromRow, PgExecutor};
use tankovault_domain::{AccountStatus, UserId};
use time::OffsetDateTime;
use uuid::Uuid;

/// One row of the operator user directory.
///
/// Carries a grant *count* rather than the grants themselves: the directory is a list, and a
/// user's actual capabilities are what the detail view is for. The count is enough to answer
/// "which of these accounts are privileged at all", which is the question the list is scanned
/// for.
#[derive(Debug, Clone, serde::Serialize)]
pub struct DirectoryRow {
    pub id: Uuid,
    pub email: String,
    pub username: String,
    pub status: AccountStatus,
    /// Whether the address has been confirmed. An unverified account that has existed for
    /// months is usually an abandoned registration.
    pub email_verified: bool,
    /// How many permissions this account holds. `0` is an ordinary reader.
    ///
    /// Never a measure of *how much* an account may do: the super user holds one grant and can
    /// do everything, which is what [`Self::is_super_user`] exists to say.
    pub permission_count: i64,
    /// Whether this account holds the super-user grant.
    ///
    /// Carried separately because the grant is not enumerable — it is absent from the permission
    /// catalogue by design, so a client reconciling grants against that catalogue sees the
    /// deployment owner as an account holding nothing.
    pub is_super_user: bool,
    /// How many series the user tracks — the cheapest signal of a real, in-use account.
    pub tracked_count: i64,
    #[serde(with = "time::serde::rfc3339::option")]
    pub last_login_at: Option<OffsetDateTime>,
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
}

/// A page of the directory plus the unfiltered-by-page total, so the UI can render
/// "showing 1–25 of 312" without a second request.
#[derive(Debug, Clone, serde::Serialize)]
pub struct DirectoryPage {
    pub users: Vec<DirectoryRow>,
    /// Total matching the current search, ignoring `limit`/`offset`.
    pub total: i64,
}

/// Search the user directory.
///
/// `search` matches username or email as a case-insensitive substring; an empty search lists
/// everyone. Both columns are `citext`, so the comparison is already case-insensitive without
/// a `lower()` wrapper that would defeat their unique indexes.
///
/// The counts come from lateral subqueries rather than `GROUP BY` joins: with two independent
/// one-to-many relations (permissions and watchlist entries), a join would multiply rows and
/// need `count(DISTINCT …)` on both.
///
/// # Errors
/// [`DbError::Sqlx`] only — no other variant is reachable. A search matching nobody is an
/// empty page with `total: 0`, not [`DbError::NotFound`]; note that `total` is read off the
/// first row, so it is only meaningful because every returned row carries the same value.
pub async fn directory<'e, E: PgExecutor<'e>>(
    exec: E,
    search: &str,
    limit: i64,
    offset: i64,
) -> DbResult<DirectoryPage> {
    #[derive(FromRow)]
    struct Row {
        id: Uuid,
        email: String,
        username: String,
        status: AccountStatus,
        email_verified: bool,
        permission_count: i64,
        is_super_user: bool,
        tracked_count: i64,
        last_login_at: Option<OffsetDateTime>,
        created_at: OffsetDateTime,
        total: i64,
    }
    // `$1 = ''` short-circuits the pattern match so an unfiltered listing does not pay for a
    // `LIKE '%%'` scan predicate on every row.
    //
    // `ILIKE`, not `LIKE`, and that is a fix rather than a preference. `username`/`email` are
    // `citext`, but `'%' || $1 || '%'` concatenates down to `text`, so `citext ~~ text`
    // resolved to a plain case-sensitive `text ~~ text` — an operator searching for `alice`
    // found nothing for a user who registered as `Alice`. Same root cause as
    // `repo::users::CiText`, which fixes the equality lookups; a wrapper cannot fix this one
    // because the concatenation, not the parameter, is what carries the type. Read that
    // type's doc comment for why the schema's intent silently stops holding at the operator.
    // A leading wildcard forecloses an index either way, so `ILIKE` costs nothing here.
    let rows = sqlx::query_as!(
        Row,
        "WITH matched AS ( \
             SELECT u.* FROM users u \
             WHERE $1 = '' \
                OR u.username ILIKE '%' || $1 || '%' \
                OR u.email ILIKE '%' || $1 || '%' \
         ) \
         SELECT m.id, m.email::text AS \"email!\", m.username::text AS \"username!\", \
                m.status AS \"status: AccountStatus\", \
                (m.email_verified_at IS NOT NULL) AS \"email_verified!\", \
                m.last_login_at, m.created_at, \
                p.count AS \"permission_count!\", w.count AS \"tracked_count!\", \
                EXISTS (SELECT 1 FROM user_permissions up WHERE up.user_id = m.id \
                        AND up.permission = 'system.superuser') AS \"is_super_user!\", \
                (SELECT count(*) FROM matched) AS \"total!\" \
         FROM matched m \
         CROSS JOIN LATERAL (SELECT count(*) FROM user_permissions up WHERE up.user_id = m.id) \
                 AS p(count) \
         CROSS JOIN LATERAL (SELECT count(*) FROM watchlist_entries we WHERE we.user_id = m.id) \
                 AS w(count) \
         ORDER BY m.created_at DESC \
         LIMIT $2 OFFSET $3",
        search,
        limit,
        offset,
    )
    .fetch_all(exec)
    .await?;

    // With no matches there is no row to read the total from, and the total is 0 by definition.
    let total = rows.first().map_or(0, |r| r.total);
    Ok(DirectoryPage {
        total,
        users: rows
            .into_iter()
            .map(|r| DirectoryRow {
                id: r.id,
                email: r.email,
                username: r.username,
                status: r.status,
                email_verified: r.email_verified,
                permission_count: r.permission_count,
                is_super_user: r.is_super_user,
                tracked_count: r.tracked_count,
                last_login_at: r.last_login_at,
                created_at: r.created_at,
            })
            .collect(),
    })
}

/// Everything the user-detail panel shows, minus the grant list (fetched separately by
/// [`crate::repo::permissions::list_for_user`] so the panel can refresh just that part after
/// an edit).
#[derive(Debug, Clone, serde::Serialize)]
pub struct UserDetail {
    pub id: Uuid,
    pub email: String,
    pub username: String,
    pub status: AccountStatus,
    pub email_verified: bool,
    pub suspension_reason: Option<String>,
    #[serde(with = "time::serde::rfc3339::option")]
    pub suspended_at: Option<OffsetDateTime>,
    #[serde(with = "time::serde::rfc3339::option")]
    pub last_login_at: Option<OffsetDateTime>,
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
    /// Live login sessions. Tells an operator whether a suspension will actually take effect
    /// without also revoking sessions.
    pub active_sessions: i64,
    pub tracked_count: i64,
    /// Linked external trackers, so an operator can see the account has third-party
    /// credentials at rest before deciding to erase it.
    pub linked_accounts: i64,
    /// Unresolved data-subject requests filed by this user. An account with an open erasure
    /// request must not be quietly edited.
    pub open_privacy_requests: i64,
}

/// Fetch one user's administrative detail.
///
/// # Errors
/// [`DbError::NotFound`] — a 404 — when no such user exists; otherwise [`DbError::Sqlx`].
pub async fn detail<'e, E: PgExecutor<'e>>(exec: E, id: UserId) -> DbResult<UserDetail> {
    let row = sqlx::query_as!(
        UserDetail,
        "SELECT u.id, u.email::text AS \"email!\", u.username::text AS \"username!\", \
                u.status AS \"status: AccountStatus\", \
                (u.email_verified_at IS NOT NULL) AS \"email_verified!\", \
                u.suspension_reason, u.suspended_at, u.last_login_at, u.created_at, \
                (SELECT count(*) FROM refresh_tokens r \
                  WHERE r.user_id = u.id AND r.revoked_at IS NULL AND r.expires_at > now()) \
                  AS \"active_sessions!\", \
                (SELECT count(*) FROM watchlist_entries w WHERE w.user_id = u.id) \
                  AS \"tracked_count!\", \
                (SELECT count(*) FROM external_accounts e WHERE e.user_id = u.id) \
                  AS \"linked_accounts!\", \
                (SELECT count(*) FROM gdpr_requests g \
                  WHERE g.user_id = u.id AND g.status IN ('pending','in_progress')) \
                  AS \"open_privacy_requests!\" \
         FROM users u WHERE u.id = $1",
        id.as_uuid(),
    )
    .fetch_optional(exec)
    .await?;
    row.ok_or(DbError::NotFound)
}

/// Edit a user's identity on their behalf. `None` leaves a field unchanged.
///
/// Distinct from [`crate::repo::users::update_profile`] only in intent and audit action, but
/// deliberately a separate function: the self-service version is reachable by anyone for their
/// own account, and sharing one entry point between "I renamed myself" and "an administrator
/// renamed someone" is how an ownership check goes missing.
///
/// # Errors
/// [`DbError::Conflict`] if the new email or username is taken; [`DbError::NotFound`] if the
/// user is gone.
pub async fn update_identity<'e, E: PgExecutor<'e>>(
    exec: E,
    id: UserId,
    username: Option<&str>,
    email: Option<&str>,
) -> DbResult<()> {
    let result = sqlx::query!(
        "UPDATE users SET username = COALESCE($2, username), email = COALESCE($3, email) \
         WHERE id = $1",
        id.as_uuid(),
        username,
        email,
    )
    .execute(exec)
    .await
    .map_err(|e| {
        let de = DbError::from(e);
        if de.is_unique_violation() {
            DbError::Conflict("email or username already registered".to_owned())
        } else {
            de
        }
    })?;
    if result.rows_affected() == 0 {
        return Err(DbError::NotFound);
    }
    Ok(())
}

/// Suspend or reinstate an account.
///
/// Suspending clears nothing else: the account's data, grants and links are untouched, so a
/// suspension is reversible and is not a covert deletion. Revoking the account's live sessions
/// is a *separate* call the caller makes deliberately
/// ([`crate::repo::users::revoke_all_sessions`]) — a suspension that silently signed people
/// out would be indistinguishable from one that did not, and an operator needs to know which
/// they performed.
///
/// Reinstating clears `suspended_at` and the reason, so a re-suspension records its own fresh
/// timestamp rather than showing the first one.
///
/// # Errors
/// [`DbError::NotFound`] — a 404 — when `id` matches no row. Otherwise [`DbError::Sqlx`].
/// Setting the status an account already has is not [`DbError::Conflict`]: it succeeds and
/// refreshes `suspended_at`, which is deliberate for a re-suspension.
pub async fn set_status<'e, E: PgExecutor<'e>>(
    exec: E,
    id: UserId,
    status: AccountStatus,
    reason: Option<&str>,
) -> DbResult<()> {
    let suspending = status == AccountStatus::Suspended;
    let result = sqlx::query!(
        "UPDATE users SET status = $2, \
                suspended_at = CASE WHEN $3 THEN now() ELSE NULL END, \
                suspension_reason = CASE WHEN $3 THEN $4 ELSE NULL END \
         WHERE id = $1",
        id.as_uuid(),
        status as AccountStatus,
        suspending,
        reason,
    )
    .execute(exec)
    .await?;
    if result.rows_affected() == 0 {
        return Err(DbError::NotFound);
    }
    Ok(())
}

/// Confirm a user's email address administratively.
///
/// The escape hatch for a deployment whose outbound mail is broken or whose user never
/// received the link: without it, an unverified account is permanently unable to sign in and
/// has no self-service path back. Idempotent, matching
/// [`crate::repo::users::mark_email_verified`].
///
/// # Errors
/// [`DbError::NotFound`] — a 404 — when `id` matches no row; otherwise [`DbError::Sqlx`].
/// Unlike [`crate::repo::users::mark_email_verified`], which returns `Ok(())` for a missing
/// user, this one reports it: an operator invoking the escape hatch needs to be told it did
/// nothing.
pub async fn force_verify_email<'e, E: PgExecutor<'e>>(exec: E, id: UserId) -> DbResult<()> {
    let result = sqlx::query!(
        "UPDATE users SET email_verified_at = COALESCE(email_verified_at, now()) WHERE id = $1",
        id.as_uuid(),
    )
    .execute(exec)
    .await?;
    if result.rows_affected() == 0 {
        return Err(DbError::NotFound);
    }
    Ok(())
}
