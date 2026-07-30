//! # tankovault-matcher
//!
//! Series canonicalisation scoring (design §10, steps 3–4). Pure and DB-free: the `db`
//! layer supplies trigram [`Candidate`]s, this crate scores them and returns a
//! [`Decision`]. Automation is aggressive where safe and human-reviewed where ambiguous.
//!
//! The nouns ([`Candidate`], [`Query`], [`Decision`]) and the [`Canonicaliser`] port they are
//! exchanged over live in [`tankovault_domain::matching`] and are re-exported here; this crate
//! owns the *scoring*. `crates/db` names the port, not this crate, so the repository layer can
//! ask for a decision without linking a scorer (ARCH-16).
//!
//! Score = trigram similarity, boosted by content-type agreement and release-year
//! proximity, capped at 1.0. Three bands:
//! - `>= high` → [`Decision::Attach`] (attach the new source to the existing series),
//! - `[low, high)` → [`Decision::Ambiguous`] (create the source, flag for operator review),
//! - `< low` → [`Decision::Create`] (a new canonical series).

use tankovault_domain::{ContentType, SeriesId};

// The scorer's input and output vocabulary lives in `tankovault_domain::matching`, because it
// is the seam `crates/db` has to name in order to *ask* for a decision rather than make one
// (ARCH-16 step 3). Re-exported here so `tankovault_matcher::Candidate` and friends still
// resolve — the scoring is what this crate owns, not the nouns.
pub use tankovault_domain::matching::{Candidate, Canonicaliser, Decision, Query};

/// Confidence thresholds for the decision bands.
#[derive(Debug, Clone, Copy)]
pub struct Thresholds {
    pub high: f32,
    pub low: f32,
}

impl Default for Thresholds {
    fn default() -> Self {
        Self {
            high: 0.85,
            low: 0.6,
        }
    }
}

/// Score a single candidate against the query, capped at 1.0.
#[must_use]
pub fn score(query: &Query, candidate: &Candidate) -> f32 {
    // Base similarity: the DB's raw trigram score OR a token-set ratio, whichever is
    // stronger. Trigram similarity is brittle when a title reorders words or carries extra
    // ones (romaji vs. english word order, season/part suffixes, sub-titles); the token-set
    // ratio recovers many of those cases so the auto-matcher links far more entries.
    let mut s = candidate.similarity.max(token_set_ratio(
        &query.normalized_title,
        &candidate.normalized_title,
    ));

    // Content-type agreement is a strong signal (a manhwa vs. manga split matters).
    if query.content_type != ContentType::Unknown && candidate.content_type != ContentType::Unknown
    {
        if query.content_type == candidate.content_type {
            s += 0.08;
        } else {
            s -= 0.15;
        }
    }

    // Release-year proximity. Saturating, not `(a - b).abs()`: `release_year` is an unvalidated
    // `i32` on `GET /v1/admin/sync/suggest`, and `i32::MIN - i32::MAX` overflowed — a panic in
    // debug *and*, since SEC-11 turned on `overflow-checks` for release, in production too. See
    // `tests/prop_scoring.rs::score_survives_the_extremes_of_the_release_year_range`.
    if let (Some(a), Some(b)) = (query.release_year, candidate.release_year) {
        match a.saturating_sub(b).saturating_abs() {
            0 => s += 0.06,
            1 => s += 0.03,
            d if d >= 3 => s -= 0.05,
            _ => {}
        }
    }

    // Exact normalized-title equality is decisive.
    if query.normalized_title == candidate.normalized_title {
        s += 0.1;
    } else if is_token_subset(&query.normalized_title, &candidate.normalized_title)
        || is_token_subset(&candidate.normalized_title, &query.normalized_title)
    {
        // One title's words are a full subset of the other's (e.g. an abbreviated vs. a
        // fully-subtitled edition of the same work). A modest nudge, deliberately smaller
        // than the exact-match bonus so genuine sequels don't get over-attached.
        s += 0.05;
    }

    // Genre/tag overlap. A weak signal on its own (genres are coarse and inconsistently
    // tagged across sites), so the bonus is small and scales with how much the two sets
    // actually agree rather than firing at full strength on a single shared genre.
    if let Some(overlap) = name_set_overlap(&query.tags, &candidate.tags) {
        s += 0.05 * overlap;
    }

    // A shared author/artist credit is a strong, low-false-positive signal: two unrelated
    // works with a similar title essentially never share a real person's name too.
    if shares_a_name(&query.authors, &candidate.authors) {
        s += 0.1;
    }

    s.clamp(0.0, 1.0)
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
    candidates
        .iter()
        .map(|c| (c.series_id, score(query, c)))
        .max_by(|a, b| a.1.total_cmp(&b.1))
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
///     // Even a poor raw trigram score attaches here: [`score`] takes the *stronger* of the
///     // trigram similarity and a token-set ratio, and identical titles agree completely.
///     similarity: 0.2,
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
    let best = candidates
        .iter()
        .map(|c| (c.series_id, score(query, c)))
        .max_by(|a, b| a.1.total_cmp(&b.1));

    match best {
        Some((id, s)) if s >= thresholds.high => Decision::Attach(id),
        Some((id, s)) if s >= thresholds.low => Decision::Ambiguous {
            candidate: id,
            score: s,
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
    }
}
