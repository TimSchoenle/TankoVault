//! Scoring and adjudication tests. The suite shares its `cand`/`query` fixtures across
//! modules, so it stays one file rather than being split alongside the code it covers.

// Tests assert exact equality of small, exactly-representable score values.
#![expect(
    clippy::float_cmp,
    reason = "scores are compared against the exact constants the scorer is defined to \
                  produce; a tolerance here would stop the test detecting a changed weight"
)]

use super::*;
use crate::similarity::{
    edit_distance, edit_ratio, is_token_subset, name_set_overlap, numeric_signature,
};
use tankovault_domain::{ContentType, SeriesId};

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
