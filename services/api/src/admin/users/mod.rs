//! User administration: the directory, per-account detail, identity edits, suspension,
//! forced sign-out, permission grants and administrative erasure.
//!
//! # The rules that are enforced here and nowhere else
//!
//! Three refusals live in this module because they are properties of the *deployment*, not of
//! any single row, and no database constraint can express them:
//!
//! 1. **No self-administration.** Suspension, erasure and permission edits refuse to target the
//!    caller. Not because it is dangerous in itself but because it is the wrong endpoint:
//!    `/v1/me` is where someone acts on their own account, and an administrator who can quietly
//!    grant themselves a capability produces an audit trail nobody can rely on.
//! 2. **The last administrator is protected.** Revoking, suspending or erasing the final active
//!    holder of [`Permission::UsersPermissions`](tankovault_domain::Permission::UsersPermissions)
//!    leaves the deployment with no way to grant anything ever again, recoverable only by
//!    editing the database by hand. Every path that could do it checks first.
//! 3. **Erasure demands the username back.** It is irreversible and cascades across every table;
//!    typing the name is the difference between an administrator deciding and an administrator
//!    mis-clicking.
//!
//! Everything mutating is audited, including the refusals — an attempt to erase the last
//! administrator is exactly the event an operator wants to find later.
//!
//! # Layout (ARCH-19)
//!
//! Split out of a 657-line file along **which facet of an account a request administers**,
//! rather than one module per route — the same axis `repo::tracking` was split on, and the
//! reason `identity` holds two handlers while `deletion` holds one:
//!
//! | module | the facet it administers |
//! |---|---|
//! | [`directory`] | who exists — the searchable, paged operator list |
//! | [`detail`] | one account's read model, which is also *every* mutator's response |
//! | [`identity`] | who the account is: username, email, and whether that address is confirmed |
//! | [`access`] | whether it may act (suspension) and whether it is acting (sessions) |
//! | [`permissions`] | what it may do: the grant set, and the catalogue the editor renders |
//! | [`deletion`] | that it exists at all — the one irreversible action |
//! | [`guards`] | the two refusals above, shared by every path that can trip them |
//!
//! [`guards`] is the split that earns its keep rather than merely tidying, the way
//! [`crate::auth::validate`] was: those two checks are not the status handler's private
//! business. `access`, `permissions` and `deletion` all write columns whose invariant is "some
//! other account can still administer this deployment", and a fourth path added later that
//! forgets to call them is the same defect class as SEC-9. They are `pub(crate)` re-exported to
//! say that they are policy rather than HTTP surface.
//!
//! [`detail`] is shared for a different reason: every mutating handler answers with the re-read
//! account rather than echoing its input, so `UserDetailResponse` is infrastructure, not one
//! handler's return type.
//!
//! The re-exports are globs and the submodules are `pub` because `#[utoipa::path]` generates a
//! sibling `__path_<handler>` item that `routes!(admin::get_user)` in `lib.rs` resolves
//! alongside the handler. Naming the handlers individually would compile and then fail to
//! route.

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
