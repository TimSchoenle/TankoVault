//! Repository layer; each module owns one aggregate's queries.
//!
//! Single-statement functions are generic over [`sqlx::PgExecutor`] and compose with a pool or
//! transaction; multi-statement ones take `&mut sqlx::PgConnection` and run inside one (see
//! [`catalog::ingest_series`]).

pub mod audit;
pub mod catalog;
pub mod flags;
pub mod gdpr;
pub mod matching;
pub mod permissions;
pub mod privacy;
pub mod providers;
pub mod recsys;
pub mod scans;
pub mod stats;
pub mod sync;
pub mod tracking;
pub mod tunables;
pub mod user_admin;
pub mod users;
