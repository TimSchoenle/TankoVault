//! What a switched-off feature actually does to a real request: that the gate layer is mounted,
//! that `tankovault_api::route_features` names paths that actually exist, and that a disabled
//! route gets the documented answer rather than running anyway. `crates/service`'s unit tests
//! cover only the resolution logic, not this end-to-end path.

#![cfg(feature = "integration")]

use axum::http::StatusCode;
use tankovault_api_test_support::{TestApp, TestConfig};
use tankovault_domain::{AccountStatus, Feature};

/// A gated route answers `404` with the RFC 9457 body, and the body names the feature.
///
/// `404`, not `403`: a disabled feature is not part of the deployment's API, whereas `403` would
/// falsely claim a permission problem. Naming the feature lets an operator debug the 404 from the
/// response alone.
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
/// Rules out a prefix rule wider than it looks: `/v1/me` prefixes the entire signed-in surface,
/// so `route_features` gates by exact path, and a regression there would look like a one-word
/// table change while taking down the whole application.
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
/// Not redundant with the tests above: a gate that answered `404` unconditionally would pass
/// both and break the deployment. This proves the `404` came from the flag, not a mis-mounted
/// layer.
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

/// Every prefix the API's route table gates still matches a published route: the anti-rot check
/// a `crates/service` unit test can't write, since `RouteFeatures` is a table of path strings a
/// rename can leave stale. Read out of the committed `openapi.json`.
///
/// # The bug this pins
///
/// `/v1/me/chapter-progress` was gated but no route ever had that path. The external-sync
/// suffixes are exempted by deriving from `tankovault_contracts::sync` rather than listing them,
/// so a suffix added there is exempted automatically.
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

    // The exemption is only sound while the surface it exempts still exists.
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
