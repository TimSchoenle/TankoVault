//! Operator/admin handlers (RBAC-gated).
//!
//! Split by the surface each group serves rather than kept as one 1400-line file, so a
//! reviewer reading the provider CRUD is not also holding the sync reconciliation in their
//! head. Every handler is gated by [`AuthUser::require`](crate::state::AuthUser::require),
//! which audits refusals, and every mutating action writes a structured audit record
//! (design §16).
//!
//! The re-exports are globs rather than named lists on purpose: `utoipa`'s `routes!` macro
//! resolves a hidden `__path_<handler>` type alongside each handler, so a named re-export
//! would compile here and then fail at the route table. The glob keeps
//! `crate::admin::list_providers` resolving exactly as it did before the split.

mod merge;
mod providers;
mod scans;
mod sync;
mod system;

pub use merge::*;
pub use providers::*;
pub use scans::*;
pub use sync::*;
pub use system::*;
