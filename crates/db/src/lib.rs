//! # tankovault-db
//!
//! SQLx-based persistence for `TankoVault`: the connection pool, the embedded migration set,
//! and the repository layer. Queries are the compile-time-checked `query!`/`query_as!`/
//! `query_scalar!` macros: every statement is verified against the real schema at build
//! time (column names, types, and nullability), while keeping full SQL control and no
//! ORM. Every worker write is idempotent (`ON CONFLICT`) per the design's at-least-once
//! invariant.
//!
//! Build-time database dependency is satisfied offline from the committed query cache in
//! `.sqlx/` (regenerated with `cargo run -p xtask -- sqlx-prepare`), so ordinary
//! `cargo build`/CI/Docker builds need no live database; set `SQLX_OFFLINE=true` to force
//! the cache even when `DATABASE_URL` is present. CI additionally runs the macros against a
//! real Postgres and fails if the cache is stale (`cargo sqlx prepare --check`).
//!
//! Domain enums are stored as native Postgres enums and mapped directly through the
//! `sqlx::Type` derive (gated behind `tankovault-domain`'s `sqlx` feature), so the enum
//! columns are read/written without `::text` casts and the domain crate stays
//! persistence-free for the WASM frontend, which builds with that feature off.

pub mod error;
pub mod pool;
pub mod repo;

pub use error::{DbError, DbResult};
pub use pool::{MIGRATOR, connect, migrate, reset};
pub use sqlx::PgPool;
