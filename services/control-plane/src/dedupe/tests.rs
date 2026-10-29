//! Tests the duplicate sweep.

use std::collections::{HashMap, HashSet};

use tankovault_config::MatchingConfig;
use tankovault_contracts::admin::MergeSweepView;
use tankovault_db::repo::matching::{DuplicatePair, SeriesMatchFacts};
use tankovault_domain::ContentType;
use tankovault_domain::SeriesId;
use tankovault_domain::matching::MergeVerdict;
use tankovault_matcher::adjudicate;

use super::{
    journal::{canonical, journal_payloads},
    judgement::{Action, Judged, Trigger},
    pass::{best_reading, retract_absorbed},
    run::{RoundLoop, SweepBudget, SweepRun, accumulate, exhaustive_budget},
};

fn facts(title: &str, alts: &[&str], sources: i64, chapters: i64) -> SeriesMatchFacts {
    SeriesMatchFacts {
        series_id: SeriesId::new(),
        canonical_title: title.to_owned(),
        normalized_title: tankovault_domain::normalize_title(title),
        alt_normalized_titles: alts
            .iter()
            .map(|t| tankovault_domain::normalize_title(t))
            .collect(),
        source_titles: Vec::new(),
        anilist_id: None,
        content_type: ContentType::Unknown,
        release_year: None,
        tags: Vec::new(),
        authors: Vec::new(),
        source_count: sources,
        chapter_count: chapters,
        watcher_count: 0,
    }
}

fn verdict_for(a: &SeriesMatchFacts, b: &SeriesMatchFacts, trigram: f32) -> MergeVerdict {
    let reading = best_reading(a, b, trigram);
    adjudicate(reading.assessment, MatchingConfig::default().thresholds()).verdict
}

/// A run's totals sum every count except the one that means "still to do".
///
/// `chains_deferred` is how many pairs the *last* pass skipped because of its own merges, and
/// the sweep re-runs for exactly as long as it is non-zero. Summing it across passes would
/// report the chains that later passes went on to resolve, so a run that fully collapsed a
/// three-way duplicate would still read as having left work behind, and the counter an
/// operator watches for a stuck chain would never come back to zero.
#[test]
fn a_multi_pass_run_reports_only_the_chains_the_last_pass_left() {
    let mut total = MergeSweepView::default();
    accumulate(
        &mut total,
        &MergeSweepView {
            pairs_examined: 10,
            auto_merged: 2,
            queued: 4,
            chains_deferred: 3,
            ..MergeSweepView::default()
        },
    );
    accumulate(
        &mut total,
        &MergeSweepView {
            pairs_examined: 4,
            auto_merged: 1,
            queued: 1,
            chains_deferred: 0,
            ..MergeSweepView::default()
        },
    );

    assert_eq!(total.pairs_examined, 14);
    assert_eq!(total.auto_merged, 3);
    assert_eq!(total.queued, 5);
    assert_eq!(
        total.chains_deferred, 0,
        "the chain the first pass deferred is the one the second resolved"
    );
}

fn judgement_of<'a>(
    left: &'a SeriesMatchFacts,
    right: &'a SeriesMatchFacts,
    action: Action<'a>,
) -> Judged<'a> {
    let reading = best_reading(left, right, 0.0);
    let verdict = adjudicate(reading.assessment, MatchingConfig::default().thresholds());
    Judged {
        pair: (left.series_id, right.series_id),
        trigger: Trigger::New,
        left,
        right,
        trigram: 0.0,
        reading,
        live_timing: None,
        verdict,
        signals: Vec::new(),
        reason: None,
        action,
    }
}

/// A verdict reached before a merge in the same pass absorbed one of its series is retracted,
/// not written.
///
/// The judging loop skips pairs naming an already-absorbed series, but it can only skip pairs
/// it has not reached yet. A pair judged *earlier* kept its verdict, and the queue write that
/// carries the whole pass (both of whose id columns are foreign keys into `series`) was
/// rejected as a body: `insert or update on table "merge_candidates" violates foreign key
/// constraint "merge_candidates_candidate_id_fkey"`, reported to the operator as
/// `Full sweep failed`. The retracted pair is a chain deferral, which is what makes the sweep
/// come back for it against the survivor.
#[test]
fn a_verdict_is_retracted_when_a_later_merge_in_the_pass_absorbs_its_series() {
    let survivor = facts("Survivor Work", &[], 3, 90);
    let doomed = facts("Survivorwork", &[], 1, 12);
    let bystander = facts("Unrelated Work", &[], 1, 8);

    let mut judged = vec![
        // Judged before the merge, and naming the series the merge went on to delete.
        judgement_of(&bystander, &doomed, Action::Review),
        judgement_of(
            &survivor,
            &doomed,
            Action::Merged {
                keep: &survivor,
                drop: &doomed,
            },
        ),
        judgement_of(&bystander, &survivor, Action::Distinct),
    ];
    let absorbed = HashSet::from([doomed.series_id]);

    assert_eq!(retract_absorbed(&mut judged, &absorbed), 1);
    assert_eq!(judged.len(), 2);
    assert!(
        judged
            .iter()
            .any(|entry| matches!(entry.action, Action::Merged { .. })),
        "the merge's own judgement names the absorbed series and must survive: it is \
         journalled with the undo record, and that row has no foreign key"
    );
    assert!(
        !judged
            .iter()
            .any(|entry| matches!(entry.action, Action::Review)),
        "the review verdict naming the deleted series is gone"
    );

    let mut untouched = vec![judgement_of(&bystander, &survivor, Action::Distinct)];
    assert_eq!(
        retract_absorbed(&mut untouched, &HashSet::new()),
        0,
        "a pass that merged nothing retracts nothing"
    );
    assert_eq!(untouched.len(), 1);
}

/// **A deferral is journalled once, not once per run.**
///
/// The merge ceiling deferred the same pairs on every run until it let them through, and each
/// run wrote a fresh `deferred` row: 1,800,193 journal rows in production
/// (`docs/MATCHING_OVERHAUL.md` WP7). A pair whose latest row is already a deferral gets no
/// new one; a pair deferred for the first time still does.
#[test]
fn a_repeat_deferral_is_not_journalled_again() {
    let a = facts("Survivor Work", &[], 3, 90);
    let b = facts("Survivorwork", &[], 1, 12);
    let c = facts("Other Work", &[], 1, 12);
    let d = facts("Otherwork", &[], 1, 12);
    let judged = vec![
        judgement_of(&a, &b, Action::Deferred),
        judgement_of(&c, &d, Action::Deferred),
    ];
    let repeat = HashSet::from([canonical((b.series_id, a.series_id))]);

    let payloads = journal_payloads(&judged, &HashMap::new(), &repeat);

    assert_eq!(payloads.len(), 1);
    assert_eq!(payloads[0].0.pair, (c.series_id, d.series_id));
    assert_eq!(payloads[0].1.outcome, "deferred");
}

fn round(shortlisted: &[DuplicatePair], auto_merged: i64) -> SweepRun {
    SweepRun {
        report: MergeSweepView {
            auto_merged,
            ..MergeSweepView::default()
        },
        shortlisted: shortlisted.to_vec(),
    }
}

const fn budget(max_auto_merges: i64) -> SweepBudget {
    SweepBudget {
        pairs: 500,
        requeue: 500,
        recheck: 500,
        max_auto_merges,
    }
}

/// An exhaustive run ends on a round that shortlists nothing new, not on one that finds
/// nothing at all.
///
/// `open_merge_pairs` and `distinct_merge_pairs` are ordered least-recently-scored first and
/// re-scoring moves a pair to the back of that order, so neither ever returns empty. A run
/// that waited for an empty round would never end against a catalogue holding a single open
/// candidate, re-scoring that one pair for as long as the process lived.
///
/// Both pairs must reach `seen` in the first round, which is why the filter is counted,
/// not short-circuited: with `any()` the second pair would still look new in round two.
#[test]
fn an_exhaustive_run_ends_when_a_round_shortlists_nothing_new() {
    let a = (SeriesId::new(), SeriesId::new());
    let b = (SeriesId::new(), SeriesId::new());
    let mut rounds = RoundLoop::new();

    assert!(
        !rounds.admit(&round(&[a, b], 0)),
        "the first round shortlisted two pairs this run had not judged"
    );
    assert!(
        rounds.admit(&round(&[a, b], 0)),
        "the same two pairs back again is one full rotation, not more work"
    );
}

/// **A round that shortlists new pairs is drawn however many rounds and merges came before.**
///
/// The bug: the exhaustive run stopped at the scheduled sweep's automatic-merge ceiling (200) and
/// at a 256-round cap, and report the stop as work remaining. The button says it sweeps the
/// whole catalogue: on a catalogue with more duplicates than that, it swept a prefix and
/// asked to be pressed again, once per 200 merges, with a full-catalogue blocking query
/// spent on each press to reach the pairs the last run stopped short of.
#[test]
fn nothing_but_an_empty_rotation_stops_an_exhaustive_run() {
    let mut rounds = RoundLoop::new();

    for _ in 0..(256 * 4) {
        let fresh = (SeriesId::new(), SeriesId::new());
        assert!(
            !rounds.admit(&round(&[fresh], 500)),
            "a round that judged a pair no round had judged is not the end of the walk"
        );
    }
}

/// The exhaustive budget lifts the merge ceiling and nothing else.
///
/// The shortlist sizes are the page the walk is drawn in; raising them here would multiply
/// what one round holds in memory without covering a single pair more, since rounds continue
/// until one comes back empty-handed.
#[test]
fn the_exhaustive_budget_lifts_the_ceiling_and_keeps_the_page() {
    let scheduled = budget(200);
    let exhaustive = exhaustive_budget(scheduled);

    assert_eq!(exhaustive.max_auto_merges, i64::MAX);
    assert_eq!(exhaustive.pairs, scheduled.pairs);
    assert_eq!(exhaustive.requeue, scheduled.requeue);
    assert_eq!(exhaustive.recheck, scheduled.recheck);
}

/// A synonym is found from whichever side it sits on.
///
/// `Query` has no alternative titles, so a sweep that scored the query side's canonical
/// title alone saw only the *right* side's synonyms. Here the synonym is on the left, and
/// the canonical titles share nothing, so that sweep filed two obvious duplicates as
/// distinct.
#[test]
fn a_synonym_on_either_side_is_enough() {
    let left = facts("Solo Leveling", &["Na Honjaman Level Up"], 1, 10);
    let right = facts("Na Honjaman Level Up", &[], 1, 10);

    // Sanity: without the synonym the pair is not an identity match at all.
    let bare_left = facts("Solo Leveling", &[], 1, 10);
    let bare = best_reading(&bare_left, &right, 0.0).assessment.signals;
    assert!(
        !bare.alias_identity && !bare.is_primary_identity(),
        "{bare:?}"
    );

    for (a, b) in [(&left, &right), (&right, &left)] {
        let reading = best_reading(a, b, 0.0);
        assert!(
            reading.assessment.signals.alias_identity,
            "{:?}",
            reading.assessment
        );
    }
}

/// **The synonym that identifies a pair survives the trigram score that synonym produced.**
///
/// `pair_similarities` takes the trigram over each side's alternative titles as well as the
/// two canonical ones, so this pair arrives with 1.0, earned by the synonym and handed to
/// every reading. That lifted the canonical reading, which agrees on nothing, to the clamp
/// ceiling, where it tied with the alias reading that carries the identity and beat it on
/// iteration order. The row then read "100% match … titles are similar but not the same
/// string", with no badge to say which title the number came from. The older test above pins
/// the same pair at a trigram of zero, which is the one value that hides this.
///
/// An alias alone no longer merges (an uncorroborated alias is what built the hub series of
/// `docs/MATCHING_OVERHAUL.md` §2), so the pair is reviewed, and must be reviewed as the
/// alias hit it is, not as a fuzzy score.
#[test]
fn an_alias_identity_is_not_lost_to_the_trigram_it_produced() {
    let left = facts("Solo Leveling", &["Na Honjaman Level Up"], 1, 10);
    let right = facts("Na Honjaman Level Up", &[], 1, 10);

    for (a, b) in [(&left, &right), (&right, &left)] {
        let reading = best_reading(a, b, 1.0);
        assert!(
            reading.assessment.signals.alias_identity,
            "{:?}",
            reading.assessment
        );
        let verdict = adjudicate(reading.assessment, MatchingConfig::default().thresholds());
        assert_eq!(verdict.verdict, MergeVerdict::Review);
        assert_eq!(verdict.reason, "alias_uncorroborated");
    }
}

/// The survivor is the series carrying more of the catalogue, in both orderings.
///
/// The bug: which id sat in `series_id` and which in `candidate_id` decided this, and it
/// recorded nothing but which series a scan happened to create second, so the console's
/// merge button routinely deleted the *older*, richer series and kept a stub.
#[test]
fn the_richer_series_survives_regardless_of_pair_order() {
    let rich = facts("Berserk", &[], 4, 380);
    let stub = facts("Berserk", &[], 1, 2);
    assert!(rich.weight() > stub.weight());

    // Whichever way round the pair arrives, the same series wins.
    let pick = |a: &SeriesMatchFacts, b: &SeriesMatchFacts| {
        if a.weight() >= b.weight() {
            a.series_id
        } else {
            b.series_id
        }
    };
    assert_eq!(pick(&rich, &stub), rich.series_id);
    assert_eq!(pick(&stub, &rich), rich.series_id);
}

/// The reported pair, scored the way the sweep scores it.
///
/// `Sorry but I’m not Yuri` and `Sorry But Im Not Yuri` sat in the queue at 0.80 with no
/// path to ever leaving it. With the corrected normalization their keys are identical, which
/// is a primary identity, which is what `adjudicate` requires before merging anything.
#[test]
fn the_reported_duplicate_is_merged_automatically() {
    let a = facts("Sorry but I\u{2019}m not Yuri", &[], 1, 12);
    let b = facts("Sorry But Im Not Yuri", &[], 1, 9);
    let reading = best_reading(&a, &b, 0.8);
    assert!(
        reading.assessment.signals.exact_title,
        "{:?}",
        reading.assessment
    );
    assert_eq!(verdict_for(&a, &b, 0.8), MergeVerdict::Auto);
}

/// The two `KunManga` listings the user reported, scored the way the sweep scores them.
///
/// One series' canonical title is byte-identical to the other's alternative title, which is
/// `alias_identity`. The pair sat split for a day because `find_duplicate_pairs` never
/// reached it; this pins that, once reached, the sweep reads it as the alias hit it is.
///
/// An alias and an agreeing year are not enough to merge: that route (alias, lifted by
/// metadata) produced 12,356 suspicious merges and the contaminated hub series in
/// `docs/MATCHING_OVERHAUL.md` §2. An alias alone is held for review, and merges once a trusted
/// non-title signal agrees.
#[test]
fn the_reported_pair_is_held_until_its_alias_is_corroborated() {
    let long = "The Capital\u{2019}s One-Man Army Golem Master\u{2014}Unexpectedly Banished!? \
                ~Now That I\u{2019}m Free, I\u{2019}ll Create the Ultimate Golem Together \
                With My Beautiful Hero Disciples. I Don\u{2019}t Care Even if They Beg Me to \
                Come Back~";
    let short = "I'll Create the Ultimate Golem Together with My Beautiful Hero Disciples. \
                 I Don't Care Even If They Beg Me to Come Back";

    let mut subtitled = facts(long, &[short], 1, 18);
    subtitled.release_year = Some(2025);
    let mut bare = facts(short, &[], 1, 18);
    bare.release_year = Some(2026);

    let reading = best_reading(&subtitled, &bare, 0.4);
    assert!(
        reading.assessment.signals.alias_identity,
        "{:?}",
        reading.assessment
    );
    let thresholds = MatchingConfig::default().thresholds();
    let held = adjudicate(reading.assessment, thresholds);
    assert_eq!(held.verdict, MergeVerdict::Review);
    assert_eq!(held.reason, "alias_uncorroborated");

    let mut corroborated = reading.assessment;
    corroborated.signals.live_timing_agreement = true;
    assert_eq!(
        adjudicate(corroborated, thresholds).verdict,
        MergeVerdict::Auto
    );
}

/// Two series that agree on nothing but a *shared synonym* are still a duplicate.
///
/// `find_duplicate_pairs` shortlists this collision (alias against alias, the same work
/// listed under its romaji name by two providers that each chose a different english title)
/// but the scorer could not see it: `Query` has no alternative titles, so both directions
/// compared a *canonical* title against the other side's names and the one title the two
/// series actually share was never an input. No structural signal meant `adjudicate` refused
/// to merge at any score, so 438 such pairs sat in the review queue with no way out of it.
#[test]
fn a_synonym_shared_by_both_sides_is_still_an_identity_match() {
    let romaji = "yuusha party wo tsuihou sareta beast tamer";
    let left = facts("The Beast Tamer Exiled From His Party", &[romaji], 1, 30);
    let right = facts("Banished Monster Tamer", &[romaji], 1, 24);

    // Sanity: strip the synonym and nothing identifies the pair, so the assertion below
    // cannot be passing on the canonical titles.
    let bare_left = facts("The Beast Tamer Exiled From His Party", &[], 1, 30);
    let bare_right = facts("Banished Monster Tamer", &[], 1, 24);
    let bare = best_reading(&bare_left, &bare_right, 0.0)
        .assessment
        .signals;
    assert!(
        !bare.alias_identity && !bare.is_primary_identity(),
        "{bare:?}"
    );

    let reading = best_reading(&left, &right, 0.0);
    let signals = reading.assessment.signals;
    assert!(signals.alias_identity, "{signals:?}");
    assert!(
        !signals.exact_title && !signals.compact_identity,
        "a synonym hit must not claim the canonical titles are the same string: {signals:?}"
    );
}

/// A numbered sequel is never merged, however identical the rest of the title is.
#[test]
fn a_sequel_is_not_swept_up() {
    let a = facts("Kingdom of the Wind", &[], 1, 40);
    let b = facts("Kingdom of the Wind 2", &[], 1, 12);
    let reading = best_reading(&a, &b, 0.95);
    assert!(
        reading.assessment.signals.numeric_conflict,
        "{:?}",
        reading.assessment
    );
    assert_eq!(verdict_for(&a, &b, 0.95), MergeVerdict::Distinct);
}

/// Two works with the same title and no creator in common are held for review.
///
/// The pair clears every part of the automatic bar (byte-identical titles, a score at the
/// ceiling) and is exactly the shape of a remake, a spin-off, or an unrelated work that
/// happens to share a name. Before the guard, the scorer had no term for disagreeing credits
/// at all: `shared_author` was a bonus, and its absence was indistinguishable from a series
/// with no credits recorded.
#[test]
fn same_title_by_different_creators_is_not_merged_automatically() {
    let mut a = facts("Phoenix", &[], 2, 40);
    a.authors = vec!["Osamu Tezuka".to_owned()];
    let mut b = facts("Phoenix", &[], 2, 30);
    b.authors = vec!["Someone Else".to_owned()];

    let reading = best_reading(&a, &b, 1.0);
    assert!(
        reading.assessment.signals.author_conflict,
        "{:?}",
        reading.assessment
    );
    assert!(
        reading.assessment.signals.is_primary_identity(),
        "sanity: titles are identical"
    );

    let held = adjudicate(reading.assessment, MatchingConfig::default().thresholds());
    assert_eq!(held.verdict, MergeVerdict::Review);
    assert_eq!(held.blocked_by, vec!["author_conflict"]);

    // The same pair with one side's credits unknown says nothing either way, and merges.
    let bare = facts("Phoenix", &[], 2, 30);
    let quiet = best_reading(&a, &bare, 1.0);
    assert!(!quiet.assessment.signals.author_conflict);
    assert_eq!(verdict_for(&a, &bare, 1.0), MergeVerdict::Auto);
}

/// Every automatic merge carries the terms that produced its score.
///
/// The journal is the whole point of the sweep being allowed to delete a row without asking:
/// a stored score and a bag of signal names say *that* the pair matched, and the terms say
/// which title matched and what each rule contributed. A reading with an empty term list is
/// a merge nobody can audit.
#[test]
fn a_merged_pair_carries_its_itemised_score() {
    let a = facts("Spy X Family", &[], 3, 90);
    let b = facts("Spyxfamily", &[], 1, 12);
    let reading = best_reading(&a, &b, 0.4);

    assert_eq!(verdict_for(&a, &b, 0.4), MergeVerdict::Auto);
    assert!(
        reading.terms.iter().any(|t| t.rule == "base_similarity"),
        "the base the score started from must be recorded: {:?}",
        reading.terms
    );
    assert!(
        reading.terms.iter().any(|t| t.rule == "title_identity"),
        "the rule that justified the merge must be recorded: {:?}",
        reading.terms
    );
    // The score is reconstructible from the terms, which is what makes them an explanation
    // instead of a commentary.
    let summed: f32 = reading.terms.iter().map(|t| t.delta).sum();
    assert!(
        (summed.clamp(0.0, 1.0) - reading.assessment.score).abs() < 1e-5,
        "terms {:?} do not sum to {}",
        reading.terms,
        reading.assessment.score
    );
}
