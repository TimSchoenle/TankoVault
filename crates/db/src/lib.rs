//! SQLx-based persistence for `TankoVault`: the connection pool, the embedded migration set,
//! and the repository layer.

pub mod error;
pub mod pool;
pub mod repo;

pub use error::{DbError, DbResult};
pub use pool::{
    MIGRATOR, PoolSettings, begin, begin_with_statement_timeout, commit, connect, connect_with,
    migrate, reset,
};
pub use sqlx::{PgExecutor, PgPool};
