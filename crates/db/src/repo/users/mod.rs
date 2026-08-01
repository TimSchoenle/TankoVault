//! User accounts and rotating refresh tokens.
//!
//! The `auth` crate owns hashing; this layer stores only the argon2id `password_hash`
//! and the SHA-256 `token_hash`, never plaintext secrets.
//!
//! What a user is *allowed* to do is not here: permission grants live in
//! [`crate::repo::permissions`] and the operator-facing administration of accounts lives in
//! [`crate::repo::user_admin`]. This module is identity and session plumbing only.
//!
//! # Layout (ARCH-19)
//!
//! Split at the banner comments the old 749-line file already carried, one module per
//! credential lifecycle:
//!
//! | module | owns |
//! |---|---|
//! | [`credentials`] | the `users` row, registration, and the login lookup |
//! | [`refresh_tokens`] | rotation lineages and reuse detection |
//! | [`password_reset`] | the forgot-password token and the password column |
//! | [`email_verification`] | the sign-up confirmation token and the verified flag |
//! | [`passkeys`] | `WebAuthn` credentials and the ceremony state that mints them |
//! | [`profile`] | self-service identity changes and notification preferences |
//! | [`sessions`] | sessions as the user sees them, and the three ways they end |
//! | [`citext`] | binding a string so a comparison against a `citext` column honours the schema |
//!
//! The modules are public and the glob re-exports below keep every existing
//! `repo::users::…` path resolving, so callers can move to the narrower
//! `repo::users::sessions::…` spelling one at a time rather than in one sweep.
//!
//! Two placements are not what the banners said, and both are deliberate. The old "account
//! settings" banner held profile edits *and* session management, which are different
//! aggregates read by different handlers, so it became two modules. And `revoke_all_for_user`
//! sat under "password reset" because that is its caller; it lives in [`sessions`] with
//! `revoke_all_sessions`, because the two are the same statement differing only in whether the
//! count is returned and separating them is how one gets fixed and the other does not.
//!
//! [`citext`] is a seventh module the report did not ask for. It exists because four of the
//! other six compare a bound string against a `citext` column, and getting that binding wrong
//! silently made every lookup by email or username case-sensitive — see the type's own
//! documentation.

pub mod citext;
pub mod credentials;
pub mod email_verification;
pub mod passkeys;
pub mod password_reset;
pub mod profile;
pub mod refresh_tokens;
pub mod sessions;

// [`passkeys`] is deliberately **not** re-exported. The globs below exist to keep pre-split
// `repo::users::…` paths resolving; there are no such paths for a module that did not exist
// before the split, and its functions are named `insert`, `delete` and `rename` — three words
// that mean nothing at `repo::users::delete(…)` and everything at
// `repo::users::passkeys::delete(…)`. New callers spell the module.
pub use citext::*;
pub use credentials::*;
pub use email_verification::*;
pub use password_reset::*;
pub use profile::*;
pub use refresh_tokens::*;
pub use sessions::*;
