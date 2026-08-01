//! Properties of [`RouteClassifier`], the map from a matched route pattern to its rate-limit
//! budget.
//!
//! These assert that the *rule set* determines the classification, not the order rules were
//! declared in — the in-module tests only check one hand-built pair.

use axum::http::Method;
use proptest::prelude::*;
use std::collections::BTreeMap;
use tankovault_service::ratelimit::{RouteClass, RouteClassifier};

/// How a rule was declared. Mirrors the three public builder methods, which is the whole
/// vocabulary a service has.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    Auth,
    Expensive,
    ExpensiveWrite,
}

/// A rule set keyed by prefix: same-prefix rules differ only by insertion order, which is
/// the case that must be excluded from the order-independence property below.
fn rule_set() -> impl Strategy<Value = BTreeMap<String, Kind>> {
    prop::collection::btree_map(
        prop_oneof![
            // Realistic prefixes, including nested ones so longest-prefix-wins is exercised.
            Just("/v1".to_owned()),
            Just("/v1/auth".to_owned()),
            Just("/v1/auth/login".to_owned()),
            Just("/v1/me".to_owned()),
            Just("/v1/me/export".to_owned()),
            Just("/v1/admin".to_owned()),
            Just("/v1/admin/scans".to_owned()),
            // Two distinct prefixes of equal length, which is the case the sort's secondary
            // key exists for.
            Just("/v1/aaaa".to_owned()),
            Just("/v1/bbbb".to_owned()),
        ],
        prop_oneof![
            Just(Kind::Auth),
            Just(Kind::Expensive),
            Just(Kind::ExpensiveWrite)
        ],
        0..7,
    )
}

/// Build a classifier by declaring `rules` in the given order.
fn build(rules: &[(String, Kind)]) -> RouteClassifier {
    rules
        .iter()
        .fold(RouteClassifier::new(), |acc, (prefix, kind)| match kind {
            Kind::Auth => acc.auth(prefix.clone()),
            Kind::Expensive => acc.expensive(prefix.clone()),
            Kind::ExpensiveWrite => acc.expensive_write(prefix.clone()),
        })
}

fn method() -> impl Strategy<Value = Method> {
    prop::sample::select(vec![
        Method::GET,
        Method::HEAD,
        Method::OPTIONS,
        Method::POST,
        Method::PUT,
        Method::PATCH,
        Method::DELETE,
    ])
}

/// Route *patterns*, which is what the classifier sees — never a concrete path, because
/// matching on `MatchedPath` is what stops a classification being dodged by varying an id.
fn route_pattern() -> impl Strategy<Value = String> {
    prop_oneof![
        prop::sample::select(vec![
            "/v1",
            "/v1/auth/login",
            "/v1/auth/register",
            "/v1/me",
            "/v1/me/export",
            "/v1/me/sync/conflicts",
            "/v1/admin/scans",
            "/v1/admin/users/{id}",
            "/v1/series/{id}",
            "/v1/aaaa/x",
            "/v1/bbbb/x",
            "/healthz",
            "/",
            "",
        ])
        .prop_map(str::to_owned),
        // Arbitrary text as well: `classify` is reached from a middleware layer and must be
        // total over anything axum can report as a matched path.
        ".{0,24}",
    ]
}

proptest! {
    /// The rule *set* decides the class; the order the rules were declared in does not —
    /// otherwise moving one line while tidying `main` would silently re-bucket a route.
    #[test]
    fn the_rule_set_decides_the_class_not_the_order_it_was_declared_in(
        rules in rule_set(),
        method in method(),
        path in route_pattern(),
    ) {
        let declared: Vec<(String, Kind)> = rules.into_iter().collect();
        let forwards = build(&declared);

        let mut backwards = declared.clone();
        backwards.reverse();
        prop_assert_eq!(
            forwards.classify(&method, &path),
            build(&backwards).classify(&method, &path),
            "reversing the declaration order changed the class of {} {:?}", method, path
        );

        // And a rotation, so the assertion is not satisfied by a symmetry peculiar to
        // reversal alone.
        if declared.len() > 1 {
            let mut rotated = declared.clone();
            rotated.rotate_left(1);
            prop_assert_eq!(
                forwards.classify(&method, &path),
                build(&rotated).classify(&method, &path),
                "rotating the declaration order changed the class of {} {:?}", method, path
            );
        }
    }

    /// Totality: `classify` runs in a middleware layer, so a panic here is a `500` on an
    /// otherwise-fine request.
    #[test]
    fn classify_is_total(
        rules in rule_set(),
        method in method(),
        path in ".*",
    ) {
        let declared: Vec<(String, Kind)> = rules.into_iter().collect();
        let _ = build(&declared).classify(&method, &path);
    }

    /// A `writes_only` rule is invisible to a safe method.
    ///
    /// Asserted as an equivalence against the same rule set with those rules removed, rather
    /// than by checking one path: `expensive_write` exists so a console `GET` that the UI polls
    /// is not charged the tight budget its sibling `POST` is (they share one route pattern), and
    /// the way that breaks is a safe request falling into `Expensive` *through* a writes-only
    /// rule instead of past it to a broader one.
    #[test]
    fn a_writes_only_rule_is_invisible_to_a_safe_method(
        rules in rule_set(),
        path in route_pattern(),
    ) {
        let declared: Vec<(String, Kind)> = rules.into_iter().collect();
        let without: Vec<(String, Kind)> = declared
            .iter()
            .filter(|(_, kind)| *kind != Kind::ExpensiveWrite)
            .cloned()
            .collect();

        for method in [Method::GET, Method::HEAD, Method::OPTIONS] {
            prop_assert_eq!(
                build(&declared).classify(&method, &path),
                build(&without).classify(&method, &path),
                "a writes-only rule changed the class of {} {:?}", method, path
            );
        }
    }

    /// An empty classifier puts everything on the shared global budget, and that is the only
    /// thing an unmatched path can fall back to. Stated because the fallback is a `map_or`
    /// default rather than a rule, so it is the one branch no rule set can exercise.
    #[test]
    fn an_unclassified_route_falls_back_to_the_global_budget(
        method in method(),
        path in ".*",
    ) {
        prop_assert_eq!(
            RouteClassifier::new().classify(&method, &path),
            RouteClass::Global
        );
    }
}
