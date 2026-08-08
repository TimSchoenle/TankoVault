//! Property tests for [`normalize_title`]: idempotence, totality, and whitespace/token
//! canonicity that stored keys, trigram indexing, and re-crawled matching all depend on.

use proptest::prelude::*;
use tankovault_domain::normalize_title;

/// The characters `char::is_alphanumeric` accepts but `normalize_title` deliberately elides, so
/// a title made only of them normalizes to an empty key.
///
/// Two classes, both load-bearing: combining marks that carry `Other_Alphabetic` (eliding them
/// is what folds an NFD-encoded title onto its NFC twin), and the two modifier letters providers
/// emit as apostrophes (eliding them is what matches a possessive against the same title typed
/// without one). Neither class can be the whole of a title a provider lists, so an empty key is
/// the right answer for one that is nothing else.
///
/// This list is pinned against the folder by
/// `an_alphanumeric_character_is_elided_only_if_it_is_listed_above`, so a new elision fails
/// deterministically instead of waiting for a proptest seed to find it.
fn is_elided_though_alphanumeric(c: char) -> bool {
    matches!(c,
        '\u{02B9}' | '\u{02BC}'
        | '\u{0345}'
        | '\u{0363}'..='\u{036F}'
        | '\u{1ABF}'..='\u{1AC0}'
        | '\u{1ACC}'..='\u{1ACE}'
    )
}

/// `is_elided_though_alphanumeric` is the folder's actual behaviour, not an assumption about it.
///
/// `"\u{363}"` reached `a_title_with_a_surviving_alphanumeric_character_yields_a_non_empty_key`
/// as a fresh seed and failed it: the property assumed `char::is_alphanumeric` was the class of
/// characters that survives normalization, and it is not. Sweeping every scalar value turns the
/// next divergence into a deterministic failure naming the character, rather than a red CI run
/// on whichever branch happens to draw the seed.
#[test]
fn an_alphanumeric_character_is_elided_only_if_it_is_listed_above() {
    for codepoint in 0..=0x0010_FFFF_u32 {
        let Some(c) = char::from_u32(codepoint) else {
            continue;
        };
        if !c.is_alphanumeric() {
            continue;
        }
        assert_eq!(
            normalize_title(&c.to_string()).is_empty(),
            is_elided_though_alphanumeric(c),
            "U+{codepoint:04X} ({c:?}) is alphanumeric and its elision disagrees with the list"
        );
    }
}

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

    /// A title with at least one alphanumeric character the folder *keeps* always yields a
    /// non-empty key.
    ///
    /// `normalize.rs` claims "never empty from non-empty input", but that's only true after
    /// the noise-word fallback — `"!!!"` strips to `""` before the fallback runs. This weaker
    /// claim is the one matching actually depends on.
    ///
    /// The exclusion is not the property being weakened to bury a failure: `is_alphanumeric` is
    /// true for combining marks, which normalization elides on purpose, so "alphanumeric" was
    /// never the class of characters that survives. What is left is the claim with teeth — a
    /// surviving character is never erased by its *context*, which is what the noise-word
    /// filter would otherwise do to a title of nothing but noise words.
    #[test]
    fn a_title_with_a_surviving_alphanumeric_character_yields_a_non_empty_key(title in ".*") {
        prop_assume!(title
            .chars()
            .any(|c| c.is_alphanumeric() && !is_elided_though_alphanumeric(c)));
        prop_assert!(
            !normalize_title(&title).is_empty(),
            "{:?} has a surviving alphanumeric character but normalized to an empty key", title
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
