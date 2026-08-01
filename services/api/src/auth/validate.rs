//! The field validators every path that writes a credential column shares.
//!
//! They live in their own module because they are *not* only the registration handler's:
//! `patch_profile` and `admin::update_user` write the same columns, and the `@`-in-username
//! rule going unenforced on one of those paths is exactly the SEC-9 defect.

use crate::error::{ApiError, ApiResult};
use secrecy::{ExposeSecret as _, SecretString};

/// The character class a username may draw from.
///
/// `@` is excluded deliberately, and it is the whole point: `find_credentials` resolves a
/// login with `WHERE email = $1 OR username = $1` against two *separate* unique constraints,
/// so a username equal to another account's email made the query match two rows and
/// `fetch_optional` silently take an arbitrary one. The victim was then locked out with a
/// bare `401`, intermittently, depending on which row the planner returned.
///
/// Enforced in `validate_registration`, `patch_profile` and `admin::update_user` — every path
/// that can write the column.
const USERNAME_ALLOWED: fn(char) -> bool =
    |c: char| c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '-');

/// Upper bound on an accepted password, in bytes.
///
/// argon2id with `Params::default()` pins 19 MiB per verification. Without a cap, concurrent
/// registrations were bounded only by the 1 MiB body limit, which is a cheap memory-exhaustion
/// `DoS` on the API replica. 4096 is far above any real passphrase and far below anything that
/// costs measurable extra hashing time.
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
/// Deliberately shallow — the address is proven by the confirmation link, not by a regex —
/// but it does bound the length, which `patch_profile` previously did not: it accepted a
/// 1 MiB value, and an address with no `@` at all.
pub(crate) fn validate_email(email: &str) -> ApiResult<()> {
    if email.len() < 3 || email.len() > 254 || !email.contains('@') {
        return Err(ApiError::BadRequest("invalid email".into()));
    }
    Ok(())
}

/// Validate a password, for registration, reset and the authenticated change.
///
/// Takes the [`SecretString`] rather than a `&str` so the single `expose_secret` needed to
/// measure a length lives inside the validator instead of at each of its three call sites.
/// The error messages carry the *bound*, never the value.
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
        // The exploit: registering `victim@example.com` as a *username* made
        // `WHERE email = $1 OR username = $1` match two rows. Both halves are closed now —
        // `find_credentials` routes an identifier containing `@` to the email column only,
        // and this validator runs on registration, `PATCH /v1/me/profile` *and*
        // `PATCH /v1/admin/users/{id}`, which was the write path it previously missed.
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
