//! Authentication handlers — register, login, refresh, logout, password-reset, email
//! confirmation — and the field validators they share with other credential-writing paths.
//! Re-exports are globs so `#[utoipa::path]`'s generated `__path_<handler>` items resolve
//! alongside each handler in `lib.rs`'s route table.

pub mod login;
pub mod mfa;
pub mod passkey;
pub mod password;
pub mod register;
pub mod session;
pub mod validate;
pub mod verification;

pub use login::*;
pub use mfa::*;
pub use passkey::*;
pub use password::*;
pub use register::*;
pub use session::*;
pub use verification::*;

// Crate-internal: shared by other handlers, not part of the HTTP surface.
pub(crate) use validate::*;
