//! Database error type.
//!
//! The variants are not interchangeable at the edge: `services/api` maps [`DbError::NotFound`]
//! to 404 and [`DbError::Conflict`] to 409, and everything else to 500. That mapping is why
//! every repository function's `# Errors` section names *which* variants it can produce rather
//! than saying it returns an error — the variant is the status code.
//!
//! There is deliberately **no** variant for a row holding an enum token this build does not
//! recognise. Domain enums derive `sqlx::Type` against a native Postgres enum, so an
//! unrecognised token is decoded by the driver and arrives as
//! `Sqlx(sqlx::Error::ColumnDecode)` — a 500, which is the right answer for schema/code drift.
//! A dedicated variant existed here until it was removed: nothing in the workspace could
//! construct it (no repository parses an enum from text) and nothing matched on it, so it
//! documented a failure mode that could not occur while naming the one that could.

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
