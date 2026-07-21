//! # tankovault-db
//!
//! SQLx-based persistence for `TankoVault`: the connection pool, the embedded migration set,
//! and the repository layer. Queries are runtime-checked `query`/`query_as` calls with
//! full SQL control and no build-time database dependency; every worker write is
//! idempotent (`ON CONFLICT`) per the design's at-least-once invariant.
//!
//! Domain enums are stored as native Postgres enums and read back via `::text` casts,
//! so `tankovault-domain` stays entirely persistence-free.

pub mod error;
pub mod pool;
pub mod repo;

pub use error::{DbError, DbResult};
pub use pool::{MIGRATOR, connect, migrate, reset};
pub use sqlx::PgPool;
