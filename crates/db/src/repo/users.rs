//! User accounts and rotating refresh tokens.
//!
//! The `auth` crate owns hashing; this layer stores only the argon2id `password_hash`
//! and the SHA-256 `token_hash`, never plaintext secrets.

use crate::error::{DbError, DbResult};
use tankovault_domain::{User, UserId, UserRole};
use sqlx::{FromRow, PgExecutor};
use std::str::FromStr;
use time::OffsetDateTime;
use uuid::Uuid;

#[derive(FromRow)]
struct UserRow {
    id: Uuid,
    email: String,
    username: String,
    role: String,
    created_at: OffsetDateTime,
}

impl TryFrom<UserRow> for User {
    type Error = DbError;
    fn try_from(r: UserRow) -> Result<Self, Self::Error> {
        Ok(Self {
            id: UserId::from_uuid(r.id),
            email: r.email,
            username: r.username,
            role: UserRole::from_str(&r.role)?,
            created_at: r.created_at,
        })
    }
}

/// A user together with the stored password hash (login verification only).
pub struct Credentials {
    pub user: User,
    pub password_hash: String,
}

/// Create a user. `password_hash` is a pre-computed argon2id PHC string.
///
/// # Errors
/// [`DbError::Conflict`] if the email or username is taken.
pub async fn create<'e, E: PgExecutor<'e>>(
    exec: E,
    email: &str,
    username: &str,
    password_hash: &str,
    role: UserRole,
) -> DbResult<User> {
    let id = UserId::new();
    let row: UserRow = sqlx::query_as(
        "INSERT INTO users (id, email, username, password_hash, role) \
         VALUES ($1,$2,$3,$4,$5::user_role) \
         RETURNING id, email, username, role::text AS role, created_at",
    )
    .bind(id.as_uuid())
    .bind(email)
    .bind(username)
    .bind(password_hash)
    .bind(role.as_str())
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
    row.try_into()
}

/// Look up credentials by email or username (login accepts either).
pub async fn find_credentials<'e, E: PgExecutor<'e>>(
    exec: E,
    login: &str,
) -> DbResult<Option<Credentials>> {
    #[derive(FromRow)]
    struct Row {
        id: Uuid,
        email: String,
        username: String,
        role: String,
        created_at: OffsetDateTime,
        password_hash: String,
    }
    let row: Option<Row> = sqlx::query_as(
        "SELECT id, email, username, role::text AS role, created_at, password_hash \
         FROM users WHERE email = $1 OR username = $1",
    )
    .bind(login)
    .fetch_optional(exec)
    .await?;

    row.map(|r| {
        Ok(Credentials {
            user: User {
                id: UserId::from_uuid(r.id),
                email: r.email,
                username: r.username,
                role: UserRole::from_str(&r.role)?,
                created_at: r.created_at,
            },
            password_hash: r.password_hash,
        })
    })
    .transpose()
}

/// Fetch a user by id.
pub async fn get<'e, E: PgExecutor<'e>>(exec: E, id: UserId) -> DbResult<User> {
    let row: Option<UserRow> = sqlx::query_as(
        "SELECT id, email, username, role::text AS role, created_at FROM users WHERE id = $1",
    )
    .bind(id.as_uuid())
    .fetch_optional(exec)
    .await?;
    row.ok_or(DbError::NotFound)?.try_into()
}

// ---------------------------------------------------------------------------
// Refresh tokens (rotation + reuse detection)
// ---------------------------------------------------------------------------

/// A stored refresh-token record (hash only).
pub struct RefreshRecord {
    pub id: Uuid,
    pub user_id: UserId,
    pub family_id: Uuid,
    pub expires_at: OffsetDateTime,
    pub revoked_at: Option<OffsetDateTime>,
}

/// Persist a freshly issued refresh token (as its SHA-256 hash).
pub async fn insert_refresh<'e, E: PgExecutor<'e>>(
    exec: E,
    user_id: UserId,
    family_id: Uuid,
    token_hash: &str,
    expires_at: OffsetDateTime,
) -> DbResult<()> {
    sqlx::query(
        "INSERT INTO refresh_tokens (id, user_id, family_id, token_hash, expires_at) \
         VALUES ($1,$2,$3,$4,$5)",
    )
    .bind(Uuid::now_v7())
    .bind(user_id.as_uuid())
    .bind(family_id)
    .bind(token_hash)
    .bind(expires_at)
    .execute(exec)
    .await?;
    Ok(())
}

/// Find a refresh token by its hash (regardless of revocation, for reuse detection).
pub async fn find_refresh<'e, E: PgExecutor<'e>>(
    exec: E,
    token_hash: &str,
) -> DbResult<Option<RefreshRecord>> {
    #[derive(FromRow)]
    struct Row {
        id: Uuid,
        user_id: Uuid,
        family_id: Uuid,
        expires_at: OffsetDateTime,
        revoked_at: Option<OffsetDateTime>,
    }
    let row: Option<Row> = sqlx::query_as(
        "SELECT id, user_id, family_id, expires_at, revoked_at FROM refresh_tokens \
         WHERE token_hash = $1",
    )
    .bind(token_hash)
    .fetch_optional(exec)
    .await?;
    Ok(row.map(|r| RefreshRecord {
        id: r.id,
        user_id: UserId::from_uuid(r.user_id),
        family_id: r.family_id,
        expires_at: r.expires_at,
        revoked_at: r.revoked_at,
    }))
}

/// Revoke a single token by id (normal rotation).
pub async fn revoke_token<'e, E: PgExecutor<'e>>(exec: E, id: Uuid) -> DbResult<()> {
    sqlx::query(
        "UPDATE refresh_tokens SET revoked_at = now() WHERE id = $1 AND revoked_at IS NULL",
    )
    .bind(id)
    .execute(exec)
    .await?;
    Ok(())
}

/// Revoke an entire token family (reuse detected → invalidate the lineage).
pub async fn revoke_family<'e, E: PgExecutor<'e>>(exec: E, family_id: Uuid) -> DbResult<()> {
    sqlx::query(
        "UPDATE refresh_tokens SET revoked_at = now() WHERE family_id = $1 AND revoked_at IS NULL",
    )
    .bind(family_id)
    .execute(exec)
    .await?;
    Ok(())
}
