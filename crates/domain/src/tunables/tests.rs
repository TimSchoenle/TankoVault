//! Registry tests: bounds, clamping and defaults.

#![expect(
    clippy::float_cmp,
    reason = "these compare against exact registry bounds and clamp results, not \n              computed values: a clamp returns its bound bit-for-bit, and a default \n              is the literal in the registry"
)]

use super::*;
use std::collections::BTreeSet;

#[test]
fn every_key_round_trips_and_is_unique() {
    let mut seen = BTreeSet::new();
    for &t in Tunable::all() {
        assert_eq!(Tunable::from_str(t.key()).unwrap(), t);
        assert!(seen.insert(t.key()), "duplicate key {}", t.key());
    }
    assert_eq!(seen.len(), Tunable::all().len());
}

#[test]
fn all_lists_every_variant() {
    // Hand-written and able to drift from the enum; bump it when adding a knob, or a
    // forgotten one is invisible to the console and to every reader.
    assert_eq!(Tunable::all().len(), 42);
}

#[test]
fn serde_uses_the_persisted_key() {
    assert_eq!(
        serde_json::to_string(&Tunable::DiversityLambda).unwrap(),
        "\"recsys.diversity.lambda\""
    );
}

/// **The default must be a legal value.**
///
/// The bug this pins: a default outside its own range makes the compiled fallback and the
/// clamped read disagree, so a deployment with an empty override table runs on a value the
/// API would refuse to write.
#[test]
fn every_default_sits_inside_its_own_range() {
    for &t in Tunable::all() {
        let spec = t.spec();
        assert!(spec.min <= spec.max, "{t} has an inverted range");
        assert!(
            spec.range().contains(&spec.default),
            "{t} defaults to {} outside {:?}",
            spec.default,
            spec.range()
        );
        assert!(spec.clamp(spec.default) - spec.default == 0.0, "{t}");
    }
}

#[test]
fn every_tunable_is_described() {
    for &t in Tunable::all() {
        let spec = t.spec();
        assert!(!spec.title.is_empty(), "{t} has no title");
        assert!(
            spec.description.len() > 30,
            "{t} needs a real description — it is read immediately before someone \
                 changes production"
        );
    }
}

/// **The k-anonymity threshold is a floor, not a default.**
///
/// The bug this pins: publishing `min_support` as an ordinary knob with a range starting at
/// one. Co-occurrence edges below the threshold describe identifiable individuals (§12.2),
/// so the bound has to be part of the registry rather than a convention the console
/// remembers.
#[test]
fn the_privacy_floor_is_the_bottom_of_its_range_and_survives_clamping() {
    let spec = Tunable::CooccurrenceMinSupport.spec();
    assert!(Tunable::CooccurrenceMinSupport.has_privacy_floor());
    assert_eq!(spec.min, 5.0);
    for attempt in [-100.0, 0.0, 1.0, 4.999] {
        assert_eq!(
            spec.clamp(attempt),
            5.0,
            "a stored {attempt} must still read as 5"
        );
    }
    // Nothing else claims the floor, or the refusal message would name the wrong reason.
    for &t in Tunable::all() {
        assert_eq!(
            t.has_privacy_floor(),
            t == Tunable::CooccurrenceMinSupport,
            "{t} floor"
        );
    }
}

/// A non-finite stored value would propagate through every comparison in the ranking
/// without producing a single error.
#[test]
fn a_non_finite_value_falls_back_to_the_default() {
    let spec = Tunable::DiversityLambda.spec();
    assert_eq!(spec.clamp(f64::NAN), spec.default);
    assert_eq!(spec.clamp(f64::INFINITY), spec.default);
    assert_eq!(spec.clamp(f64::NEG_INFINITY), spec.default);
}

#[test]
fn the_score_weights_are_all_in_the_scoring_group() {
    assert_eq!(Tunable::score_weights().len(), 5);
    for &t in Tunable::score_weights() {
        assert_eq!(t.spec().group, TunableGroup::Scoring, "{t}");
        assert_eq!(t.spec().kind, TunableKind::Weight, "{t}");
    }
}

/// Every value baked into stored model data must say so, or an operator changes it and
/// concludes the page is broken when nothing moves (§8.4).
#[test]
fn model_shaped_values_declare_that_they_need_a_rebuild() {
    for &t in &[
        Tunable::BuildEmbeddingDims,
        Tunable::BuildHnswM,
        Tunable::BuildHnswEfConstruction,
    ] {
        assert_eq!(t.spec().applies, Applies::NextFullBuild, "{t}");
    }
    for &t in &[
        Tunable::BuildMinFeatures,
        Tunable::CooccurrenceMinSupport,
        Tunable::CooccurrenceMaxListEntries,
        Tunable::PriorWeightWatchers,
    ] {
        assert_eq!(t.spec().applies, Applies::NextBuild, "{t}");
    }
    assert_eq!(
        Tunable::DiversityLambda.spec().applies,
        Applies::Immediately
    );
}

#[test]
fn keys_are_namespaced_under_recsys() {
    for &t in Tunable::all() {
        assert!(t.key().starts_with("recsys."), "{t} is not namespaced");
    }
}
