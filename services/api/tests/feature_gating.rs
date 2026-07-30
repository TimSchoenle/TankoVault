//! What a switched-off feature actually does to a real request.
//!
//! `crates/service`'s `flags.rs` carries thirteen unit tests on the *resolution* logic — which
//! rule wins for a path, how the method axis narrows, what happens with no rule at all. None of
//! them reaches the half that matters operationally: that the layer is **mounted** on this
//! service's router, that the table in `tankovault_api::route_features` names paths that
//! actually exist, and that a caller hitting a disabled route gets the documented answer rather
//! than the handler running anyway.
//!
//! Nothing could reach it before, either. `TestApp` hardcoded `FeatureGate::defaults()`, so
//! every feature was on in every test (TESTING F-09) — `TestConfig::with_features_disabled` is
//! what opened it, and these are the tests it was opened for.

#![cfg(feature = "integration")]

use axum::http::StatusCode;
use tankovault_api_test_support::{TestApp, TestConfig};
use tankovault_domain::{AccountStatus, Feature};

/// A gated route answers `404` with the RFC 9457 body, and the body names the feature.
///
/// `404`, not `403`, is the deliberate part: a disabled feature genuinely is not part of this
/// deployment's API, while `403` would tell the caller they lack permission — false, and it
/// sends a user to an administrator who cannot help them.
///
/// Naming the feature is not decoration either. An operator debugging "why is the watchlist
/// 404ing" gets the answer from the response instead of having to correlate it against the
/// flag page.
#[tokio::test]
async fn a_disabled_feature_answers_404_and_names_itself() {
    let app = TestApp::spawn_with(
        TestConfig::new()
            .without_rate_limiting()
            .with_features_disabled(&[Feature::TrackingWatchlist]),
    )
    .await;
    let user = app.seed_user("reader", &[], AccountStatus::Active).await;
    let token = app.bearer(user);

    let (status, body) = app
        .call("GET", "/v1/me/watchlist", Some(&token), None)
        .await;

    assert_eq!(
        status,
        StatusCode::NOT_FOUND,
        "a switched-off feature is absent, not forbidden"
    );
    assert_eq!(body["title"], "feature_disabled");
    assert_eq!(
        body["feature"], "tracking.watchlist",
        "the body must name the feature, or an operator cannot act on the 404"
    );
    assert_eq!(
        body["status"], 404,
        "RFC 9457 requires the body's status member to echo the HTTP one"
    );
}

/// Switching one feature off leaves every other route alone.
///
/// The failure this rules out is a prefix rule that is wider than it looks. `/v1/me` is the
/// prefix of the entire signed-in surface, and `route_features` gates it **by exact path** for
/// exactly that reason. A regression there would switch off the whole application and would
/// look like a one-word change to a table.
#[tokio::test]
async fn switching_off_self_erasure_leaves_the_rest_of_the_signed_in_surface_alone() {
    let app = TestApp::spawn_with(
        TestConfig::new()
            .without_rate_limiting()
            .with_features_disabled(&[Feature::PrivacySelfErasure]),
    )
    .await;
    let user = app.seed_user("reader", &[], AccountStatus::Active).await;
    let token = app.bearer(user);

    let (status, _) = app.call("DELETE", "/v1/me", Some(&token), None).await;
    assert_eq!(
        status,
        StatusCode::NOT_FOUND,
        "self-service erasure is the route that was switched off"
    );

    for path in ["/v1/me/watchlist", "/v1/me/notifications", "/v1/me/stats"] {
        let (status, _) = app.call("GET", path, Some(&token), None).await;
        assert_ne!(
            status,
            StatusCode::NOT_FOUND,
            "`{path}` shares the `/v1/me` prefix but not the flag; gating that prefix rather \
             than the exact path would take the whole signed-in surface down"
        );
    }
}

/// With every feature on, the same route behaves normally.
///
/// The inverse leg, and it is not redundant: a gate that answered `404` unconditionally would
/// pass the two tests above and break the deployment. This is what says the `404` came from the
/// flag rather than from the route being absent or the layer being mis-mounted.
#[tokio::test]
async fn the_same_route_is_reachable_with_the_feature_on() {
    let app = TestApp::spawn_with(TestConfig::new().without_rate_limiting()).await;
    let user = app.seed_user("reader", &[], AccountStatus::Active).await;
    let token = app.bearer(user);

    let (status, _) = app
        .call("GET", "/v1/me/watchlist", Some(&token), None)
        .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "the watchlist must serve normally when its feature is on"
    );
}

/// Every prefix the API's route table gates still matches a published route.
///
/// This is the anti-rot half, and it is the one a unit test in `crates/service` structurally
/// cannot write: `RouteFeatures` is a table of path *strings*, so a route renamed in a
/// `#[utoipa::path]` leaves its gate behind — silently ungating the route while leaving a rule
/// that matches nothing, which is the worst of both. Read out of the committed `openapi.json`,
/// the same artefact `openapi_contract.rs` uses.
///
/// It already found one: `/v1/me/chapter-progress` was gated and no route has ever had that
/// path. Harmless as it stood — the real route sits under the `/v1/me/progress` prefix, which
/// is gated too — and deleted rather than left, because a rule that gates nothing while
/// looking like it gates something is how the next person concludes their new endpoint is
/// already covered.
///
/// The external-sync suffixes are the one exemption, and it is **derived rather than listed**:
/// ARCH-18 declares them once in `tankovault_contracts::sync` and folds the same set into both
/// tiers' tables, deliberately, so each tier gates suffixes it does not itself serve. A rule
/// for an unrouted path never matches, and the alternative — each tier filtering the shared
/// list to what it happens to mount — is exactly the per-tier judgement that had already
/// drifted. Taking the exemption from the shared declaration means a suffix added there is
/// exempted automatically and a suffix added *here* is not.
#[tokio::test]
async fn every_gated_prefix_still_matches_a_published_route() {
    const SPEC: &str = include_str!("../../../openapi.json");
    let spec: serde_json::Value = serde_json::from_str(SPEC).expect("openapi.json parses");
    let published: Vec<&str> = spec["paths"]
        .as_object()
        .expect("the document declares paths")
        .keys()
        .map(String::as_str)
        .collect();

    let shared_sync: Vec<String> = tankovault_contracts::sync::sync_route_features()
        .iter()
        .map(|(suffix, _)| format!("/v1/me/sync{suffix}"))
        .collect();

    // The exemption is only sound while the surface it exempts exists at all. Without this,
    // `/v1/me/sync/**` disappearing from the document would silently excuse every one of its
    // gates instead of failing.
    assert!(
        published.iter().any(|path| path.starts_with("/v1/me/sync")),
        "the external-sync surface is exempted from the check below because ARCH-18 gates it \
         from a shared declaration; if the API stops publishing it, the exemption is covering \
         a real disappearance"
    );

    for (prefix, feature) in tankovault_api::route_features().rules() {
        if shared_sync.iter().any(|shared| shared == prefix) {
            continue;
        }
        assert!(
            published.iter().any(|path| path.starts_with(prefix)),
            "`{prefix}` is gated behind `{}`, but no published route begins with it — either \
             the route was renamed and its gate left behind, or the gate names a path that \
             never existed. Both leave the route ungated.",
            feature.key()
        );
    }
}
