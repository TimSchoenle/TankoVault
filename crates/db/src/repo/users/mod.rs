//! User accounts and rotating refresh tokens: identity/session plumbing only (permissions live
//! in [`crate::repo::permissions`], admin in [`crate::repo::user_admin`]).
//!
//! Split into [`credentials`], [`refresh_tokens`], [`password_reset`], [`email_verification`],
//! [`passkeys`], [`profile`], [`sessions`] and [`citext`]; glob re-exported for `repo::users::…`.

pub mod citext;
pub mod credentials;
pub mod email_verification;
pub mod passkeys;
pub mod password_reset;
pub mod profile;
pub mod refresh_tokens;
pub mod sessions;

// `passkeys` is deliberately not re-exported: its `insert`/`delete`/`rename` fns are meaningless
// unqualified as `repo::users::delete(…)`. New callers spell the module.
pub use citext::*;
pub use credentials::*;
pub use email_verification::*;
pub use password_reset::*;
pub use profile::*;
pub use refresh_tokens::*;
pub use sessions::*;
