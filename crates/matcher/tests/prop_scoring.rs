//! Algebraic properties of the canonicalisation scorer.
//!
//! `score`/`decide` are the whole of series canonicalisation: their output decides whether a
//! newly-crawled source is attached to an existing series, queued for human review, or forked
//! into a new canonical work. A wrong answer is not a visible error — it is a duplicated or
//! wrongly-merged series that nobody notices for months.
//!
//! The in-module tests pin a handful of hand-picked scenarios. These pin the *algebra*: the
//! bounds, symmetry and monotonicity that every one of those scenarios silently assumes and
//! that a future tweak to a bonus weight could break without failing a single example test.
//!
//! # A note on the release-year range
//!
//! The generated years below are deliberately bounded rather than `any::<i32>()`, so that the
//! properties are asserted over years a provider could plausibly print. The extremes are not
//! skipped, though — they used to *panic* (`(a - b).abs()` overflowing `i32`) and are now
//! pinned by name in
//! [`score_survives_the_extremes_of_the_release_year_range`].

// Symmetry and reflexivity are claims of *bit-exact* equality — `token_set_ratio(a, b)` and
// `token_set_ratio(b, a)` compute the same expression over the same set sizes, so an epsilon
// here would weaken the property into something that no longer catches an argument-order bug.
// The same allowance is already made by the in-module tests for the same reason.
#![expect(
    clippy::float_cmp,
    reason = "the properties under test are exact ones - symmetry and reflexivity of the \
              score - which a tolerance would weaken into something else"
)]

use proptest::prelude::*;
use tankovault_domain::{ContentType, SeriesId};
use tankovault_matcher::{Candidate, Decision, Query, Thresholds, best_match, decide, score};

/// Titles as the normalizer produces them: lowercase alphanumeric words, single-spaced.
/// Generating raw `".*"` would mostly produce strings that `normalize_title` could never
/// emit, which tests the wrong function.
fn normalized_title() -> impl Strategy<Value = String> {
    prop::collection::vec("[a-z0-9]{1,8}", 0..5).prop_map(|words| words.join(" "))
}

/// Years wide enough to include anything a provider could plausibly print, and narrow enough
/// that `(a - b)` cannot overflow `i32`. See the module note.
fn safe_year() -> impl Strategy<Value = Option<i32>> {
    prop::option::of(-100_000i32..=100_000i32)
}

fn content_type() -> impl Strategy<Value = ContentType> {
    prop::sample::select(ContentType::all().to_vec())
}

fn names() -> impl Strategy<Value = Vec<String>> {
    prop::collection::vec("[a-z ]{1,10}", 0..3)
}

prop_compose! {
    fn any_query()(
        normalized_title in normalized_title(),
        content_type in content_type(),
        release_year in safe_year(),
        tags in names(),
        authors in names(),
    ) -> Query {
        Query { normalized_title, content_type, release_year, tags, authors }
    }
}

prop_compose! {
    /// `similarity` is deliberately allowed outside `[0, 1]`: it arrives from the database's
    /// trigram operator and nothing validates its range at this boundary, so the scorer has to
    /// stay bounded even if it ever does.
    fn any_candidate()(
        normalized_title in normalized_title(),
        similarity in -2.0f32..2.0f32,
        content_type in content_type(),
        release_year in safe_year(),
        tags in names(),
        authors in names(),
    ) -> Candidate {
        Candidate {
            series_id: SeriesId::new(),
            normalized_title,
            similarity,
            content_type,
            release_year,
            tags,
            authors,
        }
    }
}

proptest! {
    /// Symmetry. `token_set_ratio` is a set-overlap measure, so swapping the arguments cannot
    /// change it — and the caller relies on that, because `score` passes query-then-candidate
    /// while the merge-candidate review path reads the pair the other way round. Pinned today
    /// by two literal pairs only.
    #[test]
    fn token_set_ratio_is_symmetric(a in normalized_title(), b in normalized_title()) {
        prop_assert_eq!(
            tankovault_matcher::token_set_ratio(&a, &b),
            tankovault_matcher::token_set_ratio(&b, &a)
        );
    }

    /// The ratio is a probability-shaped number. A value outside `[0, 1]` — or a `NaN` from an
    /// empty-set division — would propagate straight into `score` and past the decision
    /// thresholds, where `NaN >= high` is `false` and every match would silently become
    /// `Create`.
    #[test]
    fn token_set_ratio_is_bounded_and_never_nan(a in ".*", b in ".*") {
        let r = tankovault_matcher::token_set_ratio(&a, &b);
        prop_assert!(!r.is_nan(), "NaN for {a:?}/{b:?}");
        prop_assert!((0.0..=1.0).contains(&r), "{r} out of range for {a:?}/{b:?}");
    }

    /// Reflexivity on non-empty input: a title always matches itself perfectly.
    #[test]
    fn token_set_ratio_is_reflexive(a in normalized_title()) {
        prop_assume!(!a.is_empty());
        prop_assert_eq!(tankovault_matcher::token_set_ratio(&a, &a), 1.0);
    }

    /// `score` is clamped to `[0, 1]` and never `NaN`, whatever the bonuses sum to. The
    /// thresholds in `decide` are plain `>=` comparisons, so a `NaN` here does not error — it
    /// silently reclassifies every candidate as "create a new series".
    #[test]
    fn score_is_bounded_and_never_nan(q in any_query(), c in any_candidate()) {
        let s = score(&q, &c);
        prop_assert!(!s.is_nan(), "NaN score");
        prop_assert!((0.0..=1.0).contains(&s), "score {s} out of range");
    }

    /// Monotonic in the trigram similarity: a candidate the database considers *more* similar
    /// can never score lower, all else being equal. Without this the ranked suggestion list an
    /// operator reads could invert.
    #[test]
    fn score_is_monotonic_in_similarity(
        q in any_query(),
        c in any_candidate(),
        lower in -2.0f32..2.0f32,
        delta in 0.0f32..4.0f32,
    ) {
        let mut weaker = c.clone();
        weaker.similarity = lower;
        let mut stronger = c;
        stronger.similarity = lower + delta;
        prop_assert!(
            score(&q, &weaker) <= score(&q, &stronger),
            "raising similarity from {} to {} lowered the score", lower,
            lower + delta
        );
    }

    /// An exactly-equal normalized title can never score below any other title, all else
    /// being equal. This is the property the whole `normalized_title` column exists to serve:
    /// if it did not hold, two sources with byte-identical keys could rank below a near-miss.
    #[test]
    fn an_exactly_equal_title_never_loses_to_a_different_one(
        q in any_query(),
        c in any_candidate(),
    ) {
        prop_assume!(c.normalized_title != q.normalized_title);
        let mut identical = c.clone();
        identical.normalized_title.clone_from(&q.normalized_title);
        prop_assert!(
            score(&q, &identical) >= score(&q, &c),
            "an identical title scored below {:?}",
            c.normalized_title
        );
    }

    /// `decide` and `best_match` are two hand-written maxima over the same scored list
    /// (`lib.rs:197` and `lib.rs:206`). They can drift. This pins them to one another: the
    /// decision must be exactly the band the best score falls in.
    #[test]
    fn decide_agrees_with_best_match(
        q in any_query(),
        candidates in prop::collection::vec(any_candidate(), 0..6),
    ) {
        let thresholds = Thresholds::default();
        let decision = decide(&q, &candidates, thresholds);
        match best_match(&q, &candidates) {
            None => prop_assert_eq!(decision, Decision::Create),
            Some((id, s)) if s >= thresholds.high => {
                prop_assert_eq!(decision, Decision::Attach(id));
            }
            Some((id, s)) if s >= thresholds.low => {
                prop_assert_eq!(decision, Decision::Ambiguous { candidate: id, score: s });
            }
            Some(_) => prop_assert_eq!(decision, Decision::Create),
        }
    }
}

/// **Regression: the release-year proximity bonus used to panic at the extremes.**
///
/// `score` computed `(a - b).abs()` on two `i32` release years, which overflows. With
/// `overflow-checks = true` in `[profile.release]` (added by the SEC-11 fix) that panicked in
/// release builds as well as debug:
///
/// ```text
/// panicked at crates\matcher\src\lib.rs:92:15: attempt to subtract with overflow
/// ```
///
/// It was reachable by any caller holding `sync.admin.read`:
/// `GET /v1/admin/sync/suggest?title=x&start_year=-2147483648` binds `SuggestQuery.start_year`
/// (`services\api\src\admin\sync.rs`) — an unvalidated `i32` straight off the query string —
/// into `matcher::Query.release_year`, and every candidate with a positive `release_year` then
/// overflowed the subtraction. `CatchPanicLayer` contained it, so the symptom was a `500`
/// rather than a crash, but the endpoint was unusable.
///
/// Fixed by `a.saturating_sub(b).saturating_abs()`. Do **not** replace this with a bounded-year
/// case: the whole point is the pair that overflows, and [`safe_year`] deliberately cannot
/// generate it.
#[test]
fn score_survives_the_extremes_of_the_release_year_range() {
    let query = Query {
        normalized_title: "x".to_owned(),
        content_type: ContentType::Unknown,
        release_year: Some(i32::MIN),
        tags: Vec::new(),
        authors: Vec::new(),
    };
    let candidate = Candidate {
        series_id: SeriesId::new(),
        normalized_title: "x".to_owned(),
        similarity: 0.5,
        content_type: ContentType::Unknown,
        release_year: Some(i32::MAX),
        tags: Vec::new(),
        authors: Vec::new(),
    };
    let s = score(&query, &candidate);
    assert!((0.0..=1.0).contains(&s));
}
