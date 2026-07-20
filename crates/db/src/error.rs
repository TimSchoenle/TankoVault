//! Database error type.

use tankovault_domain::ParseEnumError;

/// Errors surfaced by the repository layer.
#[derive(Debug, thiserror::Error)]
pub enum DbError {
    /// A `SQLx` driver/query error.
    #[error(transparent)]
    Sqlx(#[from] sqlx::Error),
    /// A migration failed to apply.
    #[error(transparent)]
    Migrate(#[from] sqlx::migrate::MigrateError),
    /// A row held an enum token this build does not recognise (schema/code drift).
    #[error("failed to decode enum from database: {0}")]
    Enum(#[from] ParseEnumError),
    /// A uniqueness or lookup expectation was violated at the application layer.
    #[error("{0}")]
    Conflict(String),
    /// An expected row was not found.
    #[error("not found")]
    NotFound,
}

impl DbError {
    /// True when the underlying error is a Postgres unique-violation (SQLSTATE 23505).
    #[must_use]
    pub fn is_unique_violation(&self) -> bool {
        matches!(self, Self::Sqlx(sqlx::Error::Database(db)) if db.code().as_deref() == Some("23505"))
    }
}

/// Convenient result alias for the repository layer.
pub type DbResult<T> = Result<T, DbError>;
