//! Scoring and adjudication tests. The suite shares its `cand` and `query` fixtures across
//! modules, so it is one file, not one per module.

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
        source_titles: Vec::new(),
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

/// A disagreeing medium is recorded and costs nothing.
///
/// Providers guess the medium from the site they scraped (manga, manhwa or webtoon for one
/// work), so it cannot be trusted as a weight.
#[test]
fn content_type_disagreement_is_recorded_not_scored() {
    let q = query("title", ContentType::Manga, None);
    let same = cand(0.8, ContentType::Manga, None, "title");
    let diff = cand(0.8, ContentType::Manhwa, None, "title");
    assert!(assess(&q, &diff).signals.type_conflict);
    assert!((score(&q, &same) - score(&q, &diff)).abs() < 1e-6);
}

/// A shared author does not lift an ambiguous match.
///
/// Credits are wrong or spelled inconsistently across providers, and a credit bonus would lift a
/// discounted alias hit (0.80) back over the attach and merge thresholds. The signal is still
/// recorded and rendered.
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
    assert_eq!(
        token_set_ratio("beginning after the end", "the beginning after end"),
        1.0
    );
    // 3 shared of 4 total.
    let r = token_set_ratio("the beginning after the end", "beginning after end");
    assert!((r - 0.75).abs() < 1e-6, "got {r}");
}

/// A perfect token-set overlap lifts a weak trigram score to the top of the band, and with no
/// identity rule behind it the pair goes to the sweep, not to an attach.
///
/// A high similarity says the titles look alike, not that they name the same work:
/// `Onikakushi-hen` and `Onisarashi-hen`, or `A Year of Marvels: October` and `November`.
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
/// Both reach 1.0. Ranked on score alone, the reordered one could win the tie, and since it
/// cannot attach, a source with an exact match in hand would be queued.
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
/// Provider HTML produces the whole class: a missing space between two inline elements turns
/// `Spy X Family` into `Spyxfamily`, `Wants to Be Free` into `Wantsto Be Free` and `Hana Kimi`
/// into `Hanakimi`. Trigram similarity scores those pairs 0.37 to 0.58 and the token-set ratio
/// scores most of them zero, so without the compact comparison they would not reach the review
/// queue.
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

/// An apostrophe spelled three ways is one series, end to end.
///
/// `Sorry but I’m not Yuri` and `Sorry But Im Not Yuri` normalise to one key, so a change to
/// either the normalisation or the scorer has to keep them together.
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

/// A candidate found through one of its alternative titles is scored against that title, and
/// on its own that hit is evidence, not identity, so it lands in the review band.
///
/// Scoring only the canonical title would leave a series listed under its romaji name on one
/// provider and its english name on another split. Treating an exact alias hit as identity fails
/// the other way: aliases are scraped free text, and an attach files the source's aliases under
/// the series it joined, so one junk alias widens the net with every attach. The candidate's
/// canonical title is unrelated, so only the alias rule can produce the outcome.
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

/// An alias hit that agrees on the medium is still held for review.
///
/// Medium, year and credits are unreliable across providers, so none of them may lift an alias
/// hit to an automatic attach. The cross-provider romaji and english case is queued and settled
/// by the sweep, which can corroborate it.
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
/// This is the safety half of every other rule in this file. A more aggressive matcher scores
/// `Overlord` and `Overlord 2` higher, since the compact comparison puts them one character
/// apart, so without a veto every numbered sequel would merge into its first volume.
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
/// digits attached to different words. A per-token reading would see one title with no number
/// at all and veto the pair.
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
/// Neither `Overlord` nor `Overlord II` carries a digit, so without numerals their signatures
/// would agree.
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

/// A sequel's alias on a series does not wave the next sequel through.
///
/// With `douluo dalu 4 zhongji douluo` as an alias of `Douluo Dalu 5`, Douluo Dalu 4 still
/// conflicts with the canonical title, and that conflict stands whatever an alias says.
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
    // A digit run long enough to overflow saturates instead of panicking under the release
    // profile's overflow checks.
    assert_eq!(numeric_signature(&"9".repeat(40)), vec![u64::MAX]);
}

// --- the edit-distance window ------------------------------------------------------------

/// The edit comparison is confined to titles long enough for it to mean something.
///
/// One character in seven is 0.857, above the attach threshold, so `berserk` must not be
/// compared this way with `berserl`. The same difference in twenty is 0.95, and two distinct
/// works whose names differ by one character in twenty are rare, while providers producing
/// exactly that are routine.
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

/// An automatic, destructive merge needs a primary identity and a score, or a corroborated
/// alias. A score alone or an alias alone never merges.
///
/// This is the safety argument for automating the merge queue. A fuzzy score can reach the
/// automatic threshold on two different works with similar names, and an operator must see
/// those pairs; a primary identity means the two titles are the same string under a
/// conservative rule.
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
    // Below the review floor the pair is distinct, corroborated or not.
    assert_eq!(verdict(0.4, identical), MergeVerdict::Distinct);
    assert_eq!(verdict(0.4, by_timing), MergeVerdict::Distinct);

    // Each answer names itself, so a queue row can be triaged without re-deriving the reason
    // from a score and a list of signal names.
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

/// Each guard holds an otherwise automatic merge for review, names itself, and can be switched
/// off without changing the score or the signal that fired.
///
/// Every case below scores at the ceiling with a structural identity, the full automatic bar,
/// and is still not merged.
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

/// A numeric conflict is reported distinct, not queued.
///
/// Queueing it asks an operator to re-establish the one fact the scorer is certain of, and a
/// review queue full of sequel pairs is one nobody reads.
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
/// badges, so they are part of the contract, not debug output. New ones are appended.
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

// --- the individual scoring terms -------------------------------------------------------
//
// The tests above assert on an ordering or a band, which leaves the weights free to move. The
// tests below pin each term's exact delta, including the zero weight of the metadata terms,
// so a weight change cannot pass unnoticed.
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
/// not publish a type would otherwise conflict with every candidate, and the type guard, once
/// switched on, would hold every one of them.
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

/// The containment nudge fires in either direction and is a bonus, not a penalty.
///
/// Which side carries the fuller title depends on which site was scraped, so requiring the
/// query to be the subset would halve the cases this helps. The nudge is smaller than the
/// identity bonus because a sequel's title often contains its predecessor's.
#[test]
fn the_containment_nudge_is_symmetric_and_positive() {
    // "alpha beta" ⊂ "alpha beta gamma": two shared tokens of three, so the base score is
    // the token-set ratio, which beats the trigram similarity.
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

/// "No tags on one side" is not "no overlap", which is why this returns `Option`: a candidate
/// nobody has tagged must not score as one whose tags disagree.
///
/// [`score`] adds nothing either way, so the contract is asserted here directly.
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
/// They score `0.0`. An empty union makes the Jaccard `0 / 0`, and the `NaN` would pass
/// through every later term in [`score`] and compare false against both thresholds, leaving a
/// candidate neither attached, flagged nor rejected.
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
        source_titles: &[],
        anilist_id: None,
        content_type: ContentType::Manga,
        release_year: Some(2005),
        tags: &[],
        authors: &[],
    }
}

/// Two arcs of one work are two works.
///
/// `Higurashi no Naku Koro ni Onikakushi-hen` and `…Onisarashi-hen` share an english title and
/// differ in two letters out of forty.
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

/// `Railgun S` is not `Railgun T`: a single trailing letter names a season, even when both share
/// the english title `A Certain Scientific Railgun`.
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

/// `Tokyo Ghoul:re` is not `Tokyo Ghoul`, even when it carries the franchise name as an alias.
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

/// A junk alias is not an identity. `unknown` is placeholder text scraped from an empty metadata
/// cell, shared by every series a provider had no alternative title for.
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

/// A franchise name is not an identity. The alias `sao` names several Sword Art Online series,
/// and `fairy tail` on `Fairy Tail: 100 Years Quest` names its predecessor too. A short alias
/// that another title contains is containment.
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

// --- listing titles, external ids, live timing ------------------------------------------

/// A listing title is identity.
///
/// A mirror listing of a series `AniList` has renamed reaches it only through an alias, which is
/// discounted below the attach threshold. Matching the sibling source's listing title attaches
/// it instead of creating a duplicate for the sweep to merge.
#[test]
fn a_sibling_sources_listing_title_attaches() {
    let q = query("legend of star general", ContentType::Unknown, None);
    let mut renamed = cand(0.2, ContentType::Unknown, None, "xing jiang chuanqi");
    renamed.source_titles = vec!["legend of star general".to_owned()];
    let id = renamed.series_id;

    let signals = assess(&q, &renamed).signals;
    assert!(signals.source_title_identity, "{signals:?}");
    assert!(
        !signals.exact_title && !signals.alias_identity,
        "{signals:?}"
    );
    assert_eq!(
        decide(&q, &[renamed], Thresholds::default()),
        Decision::Attach(id)
    );

    // The same title as an alternative is still only evidence.
    let mut aliased = cand(0.2, ContentType::Unknown, None, "xing jiang chuanqi");
    aliased.alt_normalized_titles = vec!["legend of star general".to_owned()];
    assert!(!matches!(
        decide(&q, &[aliased], Thresholds::default()),
        Decision::Attach(_)
    ));
}

fn sourced<'a>(
    title: &'a str,
    sources: &'a [String],
    alts: &'a [String],
    anilist: Option<&'a str>,
) -> PairSide<'a> {
    PairSide {
        source_titles: sources,
        anilist_id: anilist,
        ..side(title, alts)
    }
}

/// In the sweep a listing title on either side reads as a primary identity and merges.
#[test]
fn a_listing_title_against_a_canonical_title_merges_in_the_sweep() {
    let listings = vec!["legend of star general".to_owned()];
    for (left, right) in [
        (
            sourced("xing jiang chuanqi", &listings, &[], None),
            sourced("legend of star general", &[], &[], None),
        ),
        (
            sourced("legend of star general", &[], &[], None),
            sourced("xing jiang chuanqi", &listings, &[], None),
        ),
    ] {
        let (reading, verdict) = adjudicate_pair(left, right, 0.1, Thresholds::default());
        assert!(
            reading.assessment.signals.source_title_identity,
            "{reading:?}"
        );
        assert!(!reading.assessment.signals.exact_title, "{reading:?}");
        assert_eq!(verdict.verdict, MergeVerdict::Auto, "{verdict:?}");
    }
}

/// An `AniList` id on both sides corroborates an alias when it agrees and holds a title match for
/// review when it does not.
#[test]
fn anilist_ids_corroborate_or_veto() {
    let alts = vec!["na honjaman level up".to_owned()];
    let agree = adjudicate_pair(
        sourced("solo leveling", &[], &alts, Some("105398")),
        sourced("na honjaman level up", &[], &[], Some("105398")),
        0.3,
        Thresholds::default(),
    );
    assert!(
        agree.0.assessment.signals.external_id_agreement,
        "{agree:?}"
    );
    assert_eq!(agree.1.verdict, MergeVerdict::Auto);
    assert_eq!(agree.1.reason, "corroborated_alias");
    assert!(
        agree
            .0
            .terms
            .iter()
            .any(|t| t.rule == "external_id_agreement"),
        "the corroboration must be in the explanation: {:?}",
        agree.0.terms
    );

    let conflict = adjudicate_pair(
        sourced("solo leveling", &[], &[], Some("105398")),
        sourced("solo leveling", &[], &[], Some("151025")),
        1.0,
        Thresholds::default(),
    );
    assert!(
        conflict.0.assessment.signals.external_id_conflict,
        "{conflict:?}"
    );
    assert_eq!(conflict.1.verdict, MergeVerdict::Review);
    assert_eq!(conflict.1.reason, "external_id_conflict");

    // One side unmapped says nothing.
    let quiet = adjudicate_pair(
        sourced("solo leveling", &[], &alts, Some("105398")),
        sourced("na honjaman level up", &[], &[], None),
        0.3,
        Thresholds::default(),
    );
    let signals = quiet.0.assessment.signals;
    assert!(!signals.external_id_agreement && !signals.external_id_conflict);
    assert_eq!(quiet.1.reason, "alias_uncorroborated");
}

/// Live timing within three days corroborates, over two weeks vetoes, and between says nothing.
#[test]
fn live_timing_corroborates_vetoes_or_abstains() {
    let alts = vec!["na honjaman level up".to_owned()];
    let read = |gap: Option<f64>| {
        let (mut reading, verdict) = adjudicate_pair(
            side("solo leveling", &alts),
            side("na honjaman level up", &[]),
            0.3,
            Thresholds::default(),
        );
        assert!(wants_live_timing(&reading, &verdict));
        apply_live_timing(
            &mut reading,
            LiveTiming {
                shared_live_chapters: 5,
                median_gap_hours: gap,
            },
        );
        adjudicate(reading.assessment, Thresholds::default())
    };
    let agree = read(Some(4.2));
    assert_eq!(agree.verdict, MergeVerdict::Auto);
    assert_eq!(agree.reason, "corroborated_alias");
    let conflict = read(Some(372.0));
    assert_eq!(conflict.verdict, MergeVerdict::Review);
    assert_eq!(conflict.reason, "live_timing_conflict");
    assert_eq!(read(Some(151.0)).reason, "alias_uncorroborated");
    assert_eq!(read(None).reason, "alias_uncorroborated");
}

// --- boundaries and branches the mutation run pins ---------------------------------------

/// The edit window includes both bounds: twelve characters is long enough, 160 is not too long.
#[test]
fn the_edit_window_includes_its_bounds() {
    let r = edit_ratio("abcdefghijkl", "abcdefghijkm");
    assert!((r - 11.0 / 12.0).abs() < 1e-6, "got {r}");

    let longest = "a".repeat(160);
    let r = edit_ratio(&longest, &format!("{}b", "a".repeat(159)));
    assert!((r - 159.0 / 160.0).abs() < 1e-6, "got {r}");
}

/// The length-gap shortcut rejects a gap over a sixth of the longer title and still runs the DP
/// for a gap of exactly a sixth.
#[test]
fn the_length_gap_bound_is_a_sixth_of_the_longer_title() {
    // 18 against 15: a gap of exactly 18 / 6, so three insertions are scored.
    let r = edit_ratio("abcdefghijklmnopqr", "abcdefghijklmno");
    assert!((r - 15.0 / 18.0).abs() < 1e-6, "got {r}");
    // 24 against 12: a gap of 12, over 24 / 6, rejected although the DP would give 0.5.
    assert_eq!(edit_ratio("abcdefghijklmnopqrstuvwx", "abcdefghijkl"), 0.0);
}

/// A deletion costs one edit, in the table's first column as well as inside it.
#[test]
fn edit_distance_charges_for_deletions() {
    assert_eq!(edit_distance("abc", ""), 3);
    assert_eq!(edit_distance("abc", "a"), 2);
    assert_eq!(edit_distance("sitting", "kitten"), 3);
}

/// The name-set overlap is the shared names over the union, ignoring case.
#[test]
fn name_set_overlap_is_intersection_over_union() {
    let a = ["Action".to_owned(), "Fantasy".to_owned()];
    let b = ["action".to_owned(), "Romance".to_owned()];
    assert_eq!(name_set_overlap(&a, &b), Some(1.0 / 3.0));
}

/// A pair that agrees on nothing is still explained against the candidate's canonical title,
/// not an empty string.
#[test]
fn an_unmatched_pair_names_the_canonical_title() {
    let q = query("berserk", ContentType::Unknown, None);
    let c = cand(0.1, ContentType::Unknown, None, "solo leveling");
    assert_eq!(explain(&q, &c).matched_candidate_title, "solo leveling");
}

/// A canonical title wins a tie against an alternative title, so a reordered alias cannot turn
/// an exact hit into a discounted alias hit.
#[test]
fn the_canonical_title_wins_a_tie_with_an_alias() {
    let q = query("solo leveling", ContentType::Unknown, None);
    let mut c = cand(0.1, ContentType::Unknown, None, "solo leveling");
    c.alt_normalized_titles = vec!["leveling solo".to_owned()];
    let explained = explain(&q, &c);
    assert!(!explained.via_alias, "{explained:?}");
    assert!(explained.assessment.signals.exact_title, "{explained:?}");
}

/// A title one space from the other earns the identity bonus even when it is too short for the
/// edit comparison and shares no word with it.
#[test]
fn a_short_title_a_space_apart_earns_the_identity_bonus() {
    let q = query("one piece", ContentType::Unknown, None);
    let c = cand(0.0, ContentType::Unknown, None, "onepiece");
    let explained = explain(&q, &c);
    assert!(
        explained.assessment.signals.compact_identity,
        "{explained:?}"
    );
    assert!(
        explained
            .terms
            .iter()
            .any(|t| t.rule == "title_identity" && (t.delta - 0.1).abs() < 1e-6),
        "{:?}",
        explained.terms
    );
}

/// A listing title one space from the query is a source identity, not only an exact one.
#[test]
fn a_listing_title_a_space_apart_is_a_source_identity() {
    let q = query("solo leveling", ContentType::Unknown, None);
    let mut c = cand(0.1, ContentType::Unknown, None, "na honjaman level up");
    c.source_titles = vec!["sololeveling".to_owned()];
    let signals = assess(&q, &c).signals;
    assert!(signals.source_title_identity, "{signals:?}");
}

/// `near_identical` is a title an edit or two from the other: never an identity, never two
/// unrelated titles.
#[test]
fn near_identical_is_close_but_not_identical() {
    let near = |a: &str, b: &str| {
        assess(
            &query(a, ContentType::Unknown, None),
            &cand(0.1, ContentType::Unknown, None, b),
        )
        .signals
        .near_identical
    };
    assert!(near(
        "the beginning after the end",
        "the beginning after the ent"
    ));
    assert!(!near("solo leveling", "solo leveling"));
    assert!(!near("solo leveling", "sololeveling"));
    assert!(!near("solo leveling", "berserk"));
}

/// Containment is the scorer's word-subset finding or a franchise alias hit; an alias hit that
/// is no franchise name is not containment.
#[test]
fn containment_is_a_word_subset_or_a_franchise_alias() {
    let q = query("solo leveling", ContentType::Unknown, None);
    let c = cand(0.1, ContentType::Unknown, None, "solo leveling ragnarok");
    assert!(assess(&q, &c).signals.containment);

    let q = query("berserk", ContentType::Unknown, None);
    let mut c = cand(0.1, ContentType::Unknown, None, "kenpuu denki");
    c.alt_normalized_titles = vec!["berserk".to_owned()];
    assert!(!assess(&q, &c).signals.containment);
}

/// Credits on one side only are missing data, not an author conflict.
#[test]
fn credits_on_one_side_are_not_an_author_conflict() {
    let mut q = query("solo leveling", ContentType::Unknown, None);
    q.authors = vec!["Chugong".to_owned()];
    let c = cand(0.9, ContentType::Unknown, None, "solo leveling");
    assert!(!assess(&q, &c).signals.author_conflict);

    let mut credited = c;
    credited.authors = vec!["Chugong".to_owned()];
    let uncredited = query("solo leveling", ContentType::Unknown, None);
    assert!(!assess(&uncredited, &credited).signals.author_conflict);
}

/// The numeric-conflict term is explained as the penalty it applies, a negative delta.
#[test]
fn the_numeric_conflict_term_is_a_penalty() {
    let q = query("overlord 2", ContentType::Unknown, None);
    let c = cand(0.9, ContentType::Unknown, None, "overlord");
    let explained = explain(&q, &c);
    let term = explained
        .terms
        .iter()
        .find(|t| t.rule == "numeric_conflict")
        .expect("a numeric conflict term");
    assert!((term.delta + 0.3).abs() < 1e-6, "{term:?}");
}

/// The review floor and the automatic-merge threshold are both inclusive.
#[test]
fn adjudication_thresholds_are_inclusive() {
    let t = Thresholds::default();
    let at_floor = adjudicate(
        Assessment {
            score: t.low,
            signals: MatchSignals::default(),
        },
        t,
    );
    assert_eq!(at_floor.verdict, MergeVerdict::Review);
    assert_eq!(at_floor.reason, "no_structural_identity");

    let identical = MatchSignals {
        exact_title: true,
        ..MatchSignals::default()
    };
    let at_auto = adjudicate(
        Assessment {
            score: t.auto_merge,
            signals: identical,
        },
        t,
    );
    assert_eq!(at_auto.verdict, MergeVerdict::Auto);
}

/// An edition conflict refuses an attach that a listing title would otherwise earn.
///
/// The listing `tokyo ghoul re` filed under the series `tokyo ghoul` is an identity on the
/// listing and a different edition from the canonical title.
#[test]
fn an_edition_conflict_refuses_an_attach_on_a_listing_title() {
    let q = query("tokyo ghoul re", ContentType::Unknown, None);
    let mut c = cand(1.0, ContentType::Unknown, None, "tokyo ghoul");
    c.source_titles = vec!["tokyo ghoul re".to_owned()];
    let signals = assess(&q, &c).signals;
    assert!(
        signals.source_title_identity && signals.edition_conflict,
        "{signals:?}"
    );
    assert!(matches!(
        decide(&q, &[c], Thresholds::default()),
        Decision::Ambiguous { .. }
    ));
}

/// Each veto refuses an attach on its own, including the two `assess` never sets today.
#[test]
fn every_veto_signal_vetoes_alone() {
    for signals in [
        MatchSignals {
            numeric_conflict: true,
            ..MatchSignals::default()
        },
        MatchSignals {
            edition_conflict: true,
            ..MatchSignals::default()
        },
        MatchSignals {
            external_id_conflict: true,
            ..MatchSignals::default()
        },
        MatchSignals {
            live_timing_conflict: true,
            ..MatchSignals::default()
        },
    ] {
        assert!(crate::decide::has_veto(signals), "{signals:?}");
    }
    assert!(!crate::decide::has_veto(MatchSignals::default()));
}

/// Readings tied on class and score keep the first: the left side's canonical title.
#[test]
fn tied_readings_keep_the_left_canonical_reading() {
    let alts = vec!["kenpuu denki".to_owned()];
    // A trigram of 1.0 lifts every reading to the clamp ceiling with no identity behind any.
    let reading = read_pair(side("berserk", &alts), side("vagabond", &[]), 1.0);
    assert!(reading.query_is_left, "{reading:?}");
    assert_eq!(reading.matched_query_title, "berserk");
}

/// A conflict only the pair rules find is explained once, as a zero-delta term, and one the
/// scorer already explained is not explained twice.
#[test]
fn pair_rule_conflicts_are_explained_exactly_once() {
    let count =
        |reading: &PairReading, rule: &str| reading.terms.iter().filter(|t| t.rule == rule).count();

    // The alias `dark lord` matches exactly; only the canonical titles carry different numbers.
    let alias = vec!["dark lord".to_owned()];
    let reading = read_pair(side("dark lord 2", &alias), side("dark lord", &[]), 0.3);
    assert_eq!(reading.matched_query_title, "dark lord", "{reading:?}");
    assert!(reading.assessment.signals.numeric_conflict);
    assert_eq!(
        count(&reading, "numeric_conflict"),
        1,
        "{:?}",
        reading.terms
    );

    let reading = read_pair(side("overlord", &[]), side("overlord 2", &[]), 0.9);
    assert_eq!(
        count(&reading, "numeric_conflict"),
        1,
        "{:?}",
        reading.terms
    );

    let alias = vec!["tokyo ghoul".to_owned()];
    let reading = read_pair(
        side("tokyo ghoul re", &alias),
        side("tokyo ghoul", &[]),
        0.3,
    );
    assert_eq!(reading.matched_query_title, "tokyo ghoul", "{reading:?}");
    assert!(reading.assessment.signals.edition_conflict);
    assert_eq!(
        count(&reading, "edition_conflict"),
        1,
        "{:?}",
        reading.terms
    );

    let reading = read_pair(side("tokyo ghoul re", &[]), side("tokyo ghoul", &[]), 0.3);
    assert_eq!(
        count(&reading, "edition_conflict"),
        1,
        "{:?}",
        reading.terms
    );
}

/// Live timing is wanted for an alias hit, even one already merging, and for a pair going to
/// review, and for nothing else.
#[test]
fn live_timing_is_wanted_for_an_alias_or_a_review() {
    let alts = vec!["na honjaman level up".to_owned()];
    let (reading, verdict) = adjudicate_pair(
        sourced("solo leveling", &[], &alts, Some("105398")),
        sourced("na honjaman level up", &[], &[], Some("105398")),
        0.3,
        Thresholds::default(),
    );
    assert_eq!(verdict.verdict, MergeVerdict::Auto);
    assert!(wants_live_timing(&reading, &verdict));

    let (reading, verdict) = adjudicate_pair(
        side("the beginning after the end", &[]),
        side("the beginning after the ent", &[]),
        0.9,
        Thresholds::default(),
    );
    assert_eq!(verdict.verdict, MergeVerdict::Review);
    assert!(wants_live_timing(&reading, &verdict));

    let (reading, verdict) = adjudicate_pair(
        side("solo leveling", &[]),
        side("solo leveling", &[]),
        1.0,
        Thresholds::default(),
    );
    assert_eq!(verdict.verdict, MergeVerdict::Auto);
    assert!(!wants_live_timing(&reading, &verdict));
}

/// Both live-timing bounds belong to the abstaining side: 72 hours still agrees, and exactly two
/// weeks is not yet a conflict.
#[test]
fn live_timing_bounds_are_exact() {
    let alts = vec!["na honjaman level up".to_owned()];
    let read = |gap: f64| {
        let mut reading = read_pair(
            side("solo leveling", &alts),
            side("na honjaman level up", &[]),
            0.3,
        );
        let before = reading.terms.len();
        apply_live_timing(
            &mut reading,
            LiveTiming {
                shared_live_chapters: 5,
                median_gap_hours: Some(gap),
            },
        );
        (reading.assessment.signals, reading.terms.len() - before)
    };
    let (signals, added) = read(LIVE_AGREEMENT_HOURS);
    assert!(signals.live_timing_agreement && added == 1, "{signals:?}");
    let (signals, added) = read(LIVE_CONFLICT_HOURS);
    assert!(
        !signals.live_timing_agreement && !signals.live_timing_conflict && added == 0,
        "{signals:?}"
    );
}

/// A query-side listing title one space from the other side's canonical title is relabelled a
/// source identity, not reported as the two canonical titles being compact-identical.
#[test]
fn a_compact_hit_through_a_listing_title_is_a_source_identity() {
    let listings = vec!["sololeveling".to_owned()];
    let reading = read_pair(
        sourced("na honjaman level up", &listings, &[], None),
        side("solo leveling", &[]),
        0.1,
    );
    let signals = reading.assessment.signals;
    assert!(
        signals.source_title_identity && !signals.compact_identity,
        "{signals:?}"
    );
}

/// A query-side alias or listing title that matches nothing claims no identity.
#[test]
fn an_unmatched_query_side_title_claims_nothing() {
    let alts = vec!["na honjaman level up".to_owned()];
    let reading = read_pair(side("solo leveling", &alts), side("vagabond", &[]), 0.2);
    assert!(!reading.assessment.signals.alias_identity, "{reading:?}");

    let reading = read_pair(
        sourced("solo leveling", &alts, &[], None),
        side("vagabond", &[]),
        0.2,
    );
    assert!(
        !reading.assessment.signals.source_title_identity,
        "{reading:?}"
    );
}

/// The pair rules add containment and conflicts to a reading, never clear the scorer's.
#[test]
fn pair_rules_never_clear_what_the_scorer_found() {
    // The exact alias hit is containment by the scorer's word-subset rule, and no franchise name.
    let alts = vec!["na honjaman level up".to_owned()];
    let reading = read_pair(
        side("solo leveling", &alts),
        side("na honjaman level up", &[]),
        0.3,
    );
    assert!(reading.assessment.signals.alias_identity, "{reading:?}");
    assert!(reading.assessment.signals.containment, "{reading:?}");

    // The alias names a side story; the canonical titles carry no edition marker.
    let alts = vec!["demon slayer gaiden".to_owned()];
    let (reading, verdict) = adjudicate_pair(
        side("kimetsu no yaiba", &alts),
        side("demon slayer", &[]),
        0.3,
        Thresholds::default(),
    );
    assert_eq!(
        reading.matched_query_title, "demon slayer gaiden",
        "{reading:?}"
    );
    assert!(reading.assessment.signals.edition_conflict, "{reading:?}");
    assert_eq!(verdict.reason, "edition_conflict");
}

/// A reading's claim outranks its score: an alias hit beats a reading the trigram alone lifts to
/// the ceiling.
#[test]
fn the_strongest_claim_wins_over_the_highest_score() {
    let alts = vec!["solo leveling".to_owned()];
    let reading = read_pair(
        side("solo leveling", &[]),
        side("na honjaman level up", &alts),
        1.0,
    );
    assert!(reading.assessment.signals.alias_identity, "{reading:?}");
    assert_eq!(reading.matched_query_title, "solo leveling");
}
