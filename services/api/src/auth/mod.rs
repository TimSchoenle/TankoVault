//! Authentication handlers: register, login, refresh (rotating + reuse-detecting), logout,
//! the password-reset flow and email confirmation.
//!
//! # Layout (ARCH-19)
//!
//! One module per flow, split out of a 743-line file:
//!
//! | module | owns |
//! |---|---|
//! | [`register`] | account creation, and the confirmed/unconfirmed fork |
//! | [`login`] | sign-in, including the constant-time unknown-identifier branch |
//! | [`passkey`] | passwordless sign-in with a `WebAuthn` discoverable credential |
//! | [`session`] | the rotating refresh cookie, reuse detection, and the token mint |
//! | [`password`] | request and consume a password-reset link |
//! | [`verification`] | the email-confirmation link and resending it |
//! | [`validate`] | the field validators every credential-writing path shares |
//!
//! `validate` is the split that matters rather than merely tidying: those validators are not
//! the registration handler's private business — `patch_profile` and `admin::update_user`
//! write the same columns, and the `@`-in-username rule going unenforced on one of those
//! paths is exactly what SEC-9 was.
//!
//! The re-exports are globs because `#[utoipa::path]` generates a sibling `__path_<handler>`
//! item that `routes!(auth::login)` in `lib.rs` resolves alongside the handler. Naming the
//! handlers individually would compile and then fail to route.

pub mod login;
pub mod passkey;
pub mod password;
pub mod register;
pub mod session;
pub mod validate;
pub mod verification;

pub use login::*;
pub use passkey::*;
pub use password::*;
pub use register::*;
pub use session::*;
pub use verification::*;

// The validators are crate-internal: `patch_profile` and `admin::update_user` call them, but
// they are not part of the HTTP surface.
pub(crate) use validate::*;
