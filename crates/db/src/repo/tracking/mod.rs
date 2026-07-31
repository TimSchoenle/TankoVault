//! User tracking: watchlist, read progress, notifications and the Home read models.
//!
//! # Layout (ARCH-5)
//!
//! "Tracking" was a folder name rather than an aggregate: one 1,094-line module held seven
//! unrelated things whose only common ground was a `user_id` column, and whose consumers are
//! disjoint. It is now four modules that match those consumers:
//!
//! | module | owns | read by |
//! |---|---|---|
//! | [`watchlist`] | which series a user tracks, at what status | `services/api` |
//! | [`progress`] | the read frontier and per-series sync exclusion | `services/api`, `services/sync` |
//! | [`notifications`] | notifications and the fan-out primitives | `services/notifier` |
//! | [`dashboard`] | the Home read models (feed, continue, stats, recommendations) | `services/api` |
//!
//! The modules are public and the glob re-exports below keep every existing
//! `repo::tracking::…` path resolving, so callers can narrow to
//! `repo::tracking::notifications::…` one at a time.

pub mod dashboard;
pub mod notifications;
pub mod progress;
pub mod watchlist;

pub use dashboard::*;
pub use notifications::*;
pub use progress::*;
pub use watchlist::*;
