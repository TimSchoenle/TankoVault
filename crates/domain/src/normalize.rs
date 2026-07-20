//! Title normalization — the shared key used for canonical-series matching and the
//! `normalized_title` column. Pure and deterministic so the DB, matcher, and adapters
//! all derive the same key.
//!
//! Steps: lowercase, strip diacritics, drop punctuation, remove common noise words,
//! and collapse whitespace.

/// Noise tokens that carry no discriminating signal across provider titles.
const NOISE_WORDS: &[&str] = &[
    "manga", "manhwa", "manhua", "webtoon", "comic", "official", "raw", "scan", "scans",
];

/// Produce the normalized matching key for a title.
///
/// The result is lowercase ASCII-ish, punctuation-free, noise-word-free, and
/// single-spaced. Empty input yields an empty string.
#[must_use]
pub fn normalize_title(title: &str) -> String {
    let lowered = title.to_lowercase();

    // Strip diacritics by decomposing and dropping combining marks. We do a light
    // ASCII-folding for the most common Latin accents without pulling in unicode crates.
    let folded: String = lowered.chars().map(fold_char).collect();

    // Keep alphanumerics and spaces; every other char becomes a separator.
    let cleaned: String = folded
        .chars()
        .map(|c| if c.is_alphanumeric() { c } else { ' ' })
        .collect();

    let mut tokens: Vec<&str> = cleaned
        .split_whitespace()
        .filter(|tok| !NOISE_WORDS.contains(tok))
        .collect();

    // If filtering removed everything (e.g. title was literally "Manga"), fall back to
    // the un-filtered tokens so we never produce an empty key from a non-empty title.
    if tokens.is_empty() {
        tokens = cleaned.split_whitespace().collect();
    }

    tokens.join(" ")
}

/// Fold the most common accented Latin characters to their base ASCII letter.
fn fold_char(c: char) -> char {
    match c {
        'à' | 'á' | 'â' | 'ã' | 'ä' | 'å' | 'ā' | 'ă' | 'ą' => 'a',
        'ç' | 'ć' | 'č' => 'c',
        'è' | 'é' | 'ê' | 'ë' | 'ē' | 'ĕ' | 'ę' | 'ě' => 'e',
        'ì' | 'í' | 'î' | 'ï' | 'ī' | 'ĭ' | 'į' => 'i',
        'ñ' | 'ń' | 'ň' => 'n',
        'ò' | 'ó' | 'ô' | 'õ' | 'ö' | 'ø' | 'ō' | 'ŏ' => 'o',
        'ù' | 'ú' | 'û' | 'ü' | 'ū' | 'ŭ' | 'ů' => 'u',
        'ý' | 'ÿ' => 'y',
        'ß' => 's',
        other => other,
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
}
