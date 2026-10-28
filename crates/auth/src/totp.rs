//! Time-based one-time passwords (RFC 6238), the second factor an authenticator app provides.
//!
//! Implemented here, not taken from a crate: the algorithm is an HMAC, a truncation and a
//! modulo, and the tests below run the RFC 6238 Appendix B vectors that prove it correct. A
//! dependency for those three steps would add a supply-chain edge and save nothing.
//!
//! # What this module does not decide
//!
//! Replay protection is the caller's job, and it is not optional. A code is valid for its whole
//! 30-second step, and [`verify`] accepts one step of skew either side, so the same six digits
//! answer for up to 90 seconds. [`verify`] takes the last accepted step and refuses anything at
//! or below it, but only the caller can persist that value (`user_totp.last_step`). Passing
//! `None` every time re-opens the replay window.

use hmac::{Hmac, KeyInit as _, Mac as _};
use percent_encoding::{AsciiSet, CONTROLS, utf8_percent_encode};
use rand::Rng as _;
use secrecy::{ExposeSecret as _, SecretSlice, SecretString};
use sha1::Sha1;
use subtle::ConstantTimeEq as _;
use time::OffsetDateTime;

/// The length of a generated shared secret, in bytes.
///
/// 160 bits: the RFC 4226 §4 minimum, the HMAC-SHA1 output size, and the size every
/// authenticator app accepts. Some apps silently truncate or reject longer secrets, which gains
/// no security and costs a support ticket.
pub const SECRET_LEN: usize = 20;

/// The digits in a generated code.
pub const DIGITS: usize = 6;

/// `10^DIGITS`, the modulus the truncated HMAC is reduced by.
///
/// Spelled out so the reduction is a constant. A test asserts it matches [`DIGITS`], because a
/// drifted modulus produces codes of the right length and the wrong value.
const MODULUS: u32 = 1_000_000;

/// The seconds per time step.
pub const STEP_SECONDS: i64 = 30;

/// How many steps either side of the current one are accepted.
///
/// One, a 90-second window. The phone and the server keep independent clocks, and neither is
/// guaranteed to run NTP: zero skew fails a device a few seconds fast every time. More than one
/// step keeps a shoulder-surfed code usable for minutes.
const SKEW_STEPS: i64 = 1;

/// The characters escaped in the `otpauth://` label and issuer: controls, space, and the
/// delimiters that would re-split the label or end a query value.
///
/// The label is `issuer:account`, so an unescaped `:` in either half re-splits it and the
/// authenticator files the entry under the wrong name. Both halves are text this service does
/// not choose: an operator's issuer, a user's username.
const LABEL_ESCAPE: &AsciiSet = &CONTROLS
    .add(b' ')
    .add(b'"')
    .add(b'#')
    .add(b'%')
    .add(b'&')
    .add(b'/')
    .add(b':')
    .add(b'<')
    .add(b'=')
    .add(b'>')
    .add(b'?')
    .add(b'@')
    .add(b'\\')
    .add(b'^')
    .add(b'`')
    .add(b'{')
    .add(b'|')
    .add(b'}');

/// Generates a fresh shared secret.
#[must_use]
pub fn generate_secret() -> SecretSlice<u8> {
    let mut bytes = [0u8; SECRET_LEN];
    rand::rng().fill_bytes(&mut bytes);
    SecretSlice::from(bytes.to_vec())
}

/// Returns the secret as unpadded upper-case base32 (RFC 4648), for typing into an
/// authenticator app that cannot scan a QR code.
#[must_use]
pub fn encode_secret(secret: &SecretSlice<u8>) -> SecretString {
    SecretString::from(data_encoding::BASE32_NOPAD.encode(secret.expose_secret()))
}

/// Returns the `otpauth://` provisioning URI a QR code encodes.
///
/// It carries the shared secret in a query parameter, so it is a [`SecretString`] and must not
/// reach a log line or an error body.
///
/// `algorithm`, `digits` and `period` are stated even though they are the defaults: several
/// authenticators default differently from the RFC, and a mismatch produces wrong codes every
/// time with no error anywhere.
#[must_use]
pub fn provisioning_uri(issuer: &str, account: &str, secret: &SecretSlice<u8>) -> SecretString {
    let issuer_esc = utf8_percent_encode(issuer, LABEL_ESCAPE);
    let account_esc = utf8_percent_encode(account, LABEL_ESCAPE);
    let secret_b32 = encode_secret(secret);
    SecretString::from(format!(
        "otpauth://totp/{issuer_esc}:{account_esc}?secret={}&issuer={issuer_esc}\
         &algorithm=SHA1&digits={DIGITS}&period={STEP_SECONDS}",
        secret_b32.expose_secret(),
    ))
}

/// Returns the time step `at` falls in.
#[must_use]
pub fn step_at(at: OffsetDateTime) -> i64 {
    at.unix_timestamp().div_euclid(STEP_SECONDS)
}

/// Returns the code this secret produces at `step`.
///
/// Public so a test driving a live sign-in can produce an accepted code without a second
/// implementation that could disagree with this one.
#[must_use]
pub fn code_at_step(secret: &SecretSlice<u8>, step: i64) -> SecretString {
    // HMAC accepts any key length (short keys are padded, long ones hashed), so
    // `new_from_slice` cannot fail here.
    let mut mac = <Hmac<Sha1>>::new_from_slice(secret.expose_secret())
        .unwrap_or_else(|_| unreachable!("HMAC accepts a key of any length"));
    mac.update(&step.to_be_bytes());
    let digest = mac.finalize().into_bytes();

    // RFC 4226 §5.4 dynamic truncation: the low nibble of the last byte picks a 4-byte window,
    // whose high bit is cleared so the result is sign-independent across implementations.
    let offset = usize::from(digest[digest.len() - 1] & 0x0f);
    let binary = u32::from_be_bytes([
        digest[offset] & 0x7f,
        digest[offset + 1],
        digest[offset + 2],
        digest[offset + 3],
    ]);

    SecretString::from(format!("{:0width$}", binary % MODULUS, width = DIGITS))
}

/// Checks `code` against `secret`, returning the time step it was accepted at.
///
/// `last_step` is the step this secret was last accepted at, or `None` if never; anything at or
/// below it is refused as a replay. The caller persists the returned step (see the module doc).
///
/// A wrong, malformed or replayed code is `None`, without saying which: the caller answers
/// "that code is not valid" either way.
#[must_use]
pub fn verify(
    secret: &SecretSlice<u8>,
    code: &SecretString,
    at: OffsetDateTime,
    last_step: Option<i64>,
) -> Option<i64> {
    let presented = code.expose_secret().trim();
    if presented.len() != DIGITS || !presented.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }

    let current = step_at(at);
    for step in (current - SKEW_STEPS)..=(current + SKEW_STEPS) {
        if last_step.is_some_and(|seen| step <= seen) {
            continue;
        }
        // Constant-time against the code, which derives from the secret. Which step matched
        // is the clock, not a secret, so the loop may exit early.
        let expected = code_at_step(secret, step);
        if bool::from(
            expected
                .expose_secret()
                .as_bytes()
                .ct_eq(presented.as_bytes()),
        ) {
            return Some(step);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::{
        DIGITS, MODULUS, SECRET_LEN, STEP_SECONDS, code_at_step, encode_secret, generate_secret,
        provisioning_uri, step_at, verify,
    };
    use secrecy::{ExposeSecret as _, SecretSlice, SecretString};
    use time::OffsetDateTime;

    /// The 20-byte ASCII seed RFC 6238 Appendix B uses for its HMAC-SHA1 vectors.
    fn rfc_secret() -> SecretSlice<u8> {
        SecretSlice::from(b"12345678901234567890".to_vec())
    }

    fn at(unix: i64) -> OffsetDateTime {
        OffsetDateTime::from_unix_timestamp(unix).expect("a valid unix timestamp")
    }

    /// RFC 6238 Appendix B, the HMAC-SHA1 column, truncated to this module's six digits.
    ///
    /// An implementation that drifts from the RFC (a signed truncation, the wrong counter byte
    /// order, a modulo off by a power of ten) does not fail loudly: it produces codes no
    /// authenticator agrees with. These vectors are what catch it.
    #[test]
    fn the_rfc_6238_test_vectors_reproduce_exactly() {
        // (unix time, the RFC's 8-digit value; the low six digits are what this module emits)
        let vectors = [
            (59_i64, "94287082"),
            (1_111_111_109, "07081804"),
            (1_111_111_111, "14050471"),
            (1_234_567_890, "89005924"),
            (2_000_000_000, "69279037"),
            (20_000_000_000, "65353130"),
        ];
        let secret = rfc_secret();
        for (unix, eight_digits) in vectors {
            let expected = &eight_digits[eight_digits.len() - DIGITS..];
            let got = code_at_step(&secret, step_at(at(unix)));
            assert_eq!(
                got.expose_secret(),
                expected,
                "RFC 6238 vector at t={unix} must reproduce"
            );
        }
    }

    /// A code accepted at one step must never be accepted at that step again.
    ///
    /// The bug: without the `last_step` guard, a code observed once (read off a screen,
    /// captured by a proxy, replayed from a log) stays valid for the rest of its window plus
    /// the skew either side. That is ample for an attacker who already holds the password.
    #[test]
    fn a_code_is_refused_at_a_step_already_accepted() {
        let secret = rfc_secret();
        let now = at(1_234_567_890);
        let step = step_at(now);
        let code = code_at_step(&secret, step);

        let accepted = verify(&secret, &code, now, None).expect("a fresh code verifies");
        assert_eq!(accepted, step);
        assert!(
            verify(&secret, &code, now, Some(accepted)).is_none(),
            "the same code must not verify twice at the same step"
        );
    }

    /// One step of skew is accepted either side, and two are not.
    #[test]
    fn the_skew_window_is_one_step_wide() {
        let secret = rfc_secret();
        let now = at(1_234_567_890);
        let step = step_at(now);

        for offset in [-1_i64, 0, 1] {
            let code = code_at_step(&secret, step + offset);
            assert!(
                verify(&secret, &code, now, None).is_some(),
                "a code {offset} steps away must verify"
            );
        }
        for offset in [-2_i64, 2] {
            let code = code_at_step(&secret, step + offset);
            assert!(
                verify(&secret, &code, now, None).is_none(),
                "a code {offset} steps away must not verify"
            );
        }
    }

    /// Whitespace is tolerated (users paste "123 456"); everything else is not a code.
    #[test]
    fn malformed_codes_are_refused_without_touching_the_secret() {
        let secret = rfc_secret();
        let now = at(1_234_567_890);
        for junk in ["", "12345", "1234567", "12345a", "abcdef"] {
            assert!(
                verify(&secret, &SecretString::from(junk), now, None).is_none(),
                "{junk:?} must not verify"
            );
        }
        let padded = format!(
            "  {}  ",
            code_at_step(&secret, step_at(now)).expose_secret()
        );
        assert!(
            verify(&secret, &SecretString::from(padded), now, None).is_some(),
            "a code with surrounding whitespace must verify"
        );
    }

    /// `MODULUS` is spelled out as a literal, so nothing but this stops it drifting from
    /// `DIGITS`. A mismatch yields codes of the right length that no authenticator agrees with.
    #[test]
    fn the_modulus_matches_the_digit_count() {
        assert_eq!(
            MODULUS,
            10_u32.pow(u32::try_from(DIGITS).expect("DIGITS fits a u32"))
        );
    }

    #[test]
    fn a_generated_secret_is_the_documented_length_and_encodes_to_base32() {
        let secret = generate_secret();
        assert_eq!(secret.expose_secret().len(), SECRET_LEN);
        let encoded = encode_secret(&secret);
        assert!(
            encoded
                .expose_secret()
                .bytes()
                .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit()),
            "base32 must be the unpadded upper-case alphabet an authenticator accepts"
        );
    }

    /// The provisioning URI states every parameter, and escapes both halves of the label.
    ///
    /// Without the escaping, an issuer or username containing `:` re-splits the
    /// `issuer:account` label and files the entry under the wrong name.
    #[test]
    fn the_provisioning_uri_states_its_parameters_and_escapes_the_label() {
        let uri = provisioning_uri("Tanko Vault", "ast:er", &rfc_secret());
        let uri = uri.expose_secret();
        assert!(
            uri.starts_with("otpauth://totp/Tanko%20Vault:ast%3Aer?"),
            "{uri}"
        );
        assert!(uri.contains("algorithm=SHA1"), "{uri}");
        assert!(uri.contains(&format!("digits={DIGITS}")), "{uri}");
        assert!(uri.contains(&format!("period={STEP_SECONDS}")), "{uri}");
        assert!(uri.contains("issuer=Tanko%20Vault"), "{uri}");
    }
}
