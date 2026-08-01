//! The field validators every path that writes a credential column shares, including
//! `patch_profile` and `admin::update_user`, not just registration.

use crate::error::{ApiError, ApiResult};
use secrecy::{ExposeSecret as _, SecretString};

/// The character class a username may draw from — `@` excluded so a username can never
/// collide with `WHERE email = $1 OR username = $1`'s two separate unique constraints, which
/// let `fetch_optional` silently pick an arbitrary row and lock out the real owner.
///
/// Enforced everywhere the column can be written: registration, `patch_profile`,
/// `admin::update_user`.
const USERNAME_ALLOWED: fn(char) -> bool =
    |c: char| c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '-');

/// Upper bound on an accepted password, in bytes: argon2id pins ~19 MiB per verification, so
/// an unbounded length is a cheap memory-exhaustion `DoS` on the API replica. Far above any
/// real passphrase, far below anything that costs measurable extra hashing time.
pub(crate) const MAX_PASSWORD_LEN: usize = 4096;

/// Validate a username, for every path that writes the column.
pub(crate) fn validate_username(username: &str) -> ApiResult<()> {
    if username.len() < 3 || username.len() > 32 {
        return Err(ApiError::BadRequest(
            "username must be 3–32 characters".into(),
        ));
    }
    if !username.chars().all(USERNAME_ALLOWED) {
        return Err(ApiError::BadRequest(
            "username may contain only letters, digits, '_', '.' and '-'".into(),
        ));
    }
    Ok(())
}

/// Validate an email address, for every path that writes the column.
///
/// Deliberately shallow — proven by the confirmation link, not a regex — but bounds the
/// length, which `patch_profile` previously did not.
pub(crate) fn validate_email(email: &str) -> ApiResult<()> {
    if email.len() < 3 || email.len() > 254 || !email.contains('@') {
        return Err(ApiError::BadRequest("invalid email".into()));
    }
    Ok(())
}

/// Validate a password, for registration, reset and the authenticated change.
///
/// Takes [`SecretString`] so the one `expose_secret` needed lives in the validator, not at
/// each call site. Error messages carry the bound, never the value.
pub(crate) fn validate_password(password: &SecretString) -> ApiResult<()> {
    let len = password.expose_secret().len();
    if len < 8 {
        return Err(ApiError::BadRequest(
            "password must be at least 8 characters".into(),
        ));
    }
    if len > MAX_PASSWORD_LEN {
        return Err(ApiError::BadRequest(format!(
            "password must be at most {MAX_PASSWORD_LEN} characters"
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_username_may_not_contain_an_at_sign() {
        // The exploit: `victim@example.com` as a *username* matched `WHERE email = $1 OR
        // username = $1` on two rows. Now closed: `@` routes to the email column, and this
        // validator runs on every write path, including `admin::update_user`.
        assert!(validate_username("victim@example.com").is_err());
        assert!(validate_username("aster").is_ok());
        assert!(validate_username("as.ter_1-x").is_ok());
        assert!(validate_username("no spaces").is_err());
        assert!(validate_username("ünïcode").is_err());
        assert!(validate_username("ab").is_err());
        assert!(validate_username(&"a".repeat(33)).is_err());
    }

    #[test]
    fn a_password_is_bounded_at_both_ends() {
        assert!(validate_password(&SecretString::from("short")).is_err());
        assert!(validate_password(&SecretString::from("long enough")).is_ok());
        // Unbounded length meant a 1 MiB password pinned 19 MiB of argon2 memory per request.
        assert!(validate_password(&SecretString::from("x".repeat(MAX_PASSWORD_LEN))).is_ok());
        assert!(validate_password(&SecretString::from("x".repeat(MAX_PASSWORD_LEN + 1))).is_err());
    }

    #[test]
    fn an_email_is_bounded_and_must_look_like_one() {
        assert!(validate_email("a@b").is_ok());
        assert!(validate_email("no-at-sign").is_err());
        // patch_profile previously accepted a 1 MiB value with no `@`.
        assert!(validate_email(&format!("{}@x", "a".repeat(300))).is_err());
    }
}
