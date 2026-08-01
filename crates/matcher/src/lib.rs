//! # tankovault-matcher
//!
//! Series canonicalisation scoring (design §10, steps 3–4). Pure and DB-free: the `db`
//! layer supplies trigram [`Candidate`]s, this crate scores them and returns a
//! [`Decision`]. Automation is aggressive where safe and human-reviewed where ambiguous.
//!
//! The nouns ([`Candidate`], [`Query`], [`Decision`], [`MatchSignals`], [`MergeVerdict`]) and
//! the [`Canonicaliser`] port they are exchanged over live in [`tankovault_domain::matching`]
//! and are re-exported here; this crate owns the *scoring*. `crates/db` names the port, not this
//! crate, so the repository layer can ask for a decision without linking a scorer (ARCH-16).
//!
//! # The shape of the score
//!
//! The base is the strongest of three views of the two titles, because each one is blind to a
//! failure the others catch:
//!
//! - the database's **trigram** similarity, which is what found the candidate at all;
//! - a **token-set ratio**, which survives word reordering (romaji vs. english order);
//! - a **compact** comparison — the titles with all whitespace removed — which is the only one
//!   that sees `Spy X Family` and `Spyxfamily` as the same string.
//!
//! That base is then moved by corroborating evidence (medium, release year, shared authors,
//! shared tags) and by one veto: [`MatchSignals::numeric_conflict`], which is what stands
//! between an aggressive matcher and merging `Overlord` into `Overlord 2`.
//!
//! Three bands:
//! - `>= high` → [`Decision::Attach`] (attach the new source to the existing series),
//! - `[low, high)` → [`Decision::Ambiguous`] (create the source, flag for operator review),
//! - `< low` → [`Decision::Create`] (a new canonical series).
//!
//! [`adjudicate`] answers the separate question of what to do about two series that *already*
//! exist, which is the one the merge queue and the automatic-merge sweep ask.

use tankovault_domain::{ContentType, SeriesId, compact_key};

// The scorer's input and output vocabulary lives in `tankovault_domain::matching`, because it
// is the seam `crates/db` has to name in order to *ask* for a decision rather than make one
// (ARCH-16 step 3). Re-exported here so `tankovault_matcher::Candidate` and friends still
// resolve — the scoring is what this crate owns, not the nouns.
pub use tankovault_domain::matching::{
    Candidate, Canonicaliser, Decision, MatchSignals, MergeVerdict, Query,
};

/// Confidence thresholds for the decision bands.
#[derive(Debug, Clone, Copy)]
pub struct Thresholds {
    pub high: f32,
    pub low: f32,
    /// At or above this score — **and** only with a structural identity signal — two series
    /// that already exist separately are merged without asking. See [`adjudicate`].
    pub auto_merge: f32,
}

impl Default for Thresholds {
    fn default() -> Self {
        Self {
            high: 0.85,
            low: 0.6,
            // Deliberately close to the ceiling. The automatic merge deletes a series row, and
            // the signals that can reach this number without a structural identity match are
            // exactly the fuzzy ones an operator should be looking at.
            auto_merge: 0.97,
        }
    }
}

/// A scored pair, with the rules that produced the score.
#[derive(Debug, Clone, Copy)]
pub struct Assessment {
    pub score: f32,
    pub signals: MatchSignals,
}

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

/// Score a single candidate against the query, capped at 1.0.
#[must_use]
pub fn score(query: &Query, candidate: &Candidate) -> f32 {
    assess(query, candidate).score
}

/// Score a candidate **and** report which rules produced the score.
///
/// The signals are not diagnostics: [`Decision::Ambiguous`] carries them into the merge queue's
/// stored reason, the operator console renders them as badges, and [`adjudicate`] refuses to
/// merge anything automatically without [`MatchSignals::is_structural`]. A caller that only
/// wants the number can use [`score`].
#[must_use]
pub fn assess(query: &Query, candidate: &Candidate) -> Assessment {
    let mut signals = MatchSignals::default();

    let query_compact = compact_key(&query.normalized_title);
    let title_match = best_title_match(&query.normalized_title, &query_compact, candidate);
    signals.exact_title = title_match.exact && !title_match.via_alias;
    signals.compact_identity = title_match.compact_equal && !title_match.via_alias;
    signals.alias_identity =
        (title_match.exact || title_match.compact_equal) && title_match.via_alias;
    signals.near_identical =
        !title_match.exact && !title_match.compact_equal && title_match.edit_ratio >= 0.9;
    signals.containment = title_match.containment;

    // Different numbers mean different works. Checked against *every* title the candidate
    // answers to, so a series whose alternative title carries the same volume number is not
    // penalised for its canonical title omitting it.
    signals.numeric_conflict = !numeric_signatures_agree(&query_compact, candidate);

    // Base similarity: the strongest of the three views described in the module docs. The
    // trigram score comes from the database and already covers the candidate's alternative
    // titles; the other two are computed here.
    let mut s = candidate.similarity.max(title_match.ratio);

    // Content-type agreement is a strong signal (a manhwa vs. manga split matters).
    if query.content_type != ContentType::Unknown && candidate.content_type != ContentType::Unknown
    {
        if query.content_type == candidate.content_type {
            signals.type_agreement = true;
            s += 0.08;
        } else {
            signals.type_conflict = true;
            s -= 0.15;
        }
    }

    // Release-year proximity. Saturating, not `(a - b).abs()`: `release_year` is an unvalidated
    // `i32` on `GET /v1/admin/sync/suggest`, and `i32::MIN - i32::MAX` overflowed — a panic in
    // debug *and*, since SEC-11 turned on `overflow-checks` for release, in production too. See
    // `tests/prop_scoring.rs::score_survives_the_extremes_of_the_release_year_range`.
    if let (Some(a), Some(b)) = (query.release_year, candidate.release_year) {
        match a.saturating_sub(b).saturating_abs() {
            0 => {
                signals.year_agreement = true;
                s += 0.06;
            }
            1 => {
                signals.year_agreement = true;
                s += 0.03;
            }
            d if d >= 3 => {
                signals.year_conflict = true;
                s -= 0.05;
            }
            _ => {}
        }
    }

    // Exact normalized-title equality is decisive.
    if title_match.exact || title_match.compact_equal {
        s += 0.1;
    } else if title_match.containment {
        // One title's words are a full subset of the other's (e.g. an abbreviated vs. a
        // fully-subtitled edition of the same work). A modest nudge, deliberately smaller
        // than the exact-match bonus so genuine sequels don't get over-attached.
        s += 0.05;
    }

    // Genre/tag overlap. A weak signal on its own (genres are coarse and inconsistently
    // tagged across sites), so the bonus is small and scales with how much the two sets
    // actually agree rather than firing at full strength on a single shared genre.
    if let Some(overlap) = name_set_overlap(&query.tags, &candidate.tags) {
        signals.tag_overlap = overlap > 0.0;
        s += 0.05 * overlap;
    }

    // A shared author/artist credit is a strong, low-false-positive signal: two unrelated
    // works with a similar title essentially never share a real person's name too.
    if shares_a_name(&query.authors, &candidate.authors) {
        signals.shared_author = true;
        s += 0.1;
    }

    // The veto, applied last so it bites whatever the rest of the scorer produced. A sequel's
    // title is *more* similar to its predecessor's the better the matcher works, so this is the
    // one term that has to be able to overcome a near-perfect fuzzy score. It is deliberately
    // large enough to drop a 0.89 pair (`overlord` against `overlord2`) below the create
    // threshold rather than merely out of the attach band, and [`decide`] refuses to attach on
    // it regardless of the arithmetic.
    if signals.numeric_conflict {
        s -= 0.3;
    }

    Assessment {
        score: s.clamp(0.0, 1.0),
        signals,
    }
}

/// How the query title compares against the best of the candidate's titles.
#[derive(Debug, Default, Clone, Copy)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "the return value of one comparison, private to this module and consumed field by \
              field a few lines later. Packing four independent yes/no answers into flags would \
              hide at the call site exactly what the names make obvious."
)]
struct TitleMatch {
    /// The best textual agreement in `[0,1]` across all of the candidate's titles.
    ratio: f32,
    /// The best edit-distance ratio specifically, which is what [`MatchSignals::near_identical`]
    /// reports (the overall `ratio` may have come from the token-set view instead).
    edit_ratio: f32,
    exact: bool,
    compact_equal: bool,
    containment: bool,
    /// Whether the winning title was an alternative rather than the canonical one.
    via_alias: bool,
}

/// Compare the query against the candidate's canonical title **and** each of its alternatives,
/// keeping the strongest agreement.
///
/// The trigram lookup that produced this candidate already searches `series_titles`, so a
/// candidate can be returned entirely on the strength of a synonym. Scoring only the canonical
/// title then re-judged that candidate against a name it does not go by, which is how a series
/// listed under its romaji title on one provider and its english title on another stayed split
/// even though the retrieval had already connected them.
fn best_title_match(query: &str, query_compact: &str, candidate: &Candidate) -> TitleMatch {
    let mut best = TitleMatch::default();
    let titles = std::iter::once((&candidate.normalized_title, false))
        .chain(candidate.alt_normalized_titles.iter().map(|t| (t, true)));

    for (title, is_alias) in titles {
        let compact = compact_key(title);
        let exact = !query.is_empty() && query == title;
        let compact_equal = !query_compact.is_empty() && query_compact == compact;

        // The token-set view: order- and duplicate-insensitive word overlap.
        let token = token_set_ratio(query, title);
        // The compact view: how many characters apart the two titles are once whitespace stops
        // mattering. Skipped entirely when identity already holds, when either title is too
        // short for the ratio to discriminate, or when the lengths alone rule out a useful
        // score — the last of which is what keeps this off the ingest hot path in the common
        // case of two unrelated titles.
        let edit = if exact || compact_equal {
            1.0
        } else {
            edit_ratio(query_compact, &compact)
        };

        let ratio = if exact || compact_equal {
            1.0
        } else {
            token.max(edit)
        };

        // `>` not `>=`, so the canonical title wins ties against an alternative and
        // `via_alias` reports the truth rather than whichever synonym happened to be last.
        if ratio > best.ratio {
            best.ratio = ratio;
            best.edit_ratio = edit;
            best.exact = exact;
            best.compact_equal = compact_equal;
            best.via_alias = is_alias;
        }
        // Containment is a property of the pair, not of the winning title: an abbreviated
        // canonical title with a fully subtitled synonym should still earn the nudge.
        best.containment |= is_token_subset(query, title) || is_token_subset(title, query);
    }
    best
}

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
fn edit_ratio(a: &str, b: &str) -> f32 {
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
fn edit_distance(a: &str, b: &str) -> usize {
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
fn numeric_signatures_agree(query_compact: &str, candidate: &Candidate) -> bool {
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
fn numeric_signature(compact: &str) -> Vec<u64> {
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
fn name_set_overlap(a: &[String], b: &[String]) -> Option<f32> {
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
fn shares_a_name(a: &[String], b: &[String]) -> bool {
    a.iter()
        .any(|x| b.iter().any(|y| x.eq_ignore_ascii_case(y)))
}

/// Token-set ratio: the Jaccard overlap of the two titles' word sets, in `[0,1]`. Order- and
/// duplicate-insensitive, so "life starting in another world" and "another world starting
/// life" score identically. Pure and DB-free.
#[must_use]
#[expect(
    clippy::cast_precision_loss,
    reason = "word-set sizes are title token counts, orders of magnitude below f32's exact range"
)]
pub fn token_set_ratio(a: &str, b: &str) -> f32 {
    // Three guards used to stand in front of this — `a.is_empty() || b.is_empty()`, an `a == b`
    // fast path, and `sa.is_empty() || sb.is_empty()` — and `cargo mutants` showed all three to
    // be dead: every input they answered, the Jaccard below answers identically, so a mutation
    // of any of them survived the whole suite. The one remaining guard is not dead: two
    // whitespace-only titles produce two empty sets, and `0 / 0` is `NaN`, which would then
    // propagate through every later term in `score` and compare false against both thresholds.
    //
    // The `a == b` path was the one worth thinking about, since it was also an allocation
    // shortcut for the common exact-title case. It went anyway: two `BTreeSet`s of a handful of
    // tokens cost nothing beside the trigram query that produced the candidate, and it was not
    // in fact behaviour-preserving — it answered `1.0` for two *whitespace-only* titles, i.e. a
    // perfect match between two series with no title at all.
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
fn is_token_subset(needle: &str, haystack: &str) -> bool {
    let sn: std::collections::BTreeSet<&str> = needle.split_whitespace().collect();
    if sn.len() < 2 {
        return false;
    }
    let sh: std::collections::BTreeSet<&str> = haystack.split_whitespace().collect();
    sn.is_subset(&sh)
}

/// The single best-scoring candidate for `query`, with its score — or `None` when there are
/// no candidates. Unlike [`decide`], this reports the raw best so a caller working across
/// several title variants (e.g. romaji/english/native) can pick the global maximum before
/// applying its own threshold.
#[must_use]
pub fn best_match(query: &Query, candidates: &[Candidate]) -> Option<(SeriesId, f32)> {
    best_assessment(query, candidates).map(|(id, a)| (id, a.score))
}

/// As [`best_match`], but keeping the signals that produced the winning score.
#[must_use]
pub fn best_assessment(query: &Query, candidates: &[Candidate]) -> Option<(SeriesId, Assessment)> {
    candidates
        .iter()
        .map(|c| (c.series_id, assess(query, c)))
        .max_by(|a, b| a.1.score.total_cmp(&b.1.score))
}

/// What to do about two series that **already exist** as separate rows.
///
/// [`decide`] answers a different question — where an incoming source belongs, before anything
/// is written. This one is asked by the merge queue and by the automatic-merge sweep, and its
/// affirmative is destructive: [`MergeVerdict::Auto`] ends with one of the two ids no longer
/// existing.
///
/// Which is why the bar is two-part rather than a number. A score alone can reach the automatic
/// threshold on nothing but fuzzy similarity, and two genuinely different works with similar
/// names are precisely the pairs that do. [`MatchSignals::is_structural`] means the titles are
/// *the same string* under a rule designed to be conservative — identical, identical modulo
/// whitespace, or an exact hit on a name the series already answers to — and only then does the
/// score decide.
///
/// ```
/// use tankovault_domain::matching::{MatchSignals, MergeVerdict};
/// use tankovault_matcher::{Assessment, Thresholds, adjudicate};
///
/// let t = Thresholds::default();
/// let structural = MatchSignals { compact_identity: true, ..MatchSignals::default() };
///
/// // Same string modulo whitespace, scoring at the ceiling: merge it.
/// assert_eq!(
///     adjudicate(Assessment { score: 1.0, signals: structural }, t),
///     MergeVerdict::Auto,
/// );
///
/// // The same score with no identity rule behind it is exactly what review is for.
/// let fuzzy = MatchSignals { near_identical: true, ..MatchSignals::default() };
/// assert_eq!(
///     adjudicate(Assessment { score: 1.0, signals: fuzzy }, t),
///     MergeVerdict::Review,
/// );
///
/// // And a numeric conflict is never merged automatically, whatever else agrees: a sequel's
/// // title resembles its predecessor's more closely the better the matcher works.
/// let sequel = MatchSignals { numeric_conflict: true, ..structural };
/// assert_ne!(
///     adjudicate(Assessment { score: 1.0, signals: sequel }, t),
///     MergeVerdict::Auto,
/// );
/// ```
#[must_use]
pub fn adjudicate(assessment: Assessment, thresholds: Thresholds) -> MergeVerdict {
    let Assessment { score, signals } = assessment;
    if signals.numeric_conflict {
        // Not merely "do not merge automatically": a pair whose titles carry different numbers
        // is reported as distinct rather than queued, because queueing it asks an operator to
        // re-derive the one fact the scorer is already certain about.
        return MergeVerdict::Distinct;
    }
    if score >= thresholds.auto_merge && signals.is_structural() {
        MergeVerdict::Auto
    } else if score >= thresholds.low {
        MergeVerdict::Review
    } else {
        MergeVerdict::Distinct
    }
}

/// Decide how to canonicalise the query given its candidates and thresholds.
///
/// Three outcomes, and the middle one is why this is a function rather than a comparison:
/// at or above [`Thresholds::high`] the source attaches to the existing series, below
/// [`Thresholds::low`] a new canonical series is created, and *between* them nothing is
/// decided automatically — the source is created and a merge candidate is queued for an
/// operator. Collapsing that band into either neighbour is how a catalogue either splits one
/// work across two entries or silently merges two different ones.
///
/// Only the best-scoring candidate is considered, so a long candidate list cannot outvote it.
///
/// The realistic case — the same work listed by a second provider:
///
/// ```
/// use tankovault_domain::{ContentType, SeriesId};
/// use tankovault_matcher::{Candidate, Decision, Query, Thresholds, decide};
///
/// let existing = SeriesId::new();
/// let query = Query {
///     normalized_title: "solo leveling".to_owned(),
///     content_type: ContentType::Manhwa,
///     release_year: Some(2018),
///     tags: Vec::new(),
///     authors: Vec::new(),
/// };
/// let same_work = Candidate {
///     series_id: existing,
///     normalized_title: "solo leveling".to_owned(),
///     // Even a poor raw trigram score attaches here: [`score`] takes the *strongest* of the
///     // trigram similarity, a token-set ratio and a compact comparison, and identical titles
///     // agree completely under all three.
///     similarity: 0.2,
///     alt_normalized_titles: Vec::new(),
///     content_type: ContentType::Manhwa,
///     release_year: Some(2018),
///     tags: Vec::new(),
///     authors: Vec::new(),
/// };
/// assert_eq!(
///     decide(&query, &[same_work], Thresholds::default()),
///     Decision::Attach(existing),
/// );
/// ```
///
/// The three bands, with every corroborating signal switched off (unknown medium, no year,
/// unrelated titles) so the score *is* the trigram similarity and the boundaries are visible:
///
/// ```
/// use tankovault_domain::{ContentType, SeriesId};
/// use tankovault_matcher::{Candidate, Decision, Query, Thresholds, decide};
///
/// let existing = SeriesId::new();
/// let query = Query {
///     normalized_title: "solo leveling".to_owned(),
///     content_type: ContentType::Unknown,
///     release_year: None,
///     tags: Vec::new(),
///     authors: Vec::new(),
/// };
/// let scoring = |similarity| Candidate {
///     series_id: existing,
///     normalized_title: "berserk".to_owned(),
///     similarity,
///     alt_normalized_titles: Vec::new(),
///     content_type: ContentType::Unknown,
///     release_year: None,
///     tags: Vec::new(),
///     authors: Vec::new(),
/// };
///
/// assert_eq!(
///     decide(&query, &[scoring(0.9)], Thresholds::default()),
///     Decision::Attach(existing),
/// );
/// assert!(matches!(
///     decide(&query, &[scoring(0.7)], Thresholds::default()),
///     Decision::Ambiguous { candidate, .. } if candidate == existing
/// ));
/// assert_eq!(
///     decide(&query, &[scoring(0.3)], Thresholds::default()),
///     Decision::Create,
/// );
///
/// // No candidates at all is the same answer as a bad one: the first source of a work has
/// // nothing to match against.
/// assert_eq!(decide(&query, &[], Thresholds::default()), Decision::Create);
/// ```
#[must_use]
pub fn decide(query: &Query, candidates: &[Candidate], thresholds: Thresholds) -> Decision {
    match best_assessment(query, candidates) {
        // The numeric guard is restated here rather than left to the arithmetic above. The
        // penalty in `assess` is sized to drop a realistic sequel pair out of the bands, but
        // "sized to" is not "guaranteed to", and attaching a sequel's sources onto its
        // predecessor is silent and expensive to undo.
        Some((id, a)) if a.score >= thresholds.high && !a.signals.numeric_conflict => {
            Decision::Attach(id)
        }
        Some((id, a)) if a.score >= thresholds.low => Decision::Ambiguous {
            candidate: id,
            score: a.score,
            signals: a.signals,
        },
        _ => Decision::Create,
    }
}

#[cfg(test)]
mod tests {
    // Tests assert exact equality of small, exactly-representable score values.
    #![expect(
        clippy::float_cmp,
        reason = "scores are compared against the exact constants the scorer is defined to \
                  produce; a tolerance here would stop the test detecting a changed weight"
    )]

    use super::*;

    fn cand(sim: f32, ct: ContentType, year: Option<i32>, title: &str) -> Candidate {
        Candidate {
            series_id: SeriesId::new(),
            normalized_title: title.to_owned(),
            similarity: sim,
            alt_normalized_titles: Vec::new(),
            content_type: ct,
            release_year: year,
            tags: Vec::new(),
            authors: Vec::new(),
        }
    }

    fn query(title: &str, ct: ContentType, year: Option<i32>) -> Query {
        Query {
            normalized_title: title.to_owned(),
            content_type: ct,
            release_year: year,
            tags: Vec::new(),
            authors: Vec::new(),
        }
    }

    #[test]
    fn strong_match_attaches() {
        let q = query("solo leveling", ContentType::Manhwa, Some(2018));
        let c = cand(0.82, ContentType::Manhwa, Some(2018), "solo leveling");
        let id = c.series_id;
        assert_eq!(
            decide(&q, &[c], Thresholds::default()),
            Decision::Attach(id)
        );
    }

    #[test]
    fn ambiguous_band_flags_for_review() {
        let q = query("the beginning after the end", ContentType::Unknown, None);
        let c = cand(0.65, ContentType::Unknown, None, "beginning after end");
        match decide(&q, &[c], Thresholds::default()) {
            Decision::Ambiguous { score, .. } => assert!((0.6..0.85).contains(&score)),
            other => panic!("expected ambiguous, got {other:?}"),
        }
    }

    #[test]
    fn weak_match_creates_new_series() {
        let q = query("some obscure title", ContentType::Manga, Some(2024));
        let c = cand(0.2, ContentType::Manhua, Some(2010), "unrelated work");
        assert_eq!(decide(&q, &[c], Thresholds::default()), Decision::Create);
    }

    #[test]
    fn content_type_disagreement_penalised() {
        let q = query("title", ContentType::Manga, None);
        let same = cand(0.8, ContentType::Manga, None, "title");
        let diff = cand(0.8, ContentType::Manhwa, None, "title");
        assert!(score(&q, &same) > score(&q, &diff));
    }

    #[test]
    fn shared_author_lifts_an_ambiguous_match() {
        let mut q = query("solo max leveler", ContentType::Unknown, None);
        q.authors = vec!["Chugong".to_owned()];
        let mut c = cand(0.62, ContentType::Unknown, None, "solo max levelling");
        c.authors = vec!["chugong".to_owned()]; // case-insensitive match
        let without_author = cand(0.62, ContentType::Unknown, None, "solo max levelling");
        assert!(score(&q, &c) > score(&q, &without_author));
    }

    #[test]
    fn tag_overlap_scales_with_agreement() {
        // A moderate, non-saturating base score (dissimilar-enough titles) so the tag
        // bonus is visible rather than swallowed by the 1.0 clamp.
        let mut q = query("silent night guardian tale", ContentType::Unknown, None);
        q.tags = vec!["Action".to_owned(), "Fantasy".to_owned()];
        let mut full_overlap = cand(0.3, ContentType::Unknown, None, "silent night watcher");
        full_overlap.tags = vec!["Action".to_owned(), "Fantasy".to_owned()];
        let mut partial_overlap = cand(0.3, ContentType::Unknown, None, "silent night watcher");
        partial_overlap.tags = vec!["Action".to_owned(), "Romance".to_owned()];
        let no_data = cand(0.3, ContentType::Unknown, None, "silent night watcher");

        assert!(score(&q, &full_overlap) > score(&q, &partial_overlap));
        assert!(score(&q, &partial_overlap) > score(&q, &no_data));
    }

    #[test]
    fn empty_candidates_creates() {
        let q = query("x", ContentType::Unknown, None);
        assert_eq!(decide(&q, &[], Thresholds::default()), Decision::Create);
    }

    #[test]
    fn token_set_ratio_is_order_insensitive() {
        assert_eq!(token_set_ratio("solo leveling", "solo leveling"), 1.0);
        assert_eq!(token_set_ratio("solo leveling", "leveling solo"), 1.0);
        assert_eq!(token_set_ratio("", "solo"), 0.0);
        // 3 shared of 4 total.
        assert_eq!(
            token_set_ratio("beginning after the end", "the beginning after end"),
            1.0
        );
        let r = token_set_ratio("the beginning after the end", "beginning after end");
        assert!((r - 0.75).abs() < 1e-6, "got {r}");
    }

    #[test]
    fn token_set_ratio_lifts_reordered_titles_over_weak_trigram() {
        // A low raw trigram score but a perfect token-set overlap should still attach.
        let q = query("another world reincarnation", ContentType::Unknown, None);
        let c = cand(
            0.30,
            ContentType::Unknown,
            None,
            "reincarnation another world",
        );
        let id = c.series_id;
        assert_eq!(
            decide(&q, &[c], Thresholds::default()),
            Decision::Attach(id)
        );
    }

    #[test]
    fn best_match_reports_the_global_best() {
        let q = query("solo leveling", ContentType::Manhwa, Some(2018));
        let weak = cand(0.4, ContentType::Manga, None, "some other series");
        let strong = cand(0.82, ContentType::Manhwa, Some(2018), "solo leveling");
        let id = strong.series_id;
        let (best_id, best_score) = best_match(&q, &[weak, strong]).unwrap();
        assert_eq!(best_id, id);
        assert!(best_score >= 0.85, "got {best_score}");
        assert!(best_match(&q, &[]).is_none());
    }

    #[test]
    fn subtitled_edition_gets_a_small_containment_nudge() {
        // Same work, one side fully subtitled: the subset nudge helps but stays conservative.
        let q = query("kanojo okarishimasu", ContentType::Manga, None);
        let c = cand(
            0.55,
            ContentType::Manga,
            None,
            "kanojo okarishimasu rent a girlfriend",
        );
        let s = score(&q, &c);
        assert!(s > 0.55, "containment should lift the score, got {s}");
    }

    // --- the whitespace-insensitive identity rule -------------------------------------------

    /// Two titles that differ only in where the spaces fall are the same title.
    ///
    /// The whole class comes from provider HTML: a missing space between two inline elements
    /// produces `Spyxfamily` for `Spy X Family`, `Wantsto Be Free` for `Wants to Be Free`,
    /// `Hanakimi` for `Hana Kimi`. Trigram similarity scores those pairs 0.37–0.58 and the
    /// token-set ratio scores most of them **zero**, so before the compact comparison existed
    /// they did not even reach the review queue: 59 such pairs sat unqueued in a 26k catalogue.
    #[test]
    fn a_missing_space_is_not_a_different_title() {
        for (a, b) in [
            ("spy x family", "spyxfamily"),
            ("hana kimi", "hanakimi"),
            ("day break", "daybreak"),
            (
                "the villainess just wants to live in peace",
                "the villainess just wantsto live in peace",
            ),
        ] {
            let q = query(a, ContentType::Unknown, None);
            // A raw trigram score far below the review band, so the compact rule is the only
            // thing that can be producing the outcome.
            let c = cand(0.4, ContentType::Unknown, None, b);
            let id = c.series_id;
            let a_ = assess(&q, &c);
            assert!(
                a_.signals.compact_identity,
                "{a:?} vs {b:?} should be compact-identical"
            );
            assert_eq!(
                decide(&q, &[c], Thresholds::default()),
                Decision::Attach(id),
                "{a:?} vs {b:?}"
            );
        }
    }

    /// The reported failure, end to end: an apostrophe spelled three ways is one series.
    ///
    /// `Sorry but I’m not Yuri` and `Sorry But Im Not Yuri` were two rows in the catalogue with
    /// a queued merge candidate at 0.80 that no operator had actioned. The normalization fix
    /// makes the two keys identical; this test is here so a future change to either layer has to
    /// keep them that way.
    #[test]
    fn the_same_title_with_and_without_an_apostrophe_attaches() {
        use tankovault_domain::normalize_title;
        let q = query(
            &normalize_title("Sorry but I\u{2019}m not Yuri"),
            ContentType::Unknown,
            Some(2025),
        );
        let c = cand(
            0.8,
            ContentType::Unknown,
            None,
            &normalize_title("Sorry But Im Not Yuri"),
        );
        let id = c.series_id;
        let assessment = assess(&q, &c);
        assert!(assessment.signals.exact_title, "{assessment:?}");
        assert_eq!(
            decide(&q, &[c], Thresholds::default()),
            Decision::Attach(id)
        );
    }

    /// A candidate found through one of its *alternative* titles is scored against that title.
    ///
    /// The trigram lookup already searches `series_titles`, so a series listed under its romaji
    /// name on one provider and its english name on another is retrieved — and was then
    /// re-scored against the canonical title only, which is a name the query does not go by. The
    /// candidate below has a deliberately unrelated canonical title so nothing but the alias
    /// rule can produce the outcome.
    #[test]
    fn an_exact_hit_on_an_alternative_title_counts() {
        let q = query("na honjaman level up", ContentType::Unknown, None);
        let mut c = cand(0.3, ContentType::Unknown, None, "solo leveling");
        c.alt_normalized_titles = vec!["na honjaman level up".to_owned()];
        let id = c.series_id;
        let assessment = assess(&q, &c);
        assert!(assessment.signals.alias_identity, "{assessment:?}");
        assert!(
            !assessment.signals.exact_title,
            "an alias hit is not an exact canonical-title hit"
        );
        assert_eq!(
            decide(&q, &[c], Thresholds::default()),
            Decision::Attach(id)
        );
    }

    // --- the numeric veto --------------------------------------------------------------------

    /// A sequel is not its predecessor, however similar the titles are.
    ///
    /// This is the safety half of every other rule in this file. Making the matcher more
    /// aggressive makes `Overlord` and `Overlord 2` score *higher*, not lower — the compact
    /// comparison puts them one character apart — so without a veto the same change that fixes
    /// the apostrophe class silently merges every numbered sequel into its first volume.
    #[test]
    fn differing_numbers_veto_a_match() {
        for (a, b) in [
            ("kingdom of the wind", "kingdom of the wind 2"),
            (
                "tensei shitara slime datta ken",
                "tensei shitara slime datta ken 2",
            ),
            ("dungeon reset volume 3", "dungeon reset volume 4"),
        ] {
            let q = query(a, ContentType::Unknown, None);
            // A trigram score that would otherwise attach outright.
            let c = cand(0.95, ContentType::Unknown, None, b);
            let assessment = assess(&q, &c);
            assert!(
                assessment.signals.numeric_conflict,
                "{a:?} vs {b:?} should conflict numerically"
            );
            assert!(
                !matches!(decide(&q, &[c], Thresholds::default()), Decision::Attach(_)),
                "{a:?} vs {b:?} must not attach"
            );
        }
    }

    /// The veto reads numbers off the compact key, so spacing around a number is not a conflict.
    ///
    /// `10nen Bun No Kotaeawase` and `10 Nenbun No Kotae Awase` are the same work with the
    /// digits attached to different words. Tokenising for numbers instead would have seen one
    /// title with no number at all and vetoed the pair.
    #[test]
    fn spacing_around_a_number_is_not_a_numeric_conflict() {
        let q = query("10nen bun no kotaeawase", ContentType::Unknown, None);
        let c = cand(0.55, ContentType::Unknown, None, "10 nenbun no kotae awase");
        let assessment = assess(&q, &c);
        assert!(!assessment.signals.numeric_conflict, "{assessment:?}");
        assert!(assessment.signals.compact_identity, "{assessment:?}");
    }

    /// Leading zeros are not a different number.
    #[test]
    fn the_numeric_signature_ignores_leading_zeros() {
        assert_eq!(numeric_signature("pure001mm"), vec![1]);
        assert_eq!(numeric_signature("pure1mm"), vec![1]);
        assert_eq!(numeric_signature("no digits here"), Vec::<u64>::new());
        assert_eq!(numeric_signature("a1b22c333"), vec![1, 22, 333]);
        // A digit run long enough to overflow saturates rather than panicking under the
        // release profile's overflow checks (SEC-11).
        assert_eq!(numeric_signature(&"9".repeat(40)), vec![u64::MAX]);
    }

    // --- the edit-distance window ------------------------------------------------------------

    /// The edit comparison is confined to titles long enough for it to mean something.
    ///
    /// One character out of seven is 0.857 — above the attach threshold — which is why
    /// `berserk` must not be compared this way to `berserl`. The same one-character difference
    /// over twenty is 0.95, and two distinct works whose names differ by one character in
    /// twenty essentially do not occur while providers producing exactly that do.
    #[test]
    fn the_edit_comparison_ignores_short_titles() {
        // Seven characters: below MIN_EDIT_LEN, so no edit ratio at all.
        assert_eq!(edit_ratio("berserk", "berserl"), 0.0);
        let q = query("berserk", ContentType::Unknown, None);
        let c = cand(0.3, ContentType::Unknown, None, "berserl");
        assert_eq!(decide(&q, &[c], Thresholds::default()), Decision::Create);

        // The window is keyed on the *shorter* side, which is the conservative choice: an
        // 11-character title is short whatever it is being compared against.
        assert_eq!(edit_ratio("munounananaa", "munounanana"), 0.0);

        // Long enough on both sides, and one character apart.
        let r = edit_ratio("gakuennomadonnawatanabe", "gakuennomadonawatanabe");
        assert!(r > 0.9, "got {r}");
    }

    /// A large length gap is rejected before the quadratic DP runs.
    #[test]
    fn a_length_gap_short_circuits_the_edit_comparison() {
        assert_eq!(edit_ratio("theverylongtitlehere", "short"), 0.0);
        // Beyond the cap the trigram score is doing the work, so the DP is skipped.
        assert_eq!(edit_ratio(&"a".repeat(200), &"a".repeat(200)), 0.0);
    }

    #[test]
    fn edit_distance_counts_characters_not_bytes() {
        // One different ideograph is one edit, not three (its UTF-8 length).
        assert_eq!(edit_distance("ワンピース", "ワンピーズ"), 1);
        assert_eq!(edit_distance("", "abc"), 3);
        assert_eq!(edit_distance("abc", "abc"), 0);
        assert_eq!(edit_distance("kitten", "sitting"), 3);
    }

    // --- the merge verdict -------------------------------------------------------------------

    /// An automatic, destructive merge needs an identity rule *and* a score — never a score
    /// alone.
    ///
    /// The distinction is the entire safety argument for automating the merge queue. A fuzzy
    /// score can reach the automatic threshold on two different works with similar names, and
    /// those are exactly the pairs an operator must see; a structural signal means the two
    /// titles are the same string under a rule that was written to be conservative.
    #[test]
    fn an_automatic_merge_requires_a_structural_signal() {
        let t = Thresholds::default();
        let structural = MatchSignals {
            compact_identity: true,
            ..MatchSignals::default()
        };
        let fuzzy = MatchSignals {
            near_identical: true,
            ..MatchSignals::default()
        };

        assert_eq!(
            adjudicate(
                Assessment {
                    score: 1.0,
                    signals: structural
                },
                t
            ),
            MergeVerdict::Auto
        );
        assert_eq!(
            adjudicate(
                Assessment {
                    score: 1.0,
                    signals: fuzzy
                },
                t
            ),
            MergeVerdict::Review
        );
        // Structural but not confident enough is still review, not merge.
        assert_eq!(
            adjudicate(
                Assessment {
                    score: 0.9,
                    signals: structural
                },
                t
            ),
            MergeVerdict::Review
        );
        // Below the review floor, nothing is recorded at all.
        assert_eq!(
            adjudicate(
                Assessment {
                    score: 0.4,
                    signals: structural
                },
                t
            ),
            MergeVerdict::Distinct
        );
    }

    /// A numeric conflict is reported as distinct rather than queued.
    ///
    /// Queueing it would ask an operator to re-establish the one fact the scorer is already
    /// certain about, and a review queue that fills with sequel pairs is a review queue nobody
    /// reads.
    #[test]
    fn a_numeric_conflict_is_never_queued() {
        let t = Thresholds::default();
        let signals = MatchSignals {
            compact_identity: true,
            numeric_conflict: true,
            ..MatchSignals::default()
        };
        assert_eq!(
            adjudicate(
                Assessment {
                    score: 1.0,
                    signals
                },
                t
            ),
            MergeVerdict::Distinct
        );
    }

    /// The signal labels are persisted in `merge_candidates.reason` and rendered as console
    /// badges, so they are part of the contract rather than debug output.
    #[test]
    fn signal_labels_are_stable_and_ordered() {
        let signals = MatchSignals {
            compact_identity: true,
            shared_author: true,
            year_conflict: true,
            ..MatchSignals::default()
        };
        assert_eq!(
            signals.labels(),
            vec!["compact_identity", "year_conflict", "shared_author"]
        );
        assert!(MatchSignals::default().labels().is_empty());
        assert!(signals.is_structural());
        assert!(
            !MatchSignals {
                near_identical: true,
                ..MatchSignals::default()
            }
            .is_structural()
        );
    }

    // --- the individual scoring terms (TESTING F-10, mutation testing) ---------------------
    //
    // Everything above asserts on an *ordering* — this candidate outscores that one — or on a
    // band. `cargo mutants` showed that to be too weak to hold the scorer in place: 22 mutants
    // of `score` and its helpers survived the whole suite, including deleting the exact-year
    // bonus outright, flipping the distant-year penalty into a bonus, and turning the tag
    // bonus from `0.05 * overlap` into `0.05 + overlap`. Each one changes what the matcher
    // attaches; none changed an ordering any test compared. The tests below pin the terms
    // themselves, which is the only assertion a weight change cannot slip past.
    //
    // Two titles that share no token, so the base score is the raw trigram similarity and
    // every bonus below is visible as an exact delta from it.
    const BASE_SIMILARITY: f32 = 0.5;
    fn unrelated_pair(ct: ContentType, year: Option<i32>) -> (Query, Candidate) {
        (
            query("alpha beta", ct, year),
            cand(BASE_SIMILARITY, ContentType::Unknown, None, "gamma delta"),
        )
    }

    /// Content-type agreement is consulted only when **both** sides know their type.
    ///
    /// `Unknown` means "not recorded", not "a type that differs from yours" — an adapter that
    /// does not publish a type would otherwise take a 0.15 penalty against every candidate,
    /// which is a third of the way from the ambiguous band to a rejection. Every earlier test
    /// gave both sides a known type, so flipping the `&&` in that guard to `||` survived.
    #[test]
    fn content_type_is_ignored_unless_both_sides_know_theirs() {
        let (q, c) = unrelated_pair(ContentType::Manga, None);
        assert!(
            (score(&q, &c) - BASE_SIMILARITY).abs() < 1e-6,
            "an unknown candidate type must be neutral, got {}",
            score(&q, &c)
        );

        let mut known = c.clone();
        known.content_type = ContentType::Manga;
        assert!((score(&q, &known) - (BASE_SIMILARITY + 0.08)).abs() < 1e-6);
        known.content_type = ContentType::Manhwa;
        assert!((score(&q, &known) - (BASE_SIMILARITY - 0.15)).abs() < 1e-6);
    }

    /// Release-year proximity is a **signed band**, and the two-year gap is deliberately
    /// neutral rather than a small penalty: reprints, regional releases and the difference
    /// between serialisation and volume publication routinely drift that far.
    ///
    /// Nothing asserted on this term at all before — every test either left `release_year`
    /// `None` or gave both sides the same year — so seven mutants of it survived, including
    /// deleting the exact-year arm and turning the distant-year penalty into a bonus.
    #[test]
    fn release_year_proximity_is_a_signed_band() {
        for (gap, expected) in [(0, 0.06_f32), (1, 0.03), (2, 0.0), (3, -0.05), (40, -0.05)] {
            let (q, mut c) = unrelated_pair(ContentType::Unknown, Some(2018));
            c.release_year = Some(2018 - gap);
            let s = score(&q, &c);
            assert!(
                (s - (BASE_SIMILARITY + expected)).abs() < 1e-6,
                "a {gap}-year gap must move the score by {expected}, got {s}"
            );
        }

        // The band is symmetric: a candidate published *later* is no different from one
        // published earlier by the same margin.
        let (q, mut c) = unrelated_pair(ContentType::Unknown, Some(2018));
        c.release_year = Some(2021);
        assert!((score(&q, &c) - (BASE_SIMILARITY - 0.05)).abs() < 1e-6);

        // One side missing a year is "no data", not "an infinite gap".
        let (q, c) = unrelated_pair(ContentType::Unknown, Some(2018));
        assert!((score(&q, &c) - BASE_SIMILARITY).abs() < 1e-6);
    }

    /// The containment nudge fires in **either** direction and is a bonus, not a penalty.
    ///
    /// Which side carries the fuller title is an accident of which site was scraped, so
    /// requiring the query to be the subset (the `&&` mutant) would silently halve the cases
    /// this helps. Deliberately smaller than the exact-match bonus so genuine sequels — whose
    /// titles *are* subsets of each other — do not get over-attached.
    #[test]
    fn the_containment_nudge_is_symmetric_and_positive() {
        // "alpha beta" ⊂ "alpha beta gamma": two shared tokens of three, so the base score is
        // the token-set ratio rather than the weaker trigram similarity.
        let expected = 2.0 / 3.0 + 0.05;
        let forward = score(
            &query("alpha beta", ContentType::Unknown, None),
            &cand(0.1, ContentType::Unknown, None, "alpha beta gamma"),
        );
        let backward = score(
            &query("alpha beta gamma", ContentType::Unknown, None),
            &cand(0.1, ContentType::Unknown, None, "alpha beta"),
        );
        assert!((forward - expected).abs() < 1e-6, "forward: {forward}");
        assert!((backward - expected).abs() < 1e-6, "backward: {backward}");
    }

    /// The tag bonus **scales** with agreement rather than firing at full strength.
    ///
    /// Genres are coarse and inconsistently tagged across sites, so a single shared "Action"
    /// must not count as much as a complete match. The earlier test asserted only that more
    /// overlap scores higher, which `0.05 + overlap` satisfies just as well as `0.05 *
    /// overlap` — and the additive form gives a *single* shared genre more weight than a
    /// shared author.
    #[test]
    fn the_tag_bonus_scales_with_the_overlap() {
        let (q_base, c_base) = unrelated_pair(ContentType::Unknown, None);
        let mut q = q_base;
        q.tags = vec!["Action".to_owned(), "Fantasy".to_owned()];

        // One shared of three distinct: a Jaccard overlap of 1/3.
        let mut partial = c_base.clone();
        partial.tags = vec!["Action".to_owned(), "Romance".to_owned()];
        let s = score(&q, &partial);
        assert!(
            (s - (BASE_SIMILARITY + 0.05 / 3.0)).abs() < 1e-6,
            "a one-third overlap must be worth a third of the bonus, got {s}"
        );

        let mut full = c_base.clone();
        full.tags.clone_from(&q.tags);
        assert!((score(&q, &full) - (BASE_SIMILARITY + 0.05)).abs() < 1e-6);
    }

    /// "No tags on one side" is not "no overlap", and the difference is why this returns
    /// `Option`: a candidate nobody has tagged must not be scored as one whose tags disagree.
    ///
    /// Unobservable through [`score`] — both answers add nothing — so it is asserted here, on
    /// the contract the doc comment states.
    #[test]
    fn missing_tag_data_is_none_not_zero_overlap() {
        let some = ["Action".to_owned()];
        assert_eq!(name_set_overlap(&[], &some), None);
        assert_eq!(name_set_overlap(&some, &[]), None);
        assert_eq!(name_set_overlap(&[], &[]), None);
        assert_eq!(name_set_overlap(&some, &["Romance".to_owned()]), Some(0.0));
    }

    /// Containment needs **two** shared words, so a single common one ("chronicles", "the
    /// world") is not treated as one title containing the other.
    #[test]
    fn one_shared_word_is_not_containment() {
        assert!(!is_token_subset("alpha", "alpha beta"));
        assert!(!is_token_subset("", "alpha beta"));
        assert!(is_token_subset("alpha beta", "alpha beta gamma"));
        assert!(is_token_subset(
            "alpha beta gamma",
            "alpha beta gamma delta"
        ));
        assert!(!is_token_subset("alpha beta", "alpha gamma"));
    }

    /// Two titles with no words left after normalisation are not a perfect match.
    ///
    /// They score `0.0`, and the reason the arm producing it cannot be deleted is arithmetic:
    /// an empty union makes the Jaccard `0 / 0`, and a `NaN` propagates through every later
    /// term in [`score`] and then compares `false` against *both* thresholds — a candidate
    /// that is neither attached, nor flagged, nor rejected.
    #[test]
    fn two_untitled_series_are_not_a_perfect_match() {
        assert_eq!(token_set_ratio("   ", "  "), 0.0);
        assert_eq!(token_set_ratio("", ""), 0.0);
        assert_eq!(token_set_ratio("   ", "alpha"), 0.0);
        // And the identity rules do not fire on them either: two series with no title are not
        // the same series, which is the failure mode an unguarded `==` on empty keys produces.
        let q = query("", ContentType::Unknown, None);
        let c = cand(0.0, ContentType::Unknown, None, "");
        let assessment = assess(&q, &c);
        assert!(!assessment.signals.exact_title);
        assert!(!assessment.signals.compact_identity);
    }
}
