//! Pure string and numeric similarity primitives. No domain knowledge lives here.

use tankovault_domain::compact_key;
use tankovault_domain::matching::Candidate;

/// Shortest compact title for which the edit-distance comparison is consulted at all.
///
/// Below this length a one-character difference is a large fraction of the title, and the ratio
/// stops discriminating: `berserk` and `berserl` differ by one character out of seven, which
/// would score 0.857 and attach two unrelated works. Long titles are the opposite case — two
/// distinct works whose names differ by two characters over twenty essentially do not occur,
/// while providers producing exactly that from a typo or a transliteration choice are routine.
const MIN_EDIT_LEN: usize = 12;

/// Longest compact title the edit-distance comparison is computed for.
///
/// The comparison is `O(n·m)` and runs per candidate title inside the ingest transaction. Past
/// this length the trigram similarity is already a good measure (there is plenty of text for it
/// to work with) and the quadratic cost is not worth paying on a hot path.
const MAX_EDIT_LEN: usize = 160;

/// A pair whose lengths differ by more than this fraction of the longer one cannot reach a
/// useful edit ratio, so the DP is skipped without running it.
const EDIT_LENGTH_SLACK: usize = 6;

/// Normalised edit similarity of two compact keys, in `[0,1]`, or `0.0` when the pair is
/// outside the window this comparison is meaningful in.
///
/// The guards are the whole design. See [`MIN_EDIT_LEN`], [`MAX_EDIT_LEN`] and
/// [`EDIT_LENGTH_SLACK`] for why each one is there; without them this is both a
/// false-positive generator on short titles and a quadratic cost on a hot path.
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
    // A length gap alone bounds the achievable ratio at `1 - gap/long`, so a pair that cannot
    // clear the bar is rejected before the DP runs at all.
    if long - short > long / EDIT_LENGTH_SLACK {
        return 0.0;
    }
    let distance = edit_distance(a, b);
    1.0 - (distance as f32 / long as f32)
}

/// Levenshtein distance over `char`s, two rows at a time.
///
/// Over `char`s rather than bytes because the inputs are normalized titles, which keep CJK and
/// Hangul verbatim: a byte-wise distance would count one different ideograph as three edits and
/// score two Japanese titles as far apart as two unrelated ones.
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

/// Whether the query and *any* of the candidate's titles carry the same numbers.
///
/// The numbers are read off the **compact** key, so `10nen` and `10 nen` produce the same
/// signature and only a genuine difference — `overlord` against `overlord 2`, volume 3 against
/// volume 4 — registers as a conflict. Leading zeros are insignificant, so `001` and `1` agree.
pub(crate) fn numeric_signatures_agree(query_compact: &str, candidate: &Candidate) -> bool {
    let query_signature = numeric_signature(query_compact);
    std::iter::once(&candidate.normalized_title)
        .chain(candidate.alt_normalized_titles.iter())
        .any(|t| numeric_signature(&compact_key(t)) == query_signature)
}

/// The numbers a title carries, in order, as maximal runs of ASCII digits.
///
/// Parsed rather than compared as text so `001` and `1` agree, and saturating rather than
/// wrapping so a title carrying a 40-digit run cannot panic under the release profile's
/// overflow checks (SEC-11).
pub(crate) fn numeric_signature(compact: &str) -> Vec<u64> {
    let mut out = Vec::new();
    let mut current: Option<u64> = None;
    for c in compact.chars() {
        if let Some(digit) = c.to_digit(10) {
            let acc = current.unwrap_or(0);
            current = Some(acc.saturating_mul(10).saturating_add(u64::from(digit)));
        } else if let Some(value) = current.take() {
            out.push(value);
        }
    }
    if let Some(value) = current {
        out.push(value);
    }
    out
}

/// The Jaccard overlap of two case-insensitive name sets, in `[0,1]`, or `None` when either
/// side has nothing to compare (so the caller can skip the bonus entirely rather than
/// treating "no data" as "no overlap").
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
    // Both slices are non-empty by the guard above, so the union has at least one member and
    // the division cannot be `0 / 0`. A `union == 0` arm stood here and was unreachable.
    let union = sa.union(&sb).count();
    Some(sa.intersection(&sb).count() as f32 / union as f32)
}

/// Whether `a` and `b` share at least one name, compared case-insensitively.
#[must_use]
pub(crate) fn shares_a_name(a: &[String], b: &[String]) -> bool {
    a.iter()
        .any(|x| b.iter().any(|y| x.eq_ignore_ascii_case(y)))
}

/// Token-set ratio: the Jaccard overlap of the two titles' word sets, in `[0,1]`.
///
/// Order- and duplicate-insensitive, so "life starting in another world" and "another world
/// starting life" score identically. Pure and DB-free.
#[must_use]
#[expect(
    clippy::cast_precision_loss,
    reason = "word-set sizes are title token counts, orders of magnitude below f32's exact range"
)]
pub fn token_set_ratio(a: &str, b: &str) -> f32 {
    // Guards against `0 / 0`: two whitespace-only titles produce two empty token sets, and the
    // resulting `NaN` would propagate through every later term in `score` and compare false
    // against both thresholds.
    let sa: std::collections::BTreeSet<&str> = a.split_whitespace().collect();
    let sb: std::collections::BTreeSet<&str> = b.split_whitespace().collect();
    let union = sa.union(&sb).count();
    if union == 0 {
        return 0.0;
    }
    sa.intersection(&sb).count() as f32 / union as f32
}

/// Whether every word of `needle` also appears in `haystack` (both space-separated), with
/// `needle` having at least two tokens (so a single shared word is not treated as containment).
pub(crate) fn is_token_subset(needle: &str, haystack: &str) -> bool {
    let sn: std::collections::BTreeSet<&str> = needle.split_whitespace().collect();
    if sn.len() < 2 {
        return false;
    }
    let sh: std::collections::BTreeSet<&str> = haystack.split_whitespace().collect();
    sn.is_subset(&sh)
}
