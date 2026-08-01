//! User administration: the directory, per-account detail, identity edits, suspension, forced
//! sign-out, permission grants and administrative erasure. The shared self-administration and
//! last-administrator refusals live in [`guards`]; re-exports are globs so `#[utoipa::path]`'s
//! sibling `__path_<handler>` items still resolve.

pub mod access;
pub mod deletion;
pub mod detail;
pub mod directory;
pub mod guards;
pub mod identity;
pub mod permissions;

pub use access::*;
pub use deletion::*;
pub use detail::*;
pub use directory::*;
pub use identity::*;
pub use permissions::*;

// The guards are crate-internal policy: every account-writing path calls them, but they are not
// part of the HTTP surface.
pub(crate) use guards::*;
