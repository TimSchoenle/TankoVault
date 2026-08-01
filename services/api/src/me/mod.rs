//! Authenticated user endpoints, scoped to the token's `user_id` so one user can never read
//! another's rows.
//!
//! Re-exports are globs because `utoipa`'s `routes!` macro also resolves a hidden
//! `__path_<handler>` type per handler; see [`crate::admin`].

mod account;
mod capabilities;
mod dashboard;
mod notifications;
mod passkeys;
mod privacy;
mod progress;
mod sync;
mod watchlist;

pub use account::*;
pub use capabilities::*;
pub use dashboard::*;
pub use notifications::*;
pub use passkeys::*;
pub use privacy::*;
pub use progress::*;
pub use sync::*;
pub use watchlist::*;
