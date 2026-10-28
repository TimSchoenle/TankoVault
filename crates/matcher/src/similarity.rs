//! Pure string and numeric similarity primitives. No domain knowledge lives here.

/// The shortest compact title the edit-distance comparison is consulted for.
///
/// Below this length one character is a large fraction of the title and the ratio stops
/// discriminating: `berserk` and `berserl` differ by one character in seven, which scores 0.857
/// and would attach two unrelated works. Long titles are the opposite case. Two distinct works
/// whose names differ by two characters in twenty are rare, while providers producing exactly
/// that from a typo or a transliteration choice are routine.
const MIN_EDIT_LEN: usize = 12;

/// The longest compact title the edit-distance comparison is computed for.
///
/// The comparison is `O(n·m)` and runs per candidate title inside the ingest transaction. Past
/// this length the trigram similarity has enough text to be a good measure on its own.
const MAX_EDIT_LEN: usize = 160;

/// A pair whose lengths differ by more than this fraction of the longer one cannot reach a
/// useful edit ratio, so the DP is skipped without running it.
const EDIT_LENGTH_SLACK: usize = 6;

/// Returns the normalised edit similarity of two compact keys in `[0,1]`, or `0.0` when the
/// pair is outside the window where the comparison means anything.
///
/// [`MIN_EDIT_LEN`] keeps it from matching short unrelated titles, and [`MAX_EDIT_LEN`] and
/// [`EDIT_LENGTH_SLACK`] keep its quadratic cost off the hot path.
#[must_use]
#[expect(
    clippy::cast_precision_loss,
    reason = "operands are title lengths capped at MAX_EDIT_LEN, far below f32's exact range"
)]
pub(crate) fn edit_ratio(a: &str, b: &str) -> f32 {
    let (la, lb) = (a.chars().count(), b.chars().count());
    let (short, long) = if la <= lb { (la, lb) } else { (lb, la) };
    if short < MIN_EDIT_LEN || long > MAX_EDIT_LEN {
        return 0.0;
    }
    // A length gap alone bounds the ratio at `1 - gap/long`, so a pair that cannot clear the bar
    // is rejected before the DP runs.
    if long - short > long / EDIT_LENGTH_SLACK {
        return 0.0;
    }
    let distance = edit_distance(a, b);
    1.0 - (distance as f32 / long as f32)
}

/// Returns the Levenshtein distance between `a` and `b` over `char`s, two rows at a time.
///
/// Normalized titles keep CJK and Hangul verbatim. A byte-wise distance would count one
/// different ideograph as three edits and score two Japanese titles as far apart as two
/// unrelated ones.
pub(crate) fn edit_distance(a: &str, b: &str) -> usize {
    let b_chars: Vec<char> = b.chars().collect();
    let mut previous: Vec<usize> = (0..=b_chars.len()).collect();
    let mut current = vec![0_usize; b_chars.len() + 1];

    for (i, ca) in a.chars().enumerate() {
        current[0] = i + 1;
        for (j, &cb) in b_chars.iter().enumerate() {
            let substitution = previous[j] + usize::from(ca != cb);
            let deletion = previous[j + 1] + 1;
            let insertion = current[j] + 1;
            current[j + 1] = substitution.min(deletion).min(insertion);
        }
        std::mem::swap(&mut previous, &mut current);
    }
    previous[b_chars.len()]
}

/// Returns whether `query` carries different numbers from any of `titles`.
///
/// `titles` are the titles the query must agree with: the one it was scored against and the
/// candidate's canonical title. Any title the candidate answers to is too wide, since one
/// sequel's alias on a series would then wave every further sequel through.
pub(crate) fn numbers_conflict(query: &str, titles: &[&str]) -> bool {
    let signature = numeric_signature(query);
    titles.iter().any(|t| numeric_signature(t) != signature)
}

/// Roman numerals that read as numbers when they stand alone.
///
/// `i`, `v` and `x` are left out: as tokens they are far more often a pronoun, `versus` and the
/// `×` of `Spy × Family`.
const ROMAN_NUMERALS: &[(&str, u64)] = &[
    ("ii", 2),
    ("iii", 3),
    ("iv", 4),
    ("vi", 6),
    ("vii", 7),
    ("viii", 8),
    ("ix", 9),
];

/// Spelled ordinals that read as numbers.
///
/// `first` is left out: it is part of a title (`First Love`) far more often than it names the
/// original, which rarely carries a number at all.
const ORDINALS: &[(&str, u64)] = &[
    ("second", 2),
    ("third", 3),
    ("fourth", 4),
    ("fifth", 5),
    ("sixth", 6),
    ("seventh", 7),
    ("eighth", 8),
    ("ninth", 9),
    ("tenth", 10),
];

/// Returns the numbers a normalized title carries, in order.
///
/// A digit run continues across a space, as if the title were compact, so `10nen` and `10 nen`
/// agree and only a different number registers: `overlord` against `overlord 2`, volume 3
/// against volume 4. Runs are parsed, so `001` and `1` agree, and saturate, so a 40-digit run
/// cannot panic under the release profile's overflow checks. A standalone roman numeral or
/// spelled ordinal is a number too: `Overlord II` and `Overlord 2` agree, and both conflict
/// with `Overlord`.
pub(crate) fn numeric_signature(normalized: &str) -> Vec<u64> {
    let mut out = Vec::new();
    let mut current: Option<u64> = None;
    for token in normalized.split_whitespace() {
        let word = ROMAN_NUMERALS
            .iter()
            .chain(ORDINALS)
            .find(|(w, _)| *w == token)
            .map(|(_, n)| *n);
        if let Some(value) = word {
            out.extend(current.take());
            out.push(value);
            continue;
        }
        for c in token.chars() {
            if let Some(digit) = c.to_digit(10) {
                let acc = current.unwrap_or(0);
                current = Some(acc.saturating_mul(10).saturating_add(u64::from(digit)));
            } else if let Some(value) = current.take() {
                out.push(value);
            }
        }
    }
    out.extend(current);
    out
}

/// Returns the Jaccard overlap of two case-insensitive name sets in `[0,1]`, or `None` when
/// either side is empty, so a caller can tell missing data from no overlap.
#[must_use]
#[expect(
    clippy::cast_precision_loss,
    reason = "name-set sizes are tag/author counts, orders of magnitude below f32's exact range"
)]
pub(crate) fn name_set_overlap(a: &[String], b: &[String]) -> Option<f32> {
    if a.is_empty() || b.is_empty() {
        return None;
    }
    let sa: std::collections::BTreeSet<String> = a.iter().map(|s| s.to_lowercase()).collect();
    let sb: std::collections::BTreeSet<String> = b.iter().map(|s| s.to_lowercase()).collect();
    // Both slices are non-empty, so the union has a member and the division cannot be `0 / 0`.
    let union = sa.union(&sb).count();
    Some(sa.intersection(&sb).count() as f32 / union as f32)
}

/// Returns whether `a` and `b` share at least one name, ignoring ASCII case only.
#[must_use]
pub(crate) fn shares_a_name(a: &[String], b: &[String]) -> bool {
    a.iter()
        .any(|x| b.iter().any(|y| x.eq_ignore_ascii_case(y)))
}

/// Returns the Jaccard overlap of the two titles' word sets, in `[0,1]`.
///
/// Word order and repeats do not count, so "life starting in another world" and "another world
/// starting life" score the same. Two titles with no words score 0.
///
/// ```
/// use tankovault_matcher::token_set_ratio;
///
/// assert_eq!(token_set_ratio("solo leveling", "leveling solo"), 1.0);
/// assert_eq!(token_set_ratio("solo leveling", "solo leveling ragnarok"), 2.0 / 3.0);
/// assert_eq!(token_set_ratio("", " "), 0.0);
/// ```
#[must_use]
#[expect(
    clippy::cast_precision_loss,
    reason = "word-set sizes are title token counts, orders of magnitude below f32's exact range"
)]
pub fn token_set_ratio(a: &str, b: &str) -> f32 {
    // Two whitespace-only titles give two empty sets. Their `0 / 0` would be a `NaN` that
    // passes through every later term in `score` and compares false against both thresholds.
    let sa: std::collections::BTreeSet<&str> = a.split_whitespace().collect();
    let sb: std::collections::BTreeSet<&str> = b.split_whitespace().collect();
    let union = sa.union(&sb).count();
    if union == 0 {
        return 0.0;
    }
    sa.intersection(&sb).count() as f32 / union as f32
}

/// Returns whether every word of `needle` also appears in `haystack`.
///
/// `needle` needs at least two words, so one shared word is not containment.
pub(crate) fn is_token_subset(needle: &str, haystack: &str) -> bool {
    let sn: std::collections::BTreeSet<&str> = needle.split_whitespace().collect();
    if sn.len() < 2 {
        return false;
    }
    let sh: std::collections::BTreeSet<&str> = haystack.split_whitespace().collect();
    sn.is_subset(&sh)
}

/// Returns whether every word of `needle` appears in `haystack` and `haystack` has words `needle`
/// lacks.
pub(crate) fn is_strict_token_subset(needle: &str, haystack: &str) -> bool {
    let sn: std::collections::BTreeSet<&str> = needle.split_whitespace().collect();
    let sh: std::collections::BTreeSet<&str> = haystack.split_whitespace().collect();
    !sn.is_empty() && sn.len() < sh.len() && sn.is_subset(&sh)
}
