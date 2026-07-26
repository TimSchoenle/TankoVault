//! Operator/admin handlers, each gated on a named [`Permission`](tankovault_domain::Permission).
//!
//! Split by the surface each group serves rather than kept as one 1400-line file, so a
//! reviewer reading the provider CRUD is not also holding the sync reconciliation in their
//! head. Every handler is gated by [`AuthUser::require`](crate::state::AuthUser::require),
//! which audits refusals, and every mutating action writes a structured audit record
//! (design §16).
//!
//! Authorization is per-capability, not per-tier: a handler asks for the specific thing it
//! does (`Permission::ProvidersDelete`), so "who can delete a provider" is answerable by
//! reading one line rather than inferring it from a role ordering. See
//! [`tankovault_domain::permissions`].
//!
//! The re-exports are globs rather than named lists on purpose: `utoipa`'s `routes!` macro
//! resolves a hidden `__path_<handler>` type alongside each handler, so a named re-export
//! would compile here and then fail at the route table. The glob keeps
//! `crate::admin::list_providers` resolving exactly as it did before the split.

mod flags;
mod merge;
mod privacy;
mod providers;
mod scans;
mod sync;
mod system;
mod users;

pub use flags::*;
pub use merge::*;
pub use privacy::*;
pub use providers::*;
pub use scans::*;
pub use sync::*;
pub use system::*;
pub use users::*;
