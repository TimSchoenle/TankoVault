//! Scoring and adjudication tests. The suite shares its `cand`/`query` fixtures across
//! modules, so it stays one file rather than being split alongside the code it covers.

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

/// A disagreeing medium is recorded, and costs nothing.
///
/// It used to cost 0.15. Providers guess the medium from the site they scraped (manga against
/// manhwa against webtoon for one work), so production switched the type guard off, and a
/// penalty that cannot be trusted as a guard cannot be trusted as a weight either.
#[test]
fn content_type_disagreement_is_recorded_not_scored() {
    let q = query("title", ContentType::Manga, None);
    let same = cand(0.8, ContentType::Manga, None, "title");
    let diff = cand(0.8, ContentType::Manhwa, None, "title");
    assert!(assess(&q, &diff).signals.type_conflict);
    assert!((score(&q, &same) - score(&q, &diff)).abs() < 1e-6);
}

/// **A shared author does not lift an ambiguous match.**
///
/// This pinned the opposite until the matching overhaul. The `shared_author` bonus (+0.10),
/// with the medium and year bonuses, is what lifted a discounted alternative-title hit (0.80)
/// back over the attach and merge thresholds. Production credits are wrong or spelled
/// inconsistently across providers most of the time, and the route it opened is the one the
/// evidence condemns: 12,356 automatic alias merges fired a metadata guard that had been switched
/// off for noise, and a dozen hub series absorbed 20–100 different works each
/// (`docs/MATCHING_OVERHAUL.md` §2). The signal is still recorded and rendered.
#[test]
fn a_shared_author_does_not_lift_an_ambiguous_match() {
    let mut q = query("solo max leveler", ContentType::Unknown, None);
    q.authors = vec!["Chugong".to_owned()];
    let mut c = cand(0.62, ContentType::Unknown, None, "solo max levelling");
    c.authors = vec!["chugong".to_owned()]; // case-insensitive match
    let without_author = cand(0.62, ContentType::Unknown, None, "solo max levelling");
    assert!(assess(&q, &c).signals.shared_author);
    assert!((score(&q, &c) - score(&q, &without_author)).abs() < 1e-6);
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

/// A perfect token-set overlap lifts a weak trigram score to the top of the band — and with no
/// identity rule behind it, that is a question for the sweep rather than an attach.
///
/// It used to attach. The fuzzy route is what attached `Onikakushi-hen` to `Onisarashi-hen` and
/// `A Year of Marvels: October` to `November`: a high similarity says the titles look alike, not
/// that they name the same work.
#[test]
fn token_set_ratio_lifts_reordered_titles_into_review() {
    let q = query("another world reincarnation", ContentType::Unknown, None);
    let c = cand(
        0.30,
        ContentType::Unknown,
        None,
        "reincarnation another world",
    );
    let id = c.series_id;
    assert!(score(&q, &c) >= Thresholds::default().high);
    assert!(matches!(
        decide(&q, &[c], Thresholds::default()),
        Decision::Ambiguous { candidate, .. } if candidate == id
    ));
}

/// An identical candidate wins over a reordered one at the same clamped score.
///
/// Both reach 1.0, and ranked on score alone the tie went to whichever came last — the reordered
/// one, which cannot attach — so a source with an exact match in hand was queued instead.
#[test]
fn an_identical_candidate_outranks_a_fuzzy_one_at_the_same_score() {
    let q = query("another world reincarnation", ContentType::Unknown, None);
    let exact = cand(
        0.3,
        ContentType::Unknown,
        None,
        "another world reincarnation",
    );
    let reordered = cand(
        0.3,
        ContentType::Unknown,
        None,
        "reincarnation another world",
    );
    let id = exact.series_id;
    for candidates in [
        [exact.clone(), reordered.clone()],
        [reordered.clone(), exact.clone()],
    ] {
        assert_eq!(
            decide(&q, &candidates, Thresholds::default()),
            Decision::Attach(id)
        );
    }
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

/// A candidate found through one of its *alternative* titles is scored against that title —
/// but on its own that is evidence, not identity, so it lands in the review band.
///
/// Both halves are bugs that happened. The trigram lookup already searches `series_titles`, so
/// a series listed under its romaji name on one provider and its english name on another is
/// retrieved — and was then re-scored against the canonical title only, which is a name the
/// query does not go by, leaving the two split. Fixing that by treating an exact alias hit as
/// full identity produced the opposite failure: aliases are scraped free text, an attach files
/// the attaching source's aliases under the series it joined, and the widening net put 309
/// sources for unrelated works onto one live row. The candidate below has a deliberately
/// unrelated canonical title so nothing but the alias rule can produce the outcome.
#[test]
fn an_uncorroborated_hit_on_an_alternative_title_is_held_for_review() {
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
    assert!(
        assessment.score >= Thresholds::default().low
            && assessment.score < Thresholds::default().high,
        "an alias hit alone belongs in the review band, got {assessment:?}"
    );
    assert!(matches!(
        decide(&q, &[c], Thresholds::default()),
        Decision::Ambiguous { candidate, .. } if candidate == id
    ));
}

/// **An alias hit that agrees on the medium is still held for review.**
///
/// This pinned the opposite until the matching overhaul: one agreeing medium, year or credit
/// lifted an alias hit back to an automatic attach. Those fields are unreliable across
/// providers, and the alias-plus-metadata route is the one production condemns — 12,356 alias
/// merges where a disabled metadata guard fired, and hub series that absorbed 20–100 different
/// works through junk and franchise aliases (`docs/MATCHING_OVERHAUL.md` §2). The cross-provider
/// romaji/english case is queued and settled by the sweep, which can corroborate it.
#[test]
fn an_alias_hit_that_agrees_on_the_medium_is_still_held_for_review() {
    let mut q = query("na honjaman level up", ContentType::Manhwa, Some(2018));
    q.authors = vec!["Chugong".to_owned()];
    let mut c = cand(0.3, ContentType::Manhwa, Some(2018), "solo leveling");
    c.alt_normalized_titles = vec!["na honjaman level up".to_owned()];
    c.authors = vec!["Chugong".to_owned()];
    let id = c.series_id;
    assert!(matches!(
        decide(&q, &[c], Thresholds::default()),
        Decision::Ambiguous { candidate, .. } if candidate == id
    ));
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

/// A standalone roman numeral or spelled ordinal is a number; `x`, `v` and `i` are not.
///
/// `Overlord` absorbed `Overlord II` in production: neither title carries a digit, so the two
/// signatures agreed and nothing distinguished the sequel.
#[test]
fn roman_numerals_and_ordinals_are_numbers() {
    assert_eq!(numeric_signature("overlord ii"), vec![2]);
    assert_eq!(numeric_signature("overlord 2"), vec![2]);
    assert_eq!(numeric_signature("the second coming"), vec![2]);
    assert_eq!(numeric_signature("season 2nd"), vec![2]);
    assert_eq!(numeric_signature("spy x family"), Vec::<u64>::new());
    assert_eq!(numeric_signature("batman v superman"), Vec::<u64>::new());
    // Inside a word they are letters.
    assert_eq!(numeric_signature("viii"), vec![8]);
    assert_eq!(numeric_signature("vivid strike"), Vec::<u64>::new());

    let q = query("overlord ii", ContentType::Unknown, None);
    let c = cand(0.9, ContentType::Unknown, None, "overlord");
    assert!(assess(&q, &c).signals.numeric_conflict);
    assert_eq!(decide(&q, &[c], Thresholds::default()), Decision::Create);
}

/// **A sequel's alias on a series does not wave the next sequel through.**
///
/// The numeric check used to pass when *any* title of the candidate carried the query's
/// numbers. Once `douluo dalu 4 zhongji douluo` landed on `Douluo Dalu 5` as an alias, Douluo
/// Dalu 4 attached to and merged into 5. A conflict with the canonical title is a conflict
/// whatever an alias says.
#[test]
fn an_alias_with_matching_numbers_does_not_excuse_the_canonical_title() {
    let q = query(
        "douluo dalu 4 zhongji douluo",
        ContentType::Manhua,
        Some(2023),
    );
    let mut c = cand(
        1.0,
        ContentType::Manhua,
        Some(2023),
        "douluo dalu 5 rebirth of tang san",
    );
    c.alt_normalized_titles = vec!["douluo dalu 4 zhongji douluo".to_owned()];
    assert!(assess(&q, &c).signals.numeric_conflict);
    assert_eq!(decide(&q, &[c], Thresholds::default()), Decision::Create);

    // The same pair as two existing series, read the way the sweep reads it.
    let left_alts = vec!["douluo dalu 4 zhongji douluo".to_owned()];
    let (_, verdict) = adjudicate_pair(
        side("douluo dalu 5 rebirth of tang san", &left_alts),
        side("douluo dalu 4 zhongji douluo", &[]),
        1.0,
        Thresholds::default(),
    );
    assert_eq!(verdict.verdict, MergeVerdict::Distinct);
    assert_eq!(verdict.reason, "numeric_conflict");
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

/// An automatic, destructive merge needs a primary identity *and* a score, or a corroborated
/// alias — never a score alone, and never an alias alone.
///
/// The distinction is the entire safety argument for automating the merge queue. A fuzzy
/// score can reach the automatic threshold on two different works with similar names, and
/// those are exactly the pairs an operator must see; a primary identity means the two titles
/// are the same string under a rule that was written to be conservative.
#[test]
fn an_automatic_merge_requires_a_primary_identity_or_a_corroborated_alias() {
    let t = Thresholds::default();
    let identical = MatchSignals {
        compact_identity: true,
        ..MatchSignals::default()
    };
    let fuzzy = MatchSignals {
        near_identical: true,
        ..MatchSignals::default()
    };
    let alias = MatchSignals {
        alias_identity: true,
        ..MatchSignals::default()
    };
    let by_id = MatchSignals {
        external_id_agreement: true,
        ..alias
    };
    let by_timing = MatchSignals {
        live_timing_agreement: true,
        ..alias
    };

    let verdict = |score, signals| adjudicate(Assessment { score, signals }, t).verdict;

    assert_eq!(verdict(1.0, identical), MergeVerdict::Auto);
    assert_eq!(verdict(1.0, fuzzy), MergeVerdict::Review);
    assert_eq!(verdict(1.0, alias), MergeVerdict::Review);
    assert_eq!(verdict(0.8, by_id), MergeVerdict::Auto);
    assert_eq!(verdict(0.8, by_timing), MergeVerdict::Auto);
    // An identity but not confident enough is still review, not merge.
    assert_eq!(verdict(0.9, identical), MergeVerdict::Review);
    // Below the review floor, nothing is recorded at all — corroborated or not.
    assert_eq!(verdict(0.4, identical), MergeVerdict::Distinct);
    assert_eq!(verdict(0.4, by_timing), MergeVerdict::Distinct);

    // Each answer names itself, so a queue row can be triaged without re-deriving the reason
    // from a score and a bag of signal names.
    let reason = |score, signals| adjudicate(Assessment { score, signals }, t).reason;
    assert_eq!(
        reason(1.0, identical),
        "structural_identity_above_threshold"
    );
    assert_eq!(reason(0.8, by_id), "corroborated_alias");
    assert_eq!(reason(1.0, fuzzy), "no_structural_identity");
    assert_eq!(reason(1.0, alias), "alias_uncorroborated");
    assert_eq!(reason(0.9, identical), "below_auto_merge_threshold");
    assert_eq!(reason(0.4, identical), "below_review_floor");
}

/// External-id and live-timing conflicts hold even an identical pair for review, and say so.
#[test]
fn trusted_conflicts_hold_a_merge_for_review() {
    let identical = MatchSignals {
        exact_title: true,
        ..MatchSignals::default()
    };
    for (veto, signals) in [
        (
            "external_id_conflict",
            MatchSignals {
                external_id_conflict: true,
                ..identical
            },
        ),
        (
            "live_timing_conflict",
            MatchSignals {
                live_timing_conflict: true,
                ..identical
            },
        ),
    ] {
        let held = adjudicate(
            Assessment {
                score: 1.0,
                signals,
            },
            Thresholds::default(),
        );
        assert_eq!(held.verdict, MergeVerdict::Review, "{veto}");
        assert_eq!(held.reason, veto);
        assert_eq!(held.blocked_by, vec![veto]);
    }
}

/// Each guard holds an otherwise-automatic merge for review, names itself, and is switchable
/// without changing the score or the signal that fired.
///
/// The guards exist because a score is one number and cannot express "these titles agree and
/// the works do not". Every case below scores at the ceiling with a structural identity — the
/// full automatic bar — and is still not merged.
#[test]
fn each_guard_holds_an_automatic_merge_for_review() {
    let structural = MatchSignals {
        compact_identity: true,
        ..MatchSignals::default()
    };
    let cases = [
        (
            "author_conflict",
            MatchSignals {
                author_conflict: true,
                ..structural
            },
        ),
        (
            "year_conflict",
            MatchSignals {
                year_conflict: true,
                ..structural
            },
        ),
        (
            "type_conflict",
            MatchSignals {
                type_conflict: true,
                ..structural
            },
        ),
    ];

    for (label, signals) in cases {
        let held = adjudicate(
            Assessment {
                score: 1.0,
                signals,
            },
            Thresholds::default(),
        );
        assert_eq!(held.verdict, MergeVerdict::Review, "{label}");
        assert_eq!(held.blocked_by, vec![label], "{label}");
        assert_eq!(held.reason, label, "{label}");

        // Switching only that guard off merges the pair; the signal still fires and is still
        // reported, which is the difference between disabling a guard and disabling a rule.
        let mut guards = MergeGuards::default();
        match label {
            "author_conflict" => guards.author_conflict = false,
            "year_conflict" => guards.year_conflict = false,
            _ => guards.type_conflict = false,
        }
        let merged = adjudicate(
            Assessment {
                score: 1.0,
                signals,
            },
            Thresholds {
                guards,
                ..Thresholds::default()
            },
        );
        assert_eq!(merged.verdict, MergeVerdict::Auto, "{label}");
        assert!(merged.blocked_by.is_empty(), "{label}");
    }
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
    let apart = adjudicate(
        Assessment {
            score: 1.0,
            signals,
        },
        t,
    );
    assert_eq!(apart.verdict, MergeVerdict::Distinct);
    assert_eq!(apart.reason, "numeric_conflict");
}

/// The signal labels are persisted in `merge_candidates.reason` and rendered as console
/// badges, so they are part of the contract rather than debug output. New ones are appended.
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
    assert_eq!(MatchSignals::default().labels(), Vec::<&str>::new());

    let every = MatchSignals {
        exact_title: true,
        compact_identity: true,
        alias_identity: true,
        near_identical: true,
        containment: true,
        type_agreement: true,
        type_conflict: true,
        year_agreement: true,
        year_conflict: true,
        shared_author: true,
        author_conflict: true,
        tag_overlap: true,
        numeric_conflict: true,
        source_title_identity: true,
        edition_conflict: true,
        external_id_agreement: true,
        external_id_conflict: true,
        live_timing_agreement: true,
        live_timing_conflict: true,
    };
    assert_eq!(
        every.labels(),
        vec![
            "exact_title",
            "compact_identity",
            "alias_identity",
            "near_identical",
            "containment",
            "type_agreement",
            "type_conflict",
            "year_agreement",
            "year_conflict",
            "shared_author",
            "author_conflict",
            "tag_overlap",
            "numeric_conflict",
            "source_title_identity",
            "edition_conflict",
            "external_id_agreement",
            "external_id_conflict",
            "live_timing_agreement",
            "live_timing_conflict",
        ]
    );
}

/// Which signals are identity, and which alias hits count as corroborated.
#[test]
fn primary_identity_excludes_an_alias_and_corroboration_needs_a_trusted_signal() {
    let only = |f: fn(&mut MatchSignals)| {
        let mut signals = MatchSignals::default();
        f(&mut signals);
        signals
    };
    assert!(only(|s| s.exact_title = true).is_primary_identity());
    assert!(only(|s| s.compact_identity = true).is_primary_identity());
    assert!(only(|s| s.source_title_identity = true).is_primary_identity());
    assert!(!only(|s| s.alias_identity = true).is_primary_identity());
    assert!(!only(|s| s.near_identical = true).is_primary_identity());

    assert!(!only(|s| s.alias_identity = true).is_corroborated_alias());
    assert!(
        only(|s| {
            s.alias_identity = true;
            s.external_id_agreement = true;
        })
        .is_corroborated_alias()
    );
    assert!(
        only(|s| {
            s.alias_identity = true;
            s.live_timing_agreement = true;
        })
        .is_corroborated_alias()
    );
    // Metadata is not corroboration.
    assert!(
        !only(|s| {
            s.alias_identity = true;
            s.shared_author = true;
            s.type_agreement = true;
            s.year_agreement = true;
        })
        .is_corroborated_alias()
    );
}

// --- the individual scoring terms (TESTING F-10, mutation testing) ---------------------
//
// Everything above asserts on an *ordering* — this candidate outscores that one — or on a
// band. `cargo mutants` showed that to be too weak to hold the scorer in place: 22 mutants
// of `score` and its helpers survived the whole suite. The tests below pin the terms
// themselves, which is the only assertion a weight change cannot slip past — including the
// zero weight the metadata terms now carry.
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

/// Content-type agreement is recorded only when **both** sides know their type, and never
/// moves the score.
///
/// `Unknown` means "not recorded", not "a type that differs from yours": an adapter that does
/// not publish a type would otherwise show a conflict against every candidate — and the type
/// guard, when an operator switches it on, would hold every one of them.
#[test]
fn content_type_is_recorded_only_when_both_sides_know_theirs() {
    let (q, c) = unrelated_pair(ContentType::Manga, None);
    let signals = assess(&q, &c).signals;
    assert!(
        !signals.type_agreement && !signals.type_conflict,
        "{signals:?}"
    );

    let mut known = c.clone();
    known.content_type = ContentType::Manga;
    assert!(assess(&q, &known).signals.type_agreement);
    assert!((score(&q, &known) - BASE_SIMILARITY).abs() < 1e-6);
    known.content_type = ContentType::Manhwa;
    assert!(assess(&q, &known).signals.type_conflict);
    assert!((score(&q, &known) - BASE_SIMILARITY).abs() < 1e-6);
}

/// Release-year proximity is a **band** of signals that never moves the score: within a year
/// agrees, two years is deliberately neither (reprints, regional releases and serialisation
/// against volume publication routinely drift that far), three or more conflicts.
#[test]
fn release_year_proximity_is_a_band_of_signals() {
    for (gap, agree, conflict) in [
        (0, true, false),
        (1, true, false),
        (2, false, false),
        (3, false, true),
        (40, false, true),
        // Symmetric: a candidate published later is no different from one published earlier.
        (-3, false, true),
    ] {
        let (q, mut c) = unrelated_pair(ContentType::Unknown, Some(2018));
        c.release_year = Some(2018 - gap);
        let assessment = assess(&q, &c);
        assert_eq!(assessment.signals.year_agreement, agree, "{gap}");
        assert_eq!(assessment.signals.year_conflict, conflict, "{gap}");
        assert!((assessment.score - BASE_SIMILARITY).abs() < 1e-6, "{gap}");
    }

    // One side missing a year is "no data", not "an infinite gap".
    let (q, c) = unrelated_pair(ContentType::Unknown, Some(2018));
    let signals = assess(&q, &c).signals;
    assert!(!signals.year_agreement && !signals.year_conflict);
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

/// Shared genres are recorded and never weighed: genres are coarse and inconsistently tagged
/// across sites, and two unrelated cultivation manhua share every one of them.
#[test]
fn tag_overlap_is_recorded_not_scored() {
    let (q_base, c_base) = unrelated_pair(ContentType::Unknown, None);
    let mut q = q_base;
    q.tags = vec!["Action".to_owned(), "Fantasy".to_owned()];

    let mut partial = c_base.clone();
    partial.tags = vec!["Action".to_owned(), "Romance".to_owned()];
    let mut full = c_base.clone();
    full.tags.clone_from(&q.tags);
    let mut disjoint = c_base;
    disjoint.tags = vec!["Romance".to_owned()];

    for (c, overlaps) in [(&partial, true), (&full, true), (&disjoint, false)] {
        let assessment = assess(&q, c);
        assert_eq!(assessment.signals.tag_overlap, overlaps, "{:?}", c.tags);
        assert!((assessment.score - BASE_SIMILARITY).abs() < 1e-6);
    }
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

// --- title rules: editions, junk aliases, franchise names --------------------------------

fn side<'a>(title: &'a str, alts: &'a [String]) -> PairSide<'a> {
    PairSide {
        series_id: SeriesId::new(),
        normalized_title: title,
        alt_normalized_titles: alts,
        content_type: ContentType::Manga,
        release_year: Some(2005),
        tags: &[],
        authors: &[],
    }
}

/// **Two arcs of one work are two works.**
///
/// `Higurashi no Naku Koro ni Onikakushi-hen` and `…Onisarashi-hen` were merged through their
/// shared english title, and the fuzzy route attached one arc's listing to the other at ingest:
/// the titles differ in two letters out of forty.
#[test]
fn different_arcs_are_distinct() {
    let shared = vec!["higurashi when they cry".to_owned()];
    let (reading, verdict) = adjudicate_pair(
        side("higurashi no naku koro ni onikakushi hen", &shared),
        side("higurashi no naku koro ni onisarashi hen", &shared),
        1.0,
        Thresholds::default(),
    );
    assert!(reading.assessment.signals.edition_conflict, "{reading:?}");
    assert_eq!(verdict.verdict, MergeVerdict::Distinct);
    assert_eq!(verdict.reason, "edition_conflict");

    let q = query(
        "higurashi no naku koro ni onikakushi hen",
        ContentType::Unknown,
        None,
    );
    let c = cand(
        0.9,
        ContentType::Unknown,
        None,
        "higurashi no naku koro ni onisarashi hen",
    );
    assert!(!matches!(
        decide(&q, &[c], Thresholds::default()),
        Decision::Attach(_)
    ));
}

/// **`Railgun S` is not `Railgun T`.** A single trailing letter names a season; the two were
/// merged through the shared english title `A Certain Scientific Railgun`.
#[test]
fn a_trailing_season_letter_is_an_edition() {
    let shared = vec!["a certain scientific railgun".to_owned()];
    let (_, verdict) = adjudicate_pair(
        side("toaru kagaku no railgun s", &shared),
        side("toaru kagaku no railgun t", &shared),
        1.0,
        Thresholds::default(),
    );
    assert_eq!(verdict.verdict, MergeVerdict::Distinct);
    assert_eq!(verdict.reason, "edition_conflict");
}

/// **`Tokyo Ghoul:re` is not `Tokyo Ghoul`.** It carried the franchise name as an alias and was
/// absorbed through it.
#[test]
fn a_sequel_suffix_is_an_edition() {
    let franchise = vec!["tokyo ghoul".to_owned()];
    let (reading, verdict) = adjudicate_pair(
        side("tokyo ghoul", &[]),
        side("tokyo ghoul re", &franchise),
        1.0,
        Thresholds::default(),
    );
    assert!(reading.assessment.signals.edition_conflict, "{reading:?}");
    assert_eq!(verdict.verdict, MergeVerdict::Distinct);
}

/// **A junk alias is not an identity.** `unknown` alone merged 11 series into 9 survivors:
/// placeholder text scraped from an empty metadata cell, shared by every series a provider had
/// no alternative title for.
#[test]
fn a_junk_alias_is_not_an_identity() {
    let junk = vec!["unknown".to_owned()];
    let (reading, verdict) = adjudicate_pair(
        side("martial peak", &junk),
        side("tales of demons and gods", &junk),
        1.0,
        Thresholds::default(),
    );
    assert!(!reading.assessment.signals.alias_identity, "{reading:?}");
    assert_ne!(verdict.verdict, MergeVerdict::Auto);

    // At ingest too: a listing whose only match is a junk alias is not an alias hit.
    let q = query("unknown", ContentType::Unknown, None);
    let mut c = cand(1.0, ContentType::Unknown, None, "martial peak");
    c.alt_normalized_titles = junk;
    assert!(!assess(&q, &c).signals.alias_identity);
}

/// **A franchise name is not an identity.** Two different Sword Art Online series were merged
/// through the alias `sao`; `Fairy Tail` absorbed `Fairy Tail: 100 Years Quest` through the
/// alias `fairy tail` on the sequel. A short alias that another title contains is containment.
#[test]
fn a_franchise_alias_is_containment_not_identity() {
    let sao = vec!["sao".to_owned()];
    let (reading, verdict) = adjudicate_pair(
        side("sword art online progressive", &sao),
        side("sword art online alicization", &sao),
        1.0,
        Thresholds::default(),
    );
    assert!(!reading.assessment.signals.alias_identity, "{reading:?}");
    assert_ne!(verdict.verdict, MergeVerdict::Auto);

    let franchise = vec!["fairy tail".to_owned()];
    for (left, right) in [
        (
            side("fairy tail", &[]),
            side("fairy tail 100 years quest", &franchise),
        ),
        (
            side("fairy tail 100 years quest", &franchise),
            side("fairy tail", &[]),
        ),
    ] {
        let (reading, verdict) = adjudicate_pair(left, right, 1.0, Thresholds::default());
        let signals = reading.assessment.signals;
        assert!(
            !signals.alias_identity && signals.containment,
            "{signals:?}"
        );
        assert_ne!(verdict.verdict, MergeVerdict::Auto);
    }

    // The ingest side of the same rule: the incoming listing `Fairy Tail` hits the sequel's
    // alias, which the sequel's own title contains.
    let q = query("fairy tail", ContentType::Unknown, None);
    let mut c = cand(
        1.0,
        ContentType::Unknown,
        None,
        "fairy tail 100 years quest",
    );
    c.alt_normalized_titles = vec!["fairy tail".to_owned()];
    let signals = assess(&q, &c).signals;
    assert!(
        !signals.alias_identity && signals.containment,
        "{signals:?}"
    );
}

/// A long alias contained in a longer subtitled title is the same work, not a franchise: the
/// light-novel listing and its subtitled form are one series.
#[test]
fn a_long_alias_inside_a_subtitled_title_is_still_an_alias_hit() {
    let short = "ill create the ultimate golem together with my beautiful hero disciples";
    let long = "the capitals one man army golem master ill create the ultimate golem together \
                with my beautiful hero disciples";
    let alts = vec![short.to_owned()];
    let (reading, _) = adjudicate_pair(
        side(long, &alts),
        side(short, &[]),
        0.4,
        Thresholds::default(),
    );
    assert!(reading.assessment.signals.alias_identity, "{reading:?}");
}
