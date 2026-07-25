//! Authenticated user endpoints.
//!
//! Ownership is enforced implicitly throughout: every query is scoped to the token's
//! `user_id`, so there is no path by which one user reads another's rows.
//!
//! Split by surface (watchlist, progress, dashboard, account, notifications, sync,
//! privacy). The re-exports are globs because `utoipa`'s `routes!` macro also resolves a
//! hidden `__path_<handler>` type per handler; see the note in [`crate::admin`].

mod account;
mod dashboard;
mod notifications;
mod privacy;
mod progress;
mod sync;
mod watchlist;

pub use account::*;
pub use dashboard::*;
pub use notifications::*;
pub use privacy::*;
pub use progress::*;
pub use sync::*;
pub use watchlist::*;
