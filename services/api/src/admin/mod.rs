//! Operator/admin handlers, each gated on a named [`Permission`](tankovault_domain::Permission).
//!
//! Re-exports are globs, not named lists: `utoipa`'s `routes!` macro resolves a hidden
//! `__path_<handler>` type alongside each handler, so a named re-export would compile here and
//! then fail at the route table.

mod decisions;
mod flags;
mod merge;
mod privacy;
mod providers;
mod recommendations;
mod scans;
mod stream;
mod sync;
mod system;
mod users;

pub use decisions::*;
pub use flags::*;
pub use merge::*;
pub use privacy::*;
pub use providers::*;
pub use recommendations::*;
pub use scans::*;
pub use stream::*;
pub use sync::*;
pub use system::*;
pub use users::*;
