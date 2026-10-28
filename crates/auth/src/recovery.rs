//! Owns recovery codes, the escape hatch that makes enrolling a second factor safe.
//!
//! Without them, "add 2FA" means "risk losing your account to a lost phone", and the rational
//! response is not to enrol. A set is issued when the first factor is confirmed, displayed
//! exactly once, and consumed one code at a time.

use rand::Rng as _;
use secrecy::{ExposeSecret as _, SecretString};

/// The codes issued per set.
///
/// Ten is enough to print, keep in a wallet and lose a few of, and few enough that the account
/// page's "3 of 10 remaining" prompts a regeneration.
pub const CODE_COUNT: usize = 10;

/// The random bytes behind one code: 80 bits, rendered as sixteen base32 characters.
///
/// Codes are checked by an online endpoint, so the only attack is the request budget, and 80
/// bits is far beyond it. The code is also short enough to read aloud to a support desk.
const CODE_BYTES: usize = 10;

/// The characters per group in the displayed form.
const GROUP_LEN: usize = 4;

/// Generates a fresh set of codes in the displayed form, `xxxx-xxxx-xxxx-xxxx`.
///
/// The plaintext exists only in this return value: the caller shows it once and stores
/// [`hash_code`] of each.
#[must_use]
pub fn generate_codes() -> Vec<SecretString> {
    (0..CODE_COUNT).map(|_| generate_code()).collect()
}

/// Generates one code, grouped for legibility.
fn generate_code() -> SecretString {
    let mut bytes = [0u8; CODE_BYTES];
    rand::rng().fill_bytes(&mut bytes);
    let raw = data_encoding::BASE32_NOPAD.encode(&bytes);

    let mut grouped = String::with_capacity(raw.len() + raw.len() / GROUP_LEN);
    for (i, c) in raw.chars().enumerate() {
        if i > 0 && i % GROUP_LEN == 0 {
            grouped.push('-');
        }
        grouped.push(c);
    }
    SecretString::from(grouped)
}

/// Returns the stored representation of a code.
///
/// **A fast hash on purpose.** These are server-generated 80-bit tokens, so there is no
/// dictionary for argon2 to slow down. Argon2 here would cost one deliberately expensive hash
/// per candidate row, ten per verification attempt: a denial of service an unauthenticated
/// caller can trigger. Do not switch to argon2 without changing the lookup to target one row.
///
/// The code is normalised first (case-folded, separators and whitespace removed), so `abcd
/// efgh` matches `ABCD-EFGH`. Normalisation is part of the stored form: changing it invalidates
/// every issued code.
#[must_use]
pub fn hash_code(code: &SecretString) -> String {
    let normalised: String = code
        .expose_secret()
        .chars()
        .filter(char::is_ascii_alphanumeric)
        .map(|c| c.to_ascii_uppercase())
        .collect();
    crate::opaque::sha256_hex(normalised.as_bytes())
}

#[cfg(test)]
mod tests {
    use super::{CODE_COUNT, generate_codes, hash_code};
    use secrecy::SecretString;
    use std::collections::BTreeSet;

    #[test]
    fn a_set_is_the_documented_size_and_has_no_repeats() {
        let codes = generate_codes();
        assert_eq!(codes.len(), CODE_COUNT);
        let distinct: BTreeSet<&str> = codes
            .iter()
            .map(secrecy::ExposeSecret::expose_secret)
            .collect();
        assert_eq!(
            distinct.len(),
            CODE_COUNT,
            "codes within a set must be distinct"
        );
    }

    /// The forms a user might type all hash to one value.
    ///
    /// The bug: hashing the displayed string verbatim rejects a code typed without the dashes,
    /// as it is when read off paper. The user then burns the rest of the set trying.
    #[test]
    fn the_typed_forms_of_one_code_all_match() {
        let canonical = SecretString::from("ABCD-EFGH-IJKL-MNOP");
        let expected = hash_code(&canonical);
        for variant in [
            "abcd-efgh-ijkl-mnop",
            "ABCDEFGHIJKLMNOP",
            "abcd efgh ijkl mnop",
            "  ABCD-EFGH-IJKL-MNOP  ",
        ] {
            assert_eq!(
                hash_code(&SecretString::from(variant)),
                expected,
                "{variant:?} must hash to the canonical form"
            );
        }
    }

    #[test]
    fn different_codes_hash_differently() {
        let a = hash_code(&SecretString::from("ABCD-EFGH-IJKL-MNOP"));
        let b = hash_code(&SecretString::from("ABCD-EFGH-IJKL-MNOQ"));
        assert_ne!(a, b);
    }
}
