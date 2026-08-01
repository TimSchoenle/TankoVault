//! Property tests for [`normalize_title`]: idempotence, totality, and whitespace/token
//! canonicity that stored keys, trigram indexing, and re-crawled matching all depend on.

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

    /// Totality: the adapter's `parse_chapter_number` panicked on `U+0130` from the same
    /// to-lowercase-then-walk pattern; this is the standing guard that this function doesn't.
    #[test]
    fn normalize_title_never_panics(title in ".*") {
        let _ = normalize_title(&title);
    }

    /// Whitespace canonicity: single-space-separated, no leading/trailing space — trigram
    /// similarity runs over this string, so a stray space is a silent scoring difference.
    #[test]
    fn normalize_title_is_whitespace_canonical(title in ".*") {
        let key = normalize_title(&title);
        prop_assert!(!key.starts_with(' '), "leading space in {key:?}");
        prop_assert!(!key.ends_with(' '), "trailing space in {key:?}");
        prop_assert!(!key.contains("  "), "double space in {key:?}");
        prop_assert!(!key.contains('\t') && !key.contains('\n'), "raw whitespace in {key:?}");
    }

    /// A title with at least one alphanumeric character always yields a non-empty key.
    ///
    /// `normalize.rs` claims "never empty from non-empty input", but that's only true after
    /// the noise-word fallback — `"!!!"` strips to `""` before the fallback runs. This weaker
    /// claim is the one matching actually depends on.
    #[test]
    fn a_title_with_any_alphanumeric_character_yields_a_non_empty_key(title in ".*") {
        prop_assume!(title.chars().any(char::is_alphanumeric));
        prop_assert!(
            !normalize_title(&title).is_empty(),
            "{:?} has an alphanumeric character but normalized to an empty key", title
        );
    }

    /// Case insensitivity over ASCII: two providers' casings of the same title must land on
    /// one key. Restricted to ASCII because `to_uppercase` is not an involution over full
    /// Unicode (`ß` → `SS`), so a Unicode version would test `str::to_uppercase`, not this.
    #[test]
    fn normalize_title_is_case_insensitive_over_ascii(title in "[ -~]{0,64}") {
        prop_assert_eq!(
            normalize_title(&title),
            normalize_title(&title.to_uppercase())
        );
    }

    /// Every token in the key must be alphanumeric — anything else means a separator survived
    /// stripping and would be indexed as part of a word.
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
