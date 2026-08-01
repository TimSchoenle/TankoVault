//! The user record itself: creation, the login lookup, and the authorization-relevant state.
//! `UserRow` is the shared projection [`super::password_reset`] and [`super::profile`] also read.

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
/// Created with no permissions — a registration endpoint must not be able to mint privilege.
/// Grants are added afterwards via [`crate::repo::permissions::replace`].
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
/// Routed to one column by whether `login` contains `@`, not matched with `OR`: a username
/// containing `@` could otherwise collide with a different account's email address. Both
/// branches bind through [`CiText`] since both columns are `citext`.
///
/// # Errors
/// [`DbError::Sqlx`] only. An unknown login is `Ok(None)`, not [`DbError::NotFound`] — callers
/// must not distinguish "unknown account" from "wrong password".
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
/// Separate from the credential lookup so a failed attempt cannot advance the timestamp.
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
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AccountState {
    pub status: AccountStatus,
}

/// Read just the authorization-relevant account state.
///
/// # Errors
/// [`DbError::Sqlx`] only. A deleted account is `Ok(None)`, which callers must treat as deny.
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
