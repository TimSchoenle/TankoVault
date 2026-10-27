//! Scan run + task repository. The task row is the truth for progress/audit; `JetStream` is
//! the truth for dispatch, with a `FOR UPDATE SKIP LOCKED` claim path as fallback.
//!
//! Settle-once: every claim/settle statement excludes the same three terminal states, so a
//! task counts toward `done_tasks`/`failed_tasks` at most once (`tests/repo_scans.rs` pins this).

mod activity;
mod cancel;
mod reconcile;
mod run_detail;
mod runs;
mod tasks;

pub use activity::*;
pub use cancel::*;
pub use reconcile::*;
pub use run_detail::*;
pub use runs::*;
pub use tasks::*;
