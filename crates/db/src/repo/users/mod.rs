//! User accounts and rotating refresh tokens: identity/session plumbing only (permissions live
//! in [`crate::repo::permissions`], admin in [`crate::repo::user_admin`]).
//!
//! Split into [`credentials`], [`refresh_tokens`], [`password_reset`], [`email_verification`],
//! [`webauthn`], [`mfa`], [`profile`], [`sessions`], [`source_prefs`] and [`citext`]; glob
//! re-exported for `repo::users::…`.

pub mod citext;
pub mod credentials;
pub mod email_verification;
pub mod mfa;
pub mod password_reset;
pub mod profile;
pub mod refresh_tokens;
pub mod sessions;
pub mod source_prefs;
pub mod webauthn;

// `webauthn` and `mfa` are deliberately not re-exported: their `insert`/`delete`/`rename` fns
// are meaningless unqualified as `repo::users::delete(…)`. New callers spell the module.
pub use citext::*;
pub use credentials::*;
pub use email_verification::*;
pub use password_reset::*;
pub use profile::*;
pub use refresh_tokens::*;
pub use sessions::*;
pub use source_prefs::*;
