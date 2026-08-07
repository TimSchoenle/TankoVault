//! Database error type. Variants map to HTTP status in `services/api`: [`DbError::NotFound`]
//! to 404, [`DbError::Conflict`] to 409, everything else to 500.
//!
//! An unrecognised enum token decodes as `Sqlx(ColumnDecode)` (500); there is no dedicated
//! variant for it.

/// Errors surfaced by the repository layer.
#[derive(Debug, thiserror::Error)]
pub enum DbError {
    /// A `SQLx` driver/query error.
    #[error(transparent)]
    Sqlx(#[from] sqlx::Error),
    /// A migration failed to apply.
    #[error(transparent)]
    Migrate(#[from] sqlx::migrate::MigrateError),
    /// A uniqueness or lookup expectation was violated at the application layer.
    #[error("{0}")]
    Conflict(String),
    /// An expected row was not found.
    #[error("not found")]
    NotFound,
    /// A typed document could not be encoded for a `jsonb` column.
    #[error(transparent)]
    Serialization(#[from] serde_json::Error),
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
