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

// ---------------------------------------------------------------------------
// Account settings (frontend §9.4)
// ---------------------------------------------------------------------------

/// Update a user's editable profile fields (username / email). `None` leaves a field
/// unchanged (`COALESCE`). Returns the refreshed user.
///
/// # Errors
/// [`DbError::Conflict`] if the new email or username is already taken.
pub async fn update_profile<'e, E: PgExecutor<'e>>(
    exec: E,
    id: UserId,
    username: Option<&str>,
    email: Option<&str>,
) -> DbResult<User> {
    let row: UserRow = sqlx::query_as(
        "UPDATE users SET \
            username = COALESCE($2, username), \
            email = COALESCE($3, email) \
         WHERE id = $1 \
         RETURNING id, email, username, role::text AS role, created_at",
    )
    .bind(id.as_uuid())
    .bind(username)
    .bind(email)
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

/// An active login session, derived from a live (unrevoked, unexpired) refresh token
/// (frontend §9.4 `GET /v1/me/sessions`). Only non-secret metadata is exposed.
#[derive(Debug, Clone)]
pub struct SessionInfo {
    pub id: Uuid,
    pub family_id: Uuid,
    pub created_at: OffsetDateTime,
    pub expires_at: OffsetDateTime,
}

/// List a user's active sessions (live refresh tokens), newest first.
pub async fn list_sessions<'e, E: PgExecutor<'e>>(
    exec: E,
    user_id: UserId,
) -> DbResult<Vec<SessionInfo>> {
    #[derive(FromRow)]
    struct Row {
        id: Uuid,
        family_id: Uuid,
        created_at: OffsetDateTime,
        expires_at: OffsetDateTime,
    }
    let rows: Vec<Row> = sqlx::query_as(
        "SELECT id, family_id, created_at, expires_at FROM refresh_tokens \
         WHERE user_id = $1 AND revoked_at IS NULL AND expires_at > now() \
         ORDER BY created_at DESC",
    )
    .bind(user_id.as_uuid())
    .fetch_all(exec)
    .await?;
    Ok(rows
        .into_iter()
        .map(|r| SessionInfo {
            id: r.id,
            family_id: r.family_id,
            created_at: r.created_at,
            expires_at: r.expires_at,
        })
        .collect())
}

/// Revoke one of the user's own sessions (its whole rotation family), scoped to ownership.
/// Returns the number of tokens revoked (0 if the session id was not the caller's).
pub async fn revoke_session<'e, E: PgExecutor<'e>>(
    exec: E,
    user_id: UserId,
    session_id: Uuid,
) -> DbResult<u64> {
    let result = sqlx::query(
        "UPDATE refresh_tokens SET revoked_at = now() \
         WHERE revoked_at IS NULL AND family_id = ( \
             SELECT family_id FROM refresh_tokens WHERE id = $2 AND user_id = $1 \
         )",
    )
    .bind(user_id.as_uuid())
    .bind(session_id)
    .execute(exec)
    .await?;
    Ok(result.rows_affected())
}

/// Read a user's notification preferences JSON (frontend §9.4). `{}` means "defaults".
pub async fn get_notification_prefs<'e, E: PgExecutor<'e>>(
    exec: E,
    id: UserId,
) -> DbResult<serde_json::Value> {
    let prefs: Option<serde_json::Value> =
        sqlx::query_scalar("SELECT notification_prefs FROM users WHERE id = $1")
            .bind(id.as_uuid())
            .fetch_optional(exec)
            .await?;
    Ok(prefs.unwrap_or_else(|| serde_json::json!({})))
}

/// Replace a user's notification preferences JSON (frontend §9.4).
pub async fn set_notification_prefs<'e, E: PgExecutor<'e>>(
    exec: E,
    id: UserId,
    prefs: &serde_json::Value,
) -> DbResult<()> {
    let result = sqlx::query("UPDATE users SET notification_prefs = $2 WHERE id = $1")
        .bind(id.as_uuid())
        .bind(prefs)
        .execute(exec)
        .await?;
    if result.rows_affected() == 0 {
        return Err(DbError::NotFound);
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Admin: user directory (frontend §9.5 Console Users tab)
// ---------------------------------------------------------------------------

/// One row of the operator Users table: identity, role, and how many series the user
/// tracks (frontend §9.5 `GET /v1/admin/users`).
#[derive(Debug, Clone, serde::Serialize, FromRow)]
pub struct UserRow2 {
    pub id: Uuid,
    pub email: String,
    pub username: String,
    /// RBAC role token (`user` | `operator` | `admin`).
    pub role: String,
    pub tracked_count: i64,
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
}

/// List users for the operator console, newest first.
pub async fn list_users<'e, E: PgExecutor<'e>>(exec: E, limit: i64) -> DbResult<Vec<UserRow2>> {
    let rows: Vec<UserRow2> = sqlx::query_as(
        "SELECT u.id, u.email::text AS email, u.username::text AS username, \
                u.role::text AS role, u.created_at, \
                (SELECT count(*) FROM watchlist_entries w WHERE w.user_id = u.id) AS tracked_count \
         FROM users u ORDER BY u.created_at DESC LIMIT $1",
    )
    .bind(limit)
    .fetch_all(exec)
    .await?;
    Ok(rows)
}
