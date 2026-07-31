//! The user record itself: creation, the login lookup, and the authorization-relevant state.
//!
//! This is the aggregate root of the module — `UserRow` is the shared projection of the
//! `users` table that [`super::password_reset`] and [`super::profile`] also read back.

use super::CiText;
use crate::error::{DbError, DbResult};
use sqlx::{FromRow, PgExecutor};
use tankovault_domain::{AccountStatus, User, UserId};
use time::OffsetDateTime;
use uuid::Uuid;

#[derive(FromRow)]
pub(super) struct UserRow {
    pub(super) id: Uuid,
    pub(super) email: String,
    pub(super) username: String,
    pub(super) status: AccountStatus,
    pub(super) created_at: OffsetDateTime,
}

impl From<UserRow> for User {
    fn from(r: UserRow) -> Self {
        Self {
            id: UserId::from_uuid(r.id),
            email: r.email,
            username: r.username,
            status: r.status,
            created_at: r.created_at,
        }
    }
}

/// A user together with the stored password hash (login verification only).
pub struct Credentials {
    pub user: User,
    pub password_hash: String,
    /// Whether the account's email address has been confirmed. `false` blocks sign-in until
    /// the user clicks the emailed confirmation link (see `email_verification_tokens`).
    pub email_verified: bool,
}

/// Create a user. `password_hash` is a pre-computed argon2id PHC string.
///
/// The account is created with **no permissions**, which is the only safe default: a
/// registration endpoint must not be able to mint privilege. Grants are added afterwards by
/// someone holding [`Permission::UsersPermissions`](tankovault_domain::Permission), via
/// [`crate::repo::permissions::replace`].
///
/// # Errors
/// [`DbError::Conflict`] if the email or username is taken.
pub async fn create<'e, E: PgExecutor<'e>>(
    exec: E,
    email: &str,
    username: &str,
    password_hash: &str,
) -> DbResult<User> {
    let id = UserId::new();
    let row = sqlx::query_as!(
        UserRow,
        "INSERT INTO users (id, email, username, password_hash) \
         VALUES ($1,$2,$3,$4) \
         RETURNING id, email, username, status AS \"status: AccountStatus\", created_at",
        id.as_uuid(),
        email,
        username,
        password_hash,
    )
    .fetch_one(exec)
    .await
    .map_err(|e| {
        let de = DbError::from(e);
        if de.is_unique_violation() {
            DbError::Conflict("email or username already registered".to_owned())
        } else {
            de
        }
    })?;
    Ok(row.into())
}

/// Look up credentials by email or username (login accepts either).
///
/// The identifier is routed to **one** column, chosen by whether it contains `@`, rather than
/// matched against both. `WHERE email = $1 OR username = $1` was ambiguous: a username
/// containing `@` — which nothing stopped an operator writing before the validator was applied
/// to every write path — could collide with a *different* account's address, and which row the
/// planner returned decided whose password was checked. Routing removes the ambiguity
/// regardless of what is already stored, because an address always contains `@` and a valid
/// username never may.
///
/// It is also two indexed equality lookups instead of an `OR` the planner cannot serve from
/// one index.
///
/// Both branches bind through [`CiText`], because both columns are `citext` and a bare `&str`
/// would make this the one query in the system where case matters — see that type for why.
///
/// # Errors
/// [`DbError::Sqlx`] only — no other variant is reachable. An unknown login is `Ok(None)`, not
/// [`DbError::NotFound`]: the caller must answer "unknown account" and "wrong password"
/// identically, and a distinct error variant here is how that distinction leaks back out.
pub async fn find_credentials<'e, E: PgExecutor<'e>>(
    exec: E,
    login: &str,
) -> DbResult<Option<Credentials>> {
    #[derive(FromRow)]
    struct Row {
        id: Uuid,
        email: String,
        username: String,
        status: AccountStatus,
        created_at: OffsetDateTime,
        password_hash: String,
        email_verified: bool,
    }
    let row = if login.contains('@') {
        sqlx::query_as!(
            Row,
            "SELECT id, email, username, status AS \"status: AccountStatus\", created_at, \
                    password_hash, (email_verified_at IS NOT NULL) AS \"email_verified!\" \
             FROM users WHERE email = $1",
            CiText(login) as _,
        )
        .fetch_optional(exec)
        .await?
    } else {
        sqlx::query_as!(
            Row,
            "SELECT id, email, username, status AS \"status: AccountStatus\", created_at, \
                    password_hash, (email_verified_at IS NOT NULL) AS \"email_verified!\" \
             FROM users WHERE username = $1",
            CiText(login) as _,
        )
        .fetch_optional(exec)
        .await?
    };

    Ok(row.map(|r| Credentials {
        user: User {
            id: UserId::from_uuid(r.id),
            email: r.email,
            username: r.username,
            status: r.status,
            created_at: r.created_at,
        },
        password_hash: r.password_hash,
        email_verified: r.email_verified,
    }))
}

/// Fetch a user by id.
///
/// # Errors
/// [`DbError::NotFound`] — a 404 — when no such user exists; otherwise [`DbError::Sqlx`].
pub async fn get<'e, E: PgExecutor<'e>>(exec: E, id: UserId) -> DbResult<User> {
    let row = sqlx::query_as!(
        UserRow,
        "SELECT id, email, username, status AS \"status: AccountStatus\", created_at \
         FROM users WHERE id = $1",
        id.as_uuid(),
    )
    .fetch_optional(exec)
    .await?;
    Ok(row.ok_or(DbError::NotFound)?.into())
}

/// Record a successful sign-in.
///
/// Separate from the credential lookup so a *failed* attempt cannot advance the timestamp:
/// "last login" that moves on every guess would be worse than not having it, both for the
/// operator reading the directory and for a user checking their own account.
///
/// # Errors
/// [`DbError::Sqlx`] only — no other variant is reachable. An `id` matching no row updates
/// nothing and still returns `Ok(())`; a failure here must not fail the sign-in that already
/// succeeded, so callers log rather than propagate.
pub async fn touch_last_login<'e, E: PgExecutor<'e>>(exec: E, id: UserId) -> DbResult<()> {
    sqlx::query!(
        "UPDATE users SET last_login_at = now() WHERE id = $1",
        id.as_uuid(),
    )
    .execute(exec)
    .await?;
    Ok(())
}

/// The account state the authorization layer needs on every authenticated request.
///
/// Fetched with the permission grants in one round trip (see
/// [`crate::repo::permissions::resolve`]) because both are needed together and neither is
/// useful alone: a grant set without the status would authorize a suspended account.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AccountState {
    pub status: AccountStatus,
}

/// Read just the authorization-relevant account state.
///
/// # Errors
/// [`DbError::Sqlx`] only — no other variant is reachable. A deleted account is `Ok(None)`,
/// which the authorization layer must treat as "deny", not as "no constraint": this is the
/// read that stands between a revoked account and a still-valid access token.
pub async fn account_state<'e, E: PgExecutor<'e>>(
    exec: E,
    id: UserId,
) -> DbResult<Option<AccountState>> {
    let status = sqlx::query_scalar!(
        "SELECT status AS \"status: AccountStatus\" FROM users WHERE id = $1",
        id.as_uuid(),
    )
    .fetch_optional(exec)
    .await?;
    Ok(status.map(|status| AccountState { status }))
}
