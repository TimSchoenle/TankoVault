//! Repository layer. Each module owns one aggregate's queries.
//!
//! Functions that run a single statement are generic over [`sqlx::PgExecutor`], so they
//! compose with either a pool or a transaction. Functions that run several statements
//! take `&mut sqlx::PgConnection` and are driven inside a transaction (see
//! [`catalog::ingest_series`]).

pub mod audit;
pub mod catalog;
pub mod flags;
pub mod gdpr;
pub mod matching;
pub mod permissions;
pub mod privacy;
pub mod providers;
pub mod scans;
pub mod stats;
pub mod sync;
pub mod tracking;
pub mod user_admin;
pub mod users;
