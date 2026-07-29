//! Properties of [`normalize_title`], the shared canonicalisation key.
//!
//! `normalize_title` is not an ordinary string helper. Its output is **persisted** in the
//! `series.normalized_title` column, indexed with a trigram index, *and* recomputed at match
//! time from freshly-crawled titles. Every one of those three uses assumes the same value
//! comes out for the same input, and that re-normalising a stored key is a no-op. Those
//! assumptions have never been checked — this file checks them.
//!
//! Written as properties rather than examples because the failure mode is a *character class*
//! nobody thought of. The example tests in `normalize.rs` are all Latin-script; the panic that
//! this audit found in the adapter parsers (`parse_chapter_number` on `U+0130`) came from
//! exactly the blind spot a `".*"` strategy closes in one line.

use proptest::prelude::*;
use tankovault_domain::normalize_title;

proptest! {
    /// Idempotence. The normalized key is stored in a column *and* recomputed from a fresh
    /// crawl of the same title; if a second pass could change it, a re-normalisation migration
    /// would silently orphan every affected row from its own matching key.
    #[test]
    fn normalize_title_is_idempotent(title in ".*") {
        let once = normalize_title(&title);
        let twice = normalize_title(&once);
        prop_assert_eq!(&once, &twice, "normalizing a normalized key changed it: {:?}", title);
    }

    /// Totality. The function runs `to_lowercase` and then walks the result; the adapter-side
    /// version of that pattern panicked on `U+0130`. This is the standing guard that this one
    /// does not, over arbitrary Unicode including combining marks and dotted capital I.
    #[test]
    fn normalize_title_never_panics(title in ".*") {
        let _ = normalize_title(&title);
    }

    /// Whitespace canonicity: the output is exactly single-space-separated, with no leading or
    /// trailing space. Trigram similarity is computed over this string, so a stray space is a
    /// silent scoring difference between two titles that should be identical.
    #[test]
    fn normalize_title_is_whitespace_canonical(title in ".*") {
        let key = normalize_title(&title);
        prop_assert!(!key.starts_with(' '), "leading space in {key:?}");
        prop_assert!(!key.ends_with(' '), "trailing space in {key:?}");
        prop_assert!(!key.contains("  "), "double space in {key:?}");
        prop_assert!(!key.contains('\t') && !key.contains('\n'), "raw whitespace in {key:?}");
    }

    /// A title carrying at least one alphanumeric character always yields a non-empty key.
    ///
    /// Note the precondition. `normalize.rs` claims "we never produce an empty key from a
    /// non-empty title", but that claim is only true of the *noise-word* fallback: a title of
    /// `"!!!"` normalizes to `""` because punctuation is stripped before the fallback is
    /// consulted. The honest invariant — and the one matching actually depends on — is this
    /// one, so it is what is asserted rather than the stronger claim in the comment.
    #[test]
    fn a_title_with_any_alphanumeric_character_yields_a_non_empty_key(title in ".*") {
        prop_assume!(title.chars().any(char::is_alphanumeric));
        prop_assert!(
            !normalize_title(&title).is_empty(),
            "{:?} has an alphanumeric character but normalized to an empty key", title
        );
    }

    /// Case insensitivity over ASCII. Provider titles arrive in whatever case the site uses,
    /// and two sources for the same work must land on one key or the matcher never links them.
    /// Restricted to ASCII deliberately: `to_uppercase` is not an involution over full Unicode
    /// (`ß` uppercases to `SS`), so a Unicode version of this property would be asserting
    /// something about `str::to_uppercase`, not about normalisation.
    #[test]
    fn normalize_title_is_case_insensitive_over_ascii(title in "[ -~]{0,64}") {
        prop_assert_eq!(
            normalize_title(&title),
            normalize_title(&title.to_uppercase())
        );
    }

    /// The key is built from the folded, punctuation-stripped token stream, so every token in
    /// the output must be alphanumeric. Anything else means a separator survived stripping and
    /// would be indexed as part of a word.
    #[test]
    fn every_token_of_the_key_is_alphanumeric(title in ".*") {
        for token in normalize_title(&title).split(' ').filter(|t| !t.is_empty()) {
            prop_assert!(
                token.chars().all(char::is_alphanumeric),
                "token {:?} is not alphanumeric (from {:?})", token, title
            );
        }
    }
}
