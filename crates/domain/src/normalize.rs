//! Title normalization — the shared key used for canonical-series matching and the
//! `normalized_title` column. Pure and deterministic so the DB, matcher, and adapters
//! all derive the same key.
//!
//! Steps: lowercase, fold diacritics and typographic variants, **elide** apostrophes, drop
//! remaining punctuation, remove common noise words, and collapse whitespace.
//!
//! # Why apostrophes are elided rather than separated
//!
//! Every other punctuation mark is a word boundary — `Re:Zero` really is two words. An
//! apostrophe is not: it sits *inside* a word, and the same work is listed by one provider as
//! `Sorry but I’m not Yuri` and by another as `Sorry But Im Not Yuri`. Treating it as a
//! separator produced `sorry but i m not yuri` against `sorry but im not yuri` — two keys with
//! a token-set overlap of 4/7, scoring 0.80 and landing in the operator-review band instead of
//! attaching. That single rule accounted for the largest class of duplicates in a 26k-series
//! catalogue: `witch s tears` / `witchs tears`, `king s journey` / `kings journey`, and so on
//! for every possessive and contraction a provider spells with a straight quote, a curly quote,
//! or nothing at all.
//!
//! The same reasoning covers combining marks: `İ` lowercases to `i` + U+0307, and separating on
//! the combining character split `İstanbul` into `i stanbul`. Marks are elided so an NFD-encoded
//! title folds onto its NFC twin instead of shattering.

/// Noise tokens that carry no discriminating signal across provider titles.
const NOISE_WORDS: &[&str] = &[
    "manga",
    "mangas",
    "manhwa",
    "manhwas",
    "manhua",
    "manhuas",
    "webtoon",
    "webtoons",
    "webcomic",
    "webcomics",
    "comic",
    "comics",
    "official",
    "raw",
    "raws",
    "scan",
    "scans",
    "scanlation",
];

/// Produce the normalized matching key for a title.
///
/// The result is lowercase ASCII-ish, punctuation-free, noise-word-free, and
/// single-spaced. Empty input yields an empty string.
///
/// This key decides whether two providers' listings are the same work, so the examples below
/// are the contract rather than illustration: every one of them is a pair a real catalogue
/// produces, and each would be two separate series if the rule it shows were dropped.
///
/// ```
/// use tankovault_domain::normalize_title;
///
/// // Case, punctuation and repeated spacing are noise.
/// assert_eq!(normalize_title("Solo Leveling"), "solo leveling");
/// assert_eq!(normalize_title("Re:Zero  -  Starting Life"), "re zero starting life");
///
/// // Apostrophes are elided, not separated — in every spelling a provider uses. This is the
/// // rule that makes a possessive or a contraction match the same title typed without one.
/// assert_eq!(normalize_title("Sorry but I’m not Yuri"), "sorry but im not yuri");
/// assert_eq!(normalize_title("Sorry But Im Not Yuri"),  "sorry but im not yuri");
/// assert_eq!(normalize_title("The Witch's Tears"), normalize_title("The Witchs Tears"));
///
/// // Latin diacritics fold, so an accented release matches its ASCII listing — whether the
/// // accent is a precomposed character or a base letter plus a combining mark.
/// assert_eq!(normalize_title("Ōoku"), "ooku");
/// assert_eq!(normalize_title("Be\u{0301}rserk"), "berserk");
///
/// // Full-width forms are the same letters typed on a Japanese keyboard.
/// assert_eq!(normalize_title("ＳＰＹ×ＦＡＭＩＬＹ"), "spy family");
///
/// // An ampersand is a word, and providers disagree about which spelling to use.
/// assert_eq!(normalize_title("Ao & Haru"), "ao and haru");
///
/// // Medium words are noise too: providers append them inconsistently, and a work does not
/// // stop being the same work because one site calls it a manhwa.
/// assert_eq!(normalize_title("Solo Leveling Manhwa"), "solo leveling");
/// assert_eq!(normalize_title("Berserk (Official Scan)"), "berserk");
///
/// // …but a title made *entirely* of noise words keeps them, because the alternative is an
/// // empty key, and every empty key collides with every other one.
/// assert_eq!(normalize_title("Manga"), "manga");
///
/// // Only genuinely empty input yields an empty key.
/// assert_eq!(normalize_title("   "), "");
/// ```
#[must_use]
pub fn normalize_title(title: &str) -> String {
    // One pass: lowercase (which can expand, e.g. `İ` → `i` + U+0307), then fold each
    // resulting character into the output. `fold_into` is what decides between "drop this",
    // "this is a word boundary" and "this expands to several ASCII letters", so the old
    // three-pass lowercase → fold → strip pipeline collapses into this loop.
    let mut folded = String::with_capacity(title.len());
    for c in title.chars().flat_map(char::to_lowercase) {
        fold_into(widen(c), &mut folded);
    }

    let mut tokens: Vec<&str> = folded
        .split_whitespace()
        .filter(|tok| !NOISE_WORDS.contains(tok))
        .collect();

    // If filtering removed everything (e.g. title was literally "Manga"), fall back to
    // the un-filtered tokens so we never produce an empty key from a non-empty title.
    if tokens.is_empty() {
        tokens = folded.split_whitespace().collect();
    }

    tokens.join(" ")
}

/// The whitespace-insensitive form of an already-[`normalize_title`]d key.
///
/// # Why this exists
///
/// Providers scrape titles out of HTML, and a missing space between two inline elements is the
/// single most common way one listing differs from another for the *same* work: `Spy X Family`
/// against `Spyxfamily`, `Wants to Be Free` against `Wantsto Be Free`, `Hana Kimi` against
/// `Hanakimi`. Trigram similarity scores those pairs between 0.37 and 0.58 and a token-set ratio
/// scores most of them 0, so they were invisible to the matcher — 59 such pairs sat in a 26k
/// catalogue without even reaching the review queue.
///
/// Comparing the compact keys makes the whole class exact. It is deliberately derived from the
/// *normalized* key rather than the raw title, so it inherits the case-folding, diacritic
/// folding and noise-word removal above and adds exactly one rule of its own.
///
/// ```
/// use tankovault_domain::{compact_key, normalize_title};
///
/// assert_eq!(
///     compact_key(&normalize_title("Spy X Family")),
///     compact_key(&normalize_title("Spyxfamily")),
/// );
/// assert_eq!(compact_key("hana kimi"), "hanakimi");
/// assert_eq!(compact_key(""), "");
/// ```
#[must_use]
pub fn compact_key(normalized: &str) -> String {
    normalized.chars().filter(|c| !c.is_whitespace()).collect()
}

/// Map a full-width or ideographic character onto its ASCII equivalent, leaving everything else
/// alone.
///
/// Full-width forms (`Ｓ`, `０`, `＆`) are the same characters typed on a CJK input method and
/// appear throughout Japanese and Korean provider listings. Folding them here — *before*
/// [`fold_into`] dispatches — means the apostrophe and ampersand rules below apply to `＇` and
/// `＆` without a second set of arms for them.
fn widen(c: char) -> char {
    match c {
        // U+FF01..=U+FF5E are ASCII U+0021..=U+007E shifted by 0xFEE0.
        '\u{FF01}'..='\u{FF5E}' => char::from_u32(c as u32 - 0xFEE0).unwrap_or(c),
        // Ideographic space.
        '\u{3000}' => ' ',
        other => other,
    }
}

/// Fold one lowercased character into the key being built.
///
/// Three outcomes, and which one a character gets is the whole of the normalization policy:
/// elided (contributes nothing and does **not** break the word), expanded to one or more ASCII
/// letters, or treated as a word boundary.
#[expect(
    clippy::match_same_arms,
    reason = "the two elided arms have the same *body* and different reasons — apostrophes sit \
              inside a word, combining marks belong to the letter before them — and each \
              carries the comment explaining which duplicate class it exists to prevent. \
              Merging them into one arm would leave one comment describing two rules."
)]
fn fold_into(c: char, out: &mut String) {
    match c {
        // --- elided: these sit *inside* a word ------------------------------------------
        //
        // Apostrophes in every spelling a provider emits — straight, curly, modifier-letter,
        // backtick, acute accent used as one, prime. See the module docs: separating on these
        // is what split `i’m` into `i m` and cost the largest duplicate class in the catalogue.
        '\'' | '\u{2018}' | '\u{2019}' | '\u{02BC}' | '\u{02B9}' | '`' | '\u{00B4}'
        | '\u{2032}' => {}
        // Combining marks. Dropping them folds an NFD-encoded title onto its NFC twin;
        // separating on them split `İstanbul` (which lowercases to `i` + U+0307) into two words.
        '\u{0300}'..='\u{036F}'
        | '\u{1AB0}'..='\u{1AFF}'
        | '\u{20D0}'..='\u{20F0}'
        | '\u{FE20}'..='\u{FE2F}' => {}

        // --- expanded: one character that is really several letters ---------------------
        //
        // Surrounded by spaces because an ampersand *is* a word: providers write "Ao & Haru"
        // and "Ao and Haru" for the same series, and `x&y` is two words either way.
        '&' => out.push_str(" and "),
        'æ' => out.push_str("ae"),
        'œ' => out.push_str("oe"),
        'ĳ' => out.push_str("ij"),
        // `ß` is `ss`, not `s`: the German spelling reform swaps the two, so folding to a
        // single `s` would leave `Straße` and `Strasse` on different keys.
        'ß' => out.push_str("ss"),
        'þ' => out.push_str("th"),
        // Latin ligatures a PDF-sourced or OCR'd listing carries through.
        '\u{FB00}' => out.push_str("ff"),
        '\u{FB01}' => out.push_str("fi"),
        '\u{FB02}' => out.push_str("fl"),
        '\u{FB03}' => out.push_str("ffi"),
        '\u{FB04}' => out.push_str("ffl"),

        // --- folded: an accented letter is its base letter -------------------------------
        'à' | 'á' | 'â' | 'ã' | 'ä' | 'å' | 'ā' | 'ă' | 'ą' | 'ǎ' | 'ȧ' => out.push('a'),
        'ç' | 'ć' | 'ĉ' | 'ċ' | 'č' => out.push('c'),
        'ď' | 'đ' | 'ð' => out.push('d'),
        'è' | 'é' | 'ê' | 'ë' | 'ē' | 'ĕ' | 'ė' | 'ę' | 'ě' => out.push('e'),
        'ĝ' | 'ğ' | 'ġ' | 'ģ' => out.push('g'),
        'ĥ' | 'ħ' => out.push('h'),
        'ì' | 'í' | 'î' | 'ï' | 'ĩ' | 'ī' | 'ĭ' | 'į' | 'ı' => out.push('i'),
        'ĵ' => out.push('j'),
        'ķ' => out.push('k'),
        'ĺ' | 'ļ' | 'ľ' | 'ŀ' | 'ł' => out.push('l'),
        'ñ' | 'ń' | 'ņ' | 'ň' | 'ŋ' => out.push('n'),
        'ò' | 'ó' | 'ô' | 'õ' | 'ö' | 'ø' | 'ō' | 'ŏ' | 'ő' => out.push('o'),
        'ŕ' | 'ŗ' | 'ř' => out.push('r'),
        'ś' | 'ŝ' | 'ş' | 'š' => out.push('s'),
        'ţ' | 'ť' | 'ŧ' => out.push('t'),
        'ù' | 'ú' | 'û' | 'ü' | 'ũ' | 'ū' | 'ŭ' | 'ů' | 'ű' | 'ų' => out.push('u'),
        'ŵ' => out.push('w'),
        'ý' | 'ÿ' | 'ŷ' => out.push('y'),
        'ź' | 'ż' | 'ž' => out.push('z'),

        // --- everything else -------------------------------------------------------------
        //
        // Alphanumeric survives verbatim, which is what keeps CJK and Hangul titles intact;
        // any other character is a word boundary.
        other if other.is_alphanumeric() => out.push(other),
        _ => out.push(' '),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lowercases_and_collapses_whitespace() {
        assert_eq!(normalize_title("  Solo   Leveling  "), "solo leveling");
    }

    #[test]
    fn strips_punctuation() {
        assert_eq!(
            normalize_title("Re:Zero - Starting Life!"),
            "re zero starting life"
        );
    }

    #[test]
    fn drops_noise_words() {
        assert_eq!(normalize_title("Tower of God (Webtoon)"), "tower of god");
    }

    #[test]
    fn folds_diacritics() {
        assert_eq!(normalize_title("Bërserk"), "berserk");
    }

    #[test]
    fn noise_only_title_is_not_emptied() {
        assert_eq!(normalize_title("Manga"), "manga");
    }

    #[test]
    fn empty_stays_empty() {
        assert_eq!(normalize_title("   "), "");
    }

    /// An apostrophe joins a word; it does not split one.
    ///
    /// This is the rule the whole merge queue turned on. `Sorry but I’m not Yuri` and `Sorry But
    /// Im Not Yuri` are the same series on two providers, and while the apostrophe was a
    /// separator they normalized to `sorry but i m not yuri` and `sorry but im not yuri` — a
    /// token-set overlap of 4/7 and a trigram score of 0.80, which is the review band, not the
    /// attach band. Every possessive in the catalogue had the same problem.
    #[test]
    fn apostrophes_join_a_word_rather_than_splitting_it() {
        let expected = "sorry but im not yuri";
        for spelling in [
            "Sorry but I'm not Yuri",
            "Sorry but I\u{2019}m not Yuri",
            "Sorry but I\u{02BC}m not Yuri",
            "Sorry but I`m not Yuri",
            "Sorry But Im Not Yuri",
            // Full-width apostrophe, as a Japanese input method emits it.
            "Sorry but I\u{FF07}m not Yuri",
        ] {
            assert_eq!(normalize_title(spelling), expected, "from {spelling:?}");
        }
        assert_eq!(
            normalize_title("The Witch's Tears Become Poison"),
            normalize_title("The Witchs Tears Become Poison")
        );
    }

    /// Combining marks fold into the letter they sit on instead of breaking the word.
    ///
    /// `İ` (U+0130) lowercases to `i` + U+0307, and while combining marks were separators that
    /// turned `İstanbul` into two tokens. The same rule makes an NFD-encoded title match its
    /// precomposed twin, which providers mix freely.
    #[test]
    fn combining_marks_are_elided_not_separated() {
        assert_eq!(normalize_title("\u{0130}stanbul"), "istanbul");
        assert_eq!(normalize_title("Be\u{0301}rserk"), "berserk");
        assert_eq!(
            normalize_title("Bérserk"),
            normalize_title("Be\u{0301}rserk")
        );
    }

    /// Full-width forms are the same characters as their ASCII twins.
    ///
    /// Japanese and Korean listings use them freely, and every one of them was previously a
    /// non-ASCII alphanumeric that survived normalization verbatim — so `ＳＰＹ` and `SPY`
    /// were different words with a trigram similarity of zero.
    #[test]
    fn full_width_forms_fold_to_ascii() {
        assert_eq!(normalize_title("ＳＰＹ×ＦＡＭＩＬＹ"), "spy family");
        assert_eq!(normalize_title("ＶＯＬ．１２"), "vol 12");
        assert_eq!(normalize_title("Ａ＆Ｂ"), "a and b");
    }

    /// An ampersand is a word, and it is the word "and".
    #[test]
    fn an_ampersand_is_the_word_and() {
        assert_eq!(normalize_title("Ao & Haru"), "ao and haru");
        assert_eq!(normalize_title("Ao and Haru"), normalize_title("Ao & Haru"));
        // No space around it in the source is still two words plus a conjunction.
        assert_eq!(normalize_title("Tom&Jerry"), "tom and jerry");
    }

    /// Multi-letter folds keep both letters.
    ///
    /// `ß` → `s` would have put `Straße` and `Strasse` on different keys, which is the exact
    /// case the fold exists to collapse.
    #[test]
    fn multi_letter_folds_expand_rather_than_truncate() {
        assert_eq!(normalize_title("Straße"), "strasse");
        assert_eq!(normalize_title("Strasse"), normalize_title("Straße"));
        assert_eq!(normalize_title("Æon"), "aeon");
        assert_eq!(normalize_title("\u{FB01}nal"), "final");
    }

    /// CJK and Hangul survive normalization, because the catch-all arm keeps every
    /// alphanumeric character rather than only ASCII ones.
    #[test]
    fn cjk_titles_survive() {
        assert_eq!(normalize_title("ワンピース"), "ワンピース");
        assert_eq!(normalize_title("나 혼자만 레벨업"), "나 혼자만 레벨업");
    }

    /// The compact key differs from the normalized key by exactly one rule.
    #[test]
    fn the_compact_key_is_the_normalized_key_without_spaces() {
        assert_eq!(
            compact_key(&normalize_title("Spy X Family")),
            compact_key(&normalize_title("Spyxfamily"))
        );
        assert_eq!(
            compact_key(&normalize_title("Wants to Be Free")),
            compact_key(&normalize_title("Wantsto Be Free"))
        );
        // It inherits every other rule rather than restating any of them.
        assert_eq!(
            compact_key(&normalize_title("It's Love")),
            compact_key(&normalize_title("Its Love"))
        );
    }
}
