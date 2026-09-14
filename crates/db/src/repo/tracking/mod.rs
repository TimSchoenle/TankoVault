//! User tracking: watchlist, read progress, notifications and the Home read models.
//!
//! Split into [`watchlist`], [`progress`], [`notifications`], [`dashboard`] and [`unread`]; glob
//! re-exported here so `repo::tracking::…` paths still resolve.

pub mod dashboard;
pub mod notifications;
pub mod progress;
pub mod unread;
pub mod watchlist;

pub use dashboard::*;
pub use notifications::*;
pub use progress::*;
pub use watchlist::*;
