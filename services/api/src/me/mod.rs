//! Authenticated user endpoints, scoped to the token's `user_id` so one user can never read
//! another's rows.
//!
//! Re-exports are globs because `utoipa`'s `routes!` macro also resolves a hidden
//! `__path_<handler>` type per handler; see [`crate::admin`].

mod account;
mod capabilities;
mod content;
mod dashboard;
// `pub(crate)`, not private: `/v1/admin/stream` reuses this module's ticket query type and
// its SSE gauge guard, so both kinds of stream land in one metric.
pub(crate) mod notifications;
mod passkeys;
mod privacy;
mod progress;
mod recommendations;
mod source_prefs;
mod sync;
mod watchlist;

pub use account::*;
pub use capabilities::*;
pub use content::*;
pub use dashboard::*;
pub use notifications::*;
pub use passkeys::*;
pub use privacy::*;
pub use progress::*;
pub use recommendations::*;
pub use source_prefs::*;
pub use sync::*;
pub use watchlist::*;
