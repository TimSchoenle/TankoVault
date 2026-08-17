//! The field validators every path that writes a user-supplied column shares — credentials for
//! `patch_profile` and `admin::update_user` as well as registration, plus the length bound on
//! the free-text fields (notes, reasons, details) and the range on a submitted chapter number.

use crate::error::{ApiError, ApiResult};
use secrecy::{ExposeSecret as _, SecretString};
use tankovault_domain::chapter_number::{MAX_CHAPTER_NUMBER, is_storable};

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

/// Upper bound on a free-text field a caller writes into a stored column — a privacy request's
/// detail, an operator's resolution note or extension reason, a catalogue title.
///
/// `security.max_body_bytes` (1 MiB by default) is the only thing that bounded these before, and
/// it is the wrong instrument twice over: it is a whole-request ceiling rather than a per-field
/// one, and a megabyte of text is not a value any of these fields is *for*. The one that bites
/// hardest is a title, which feeds the `gin_trgm_ops` indexes migration 0033 added — a megabyte
/// of text there is a million trigram entries in a shared index.
pub(crate) const MAX_FREE_TEXT: usize = 4096;

/// Validate a free-text field, for every path that writes one into a stored column.
///
/// Bounds the length only. What the text *says* is the operator's or the subject's business, and
/// the storage layer is parameterised, so there is nothing else to check here. `field` names the
/// field in the error so a rejected request says which one was too long.
///
/// # Errors
/// [`ApiError::BadRequest`] when `value` exceeds [`MAX_FREE_TEXT`] bytes.
pub(crate) fn validate_free_text(field: &str, value: &str) -> ApiResult<()> {
    if value.len() > MAX_FREE_TEXT {
        return Err(ApiError::BadRequest(format!(
            "{field} must be at most {MAX_FREE_TEXT} characters"
        )));
    }
    Ok(())
}

/// Validate a caller-submitted chapter number before it reaches a `numeric(10,4)` column.
///
/// The progress endpoints take the number straight off the wire — a JSON body field or, for
/// `mark_chapter`, a **path segment**, which `f64::from_str` will happily read as `inf` or `NaN`.
/// Unchecked, each of those is a `numeric field overflow` or a "cannot convert NaN to numeric"
/// raised inside the repo layer, which the API can only surface as a 500. The caller sent a bad
/// value; that is a 400.
///
/// # Errors
/// [`ApiError::BadRequest`] when `number` is non-finite, negative, or past
/// [`MAX_CHAPTER_NUMBER`].
pub(crate) fn validate_chapter_number(number: f64) -> ApiResult<()> {
    if !is_storable(number) {
        return Err(ApiError::BadRequest(format!(
            "chapter number must be between 0 and {MAX_CHAPTER_NUMBER}"
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

    #[test]
    fn free_text_is_bounded_and_names_its_field() {
        assert!(validate_free_text("note", "why it was rejected").is_ok());
        assert!(validate_free_text("note", &"x".repeat(MAX_FREE_TEXT)).is_ok());
        let err = validate_free_text("note", &"x".repeat(MAX_FREE_TEXT + 1)).unwrap_err();
        assert!(
            matches!(&err, ApiError::BadRequest(m) if m.contains("note")),
            "the error has to name the field: {err:?}"
        );
    }

    /// The exploit: `PUT /v1/me/progress/{id}/chapters/NaN`. `f64::from_str` accepts `"NaN"` and
    /// `"inf"`, so an unvalidated path segment reached `numeric(10,4)` and the repo layer raised
    /// a 500 on what is plainly a malformed request.
    #[test]
    fn a_submitted_chapter_number_is_bounded_at_both_ends() {
        assert!(validate_chapter_number(1050.5).is_ok());
        assert!(validate_chapter_number(0.0).is_ok());
        assert!(validate_chapter_number("NaN".parse::<f64>().expect("parses")).is_err());
        assert!(validate_chapter_number("inf".parse::<f64>().expect("parses")).is_err());
        assert!(validate_chapter_number(-1.0).is_err());
        assert!(validate_chapter_number(20_250_817.0).is_err());
    }
}
