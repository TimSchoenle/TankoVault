//! The recommender's tuning surface, end to end through the real router.
//!
//! Three properties, each of which the design calls out as a way this feature fails silently:
//! the privacy floor cannot be lowered through the API (§8.3), a change reaches the thing it
//! configures (§8.4's failure mode is a knob that changes nothing), and the registry the console
//! reads is the registry the server honours (§10.3).
//!
//! Opt-in: gated behind the `integration` feature because it requires Docker.
#![cfg(feature = "integration")]

use axum::http::StatusCode;
use serde_json::{Value, json};
use tankovault_api_test_support::{TestApp, TestConfig};
use tankovault_domain::{AccountStatus, Permission, Tunable, UserId};

async fn operator(app: &TestApp) -> UserId {
    app.seed_user(
        "tuner",
        &[Permission::RecsysRead, Permission::RecsysWrite],
        AccountStatus::Active,
    )
    .await
}

/// One tunable's row out of the list response.
fn row<'a>(body: &'a Value, key: &str) -> &'a Value {
    body.as_array()
        .expect("an array of tunables")
        .iter()
        .find(|item| item["key"] == key)
        .unwrap_or_else(|| panic!("{key} is not published"))
}

/// **The k-anonymity threshold cannot be lowered through the API.**
///
/// The bug this pins: enforcing `recsys.cooccurrence.min_support >= 5` in the console's number
/// input and nowhere else. The bound is the threshold below which a "readers of X also read Y"
/// edge stops being a statistic and starts being a statement about identifiable people
/// (`docs/RECOMMENDATIONS.md` §12.2), and a `curl` past a UI validator is not an attack — it is
/// Tuesday.
#[tokio::test]
async fn the_cooccurrence_privacy_floor_cannot_be_lowered() {
    let app = TestApp::spawn_with(TestConfig::new().without_rate_limiting()).await;
    let bearer = app.bearer(operator(&app).await);
    let key = Tunable::CooccurrenceMinSupport.key();
    let path = format!("/v1/admin/recommendations/tunables/{key}");

    for attempt in [0.0, 1.0, 4.0, -3.0] {
        let (status, body) = app
            .call(
                "PUT",
                &path,
                Some(&bearer),
                Some(json!({ "value": attempt })),
            )
            .await;
        assert_eq!(
            status,
            StatusCode::BAD_REQUEST,
            "min_support {attempt} must be refused, got {body}"
        );
        // The refusal has to say *why*: an operator told only "out of range" will assume the
        // range is arbitrary and go looking for a way around it.
        let detail = body["detail"].as_str().unwrap_or_default();
        assert!(
            detail.contains("privacy"),
            "the refusal must name the reason, got {detail:?}"
        );
    }

    // Nothing was stored, and the published value is still the floor.
    let (_, body) = app
        .call(
            "GET",
            "/v1/admin/recommendations/tunables",
            Some(&bearer),
            None,
        )
        .await;
    let stored = row(&body, key);
    assert_eq!(stored["value"], json!(5.0));
    assert_eq!(stored["overridden"], json!(false));
    assert_eq!(stored["min"], json!(5.0));
    assert_eq!(
        stored["privacy_floor"],
        json!(true),
        "the console needs to know this bound is not a taste decision"
    );

    // The inverse leg: the knob is otherwise a normal knob. A test that only proved refusal
    // would also pass against an endpoint that refused everything.
    let (status, _) = app
        .call("PUT", &path, Some(&bearer), Some(json!({ "value": 20.0 })))
        .await;
    assert_eq!(status, StatusCode::OK);
}

/// Every bound is enforced by the server, not only by the page.
#[tokio::test]
async fn a_value_outside_its_range_is_refused_at_both_ends() {
    let app = TestApp::spawn_with(TestConfig::new().without_rate_limiting()).await;
    let bearer = app.bearer(operator(&app).await);

    for (tunable, value) in [
        (Tunable::DiversityLambda, 5.0),
        (Tunable::DiversityLambda, -1.0),
        (Tunable::ServeShelfSize, 0.0),
        (Tunable::ServeShelfSize, 1000.0),
        (Tunable::AffinityDroppedFloor, 0.5),
    ] {
        let (status, body) = app
            .call(
                "PUT",
                &format!("/v1/admin/recommendations/tunables/{}", tunable.key()),
                Some(&bearer),
                Some(json!({ "value": value })),
            )
            .await;
        assert_eq!(
            status,
            StatusCode::BAD_REQUEST,
            "{tunable} = {value} must be refused, got {body}"
        );
    }

    let (status, _) = app
        .call(
            "PUT",
            "/v1/admin/recommendations/tunables/recsys.diversity.teleport",
            Some(&bearer),
            Some(json!({ "value": 1.0 })),
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "an unknown key is refused");
}

/// **A shelf with every score weight at zero has nothing to rank by.**
///
/// The bug this pins: the five weights are rank-normalised per path before blending, so their
/// scale is free and no individual zero is wrong. All five at zero is: the blend then sums to
/// zero for every candidate, ties break by id, and the reader is served a shelf ordered by UUID
/// with no error raised anywhere.
#[tokio::test]
async fn zeroing_every_score_weight_is_refused() {
    let app = TestApp::spawn_with(TestConfig::new().without_rate_limiting()).await;
    let bearer = app.bearer(operator(&app).await);

    let weights = Tunable::score_weights();
    let (last, rest) = weights.split_last().expect("five weights");

    // Four zeroes are legitimate — that is how an operator pins the shelf to one retrieval path.
    for tunable in rest {
        let (status, body) = app
            .call(
                "PUT",
                &format!("/v1/admin/recommendations/tunables/{}", tunable.key()),
                Some(&bearer),
                Some(json!({ "value": 0.0 })),
            )
            .await;
        assert_eq!(
            status,
            StatusCode::OK,
            "{tunable} = 0 alone is legal: {body}"
        );
    }

    let (status, body) = app
        .call(
            "PUT",
            &format!("/v1/admin/recommendations/tunables/{}", last.key()),
            Some(&bearer),
            Some(json!({ "value": 0.0 })),
        )
        .await;
    assert_eq!(
        status,
        StatusCode::BAD_REQUEST,
        "the fifth zero must be refused, got {body}"
    );
}

/// **The page cannot show a value the server does not honour.**
///
/// The bug this pins is the hand-maintained-vocabulary class this repo has already been bitten by
/// once: a console list that is not the compiled registry drifts, and the symptom is a knob that
/// is either invisible or does nothing. Every registry entry must appear, with the bounds and the
/// `applies` badge the console renders from.
#[tokio::test]
async fn the_listing_is_the_compiled_registry() {
    let app = TestApp::spawn_with(TestConfig::new().without_rate_limiting()).await;
    let bearer = app.bearer(operator(&app).await);

    let (status, body) = app
        .call(
            "GET",
            "/v1/admin/recommendations/tunables",
            Some(&bearer),
            None,
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        body.as_array().map(Vec::len),
        Some(Tunable::all().len()),
        "the listing must publish every tunable this build defines"
    );

    for &tunable in Tunable::all() {
        let published = row(&body, tunable.key());
        let spec = tunable.spec();
        assert_eq!(published["default_value"], json!(spec.default), "{tunable}");
        assert_eq!(published["min"], json!(spec.min), "{tunable}");
        assert_eq!(published["max"], json!(spec.max), "{tunable}");
        assert_eq!(
            published["applies"],
            json!(spec.applies.as_str()),
            "{tunable} must say when a change takes effect"
        );
        assert!(
            published["description"]
                .as_str()
                .is_some_and(|d| d.len() > 30),
            "{tunable} must carry the text an operator reads before changing production"
        );
    }
}

/// A write is recorded, reported and reversible.
#[tokio::test]
async fn a_write_is_audited_and_a_reset_restores_the_default() {
    let app = TestApp::spawn_with(TestConfig::new().without_rate_limiting()).await;
    let user = operator(&app).await;
    let bearer = app.bearer(user);
    let key = Tunable::DiversityLambda.key();
    let path = format!("/v1/admin/recommendations/tunables/{key}");

    let (status, body) = app
        .call(
            "PUT",
            &path,
            Some(&bearer),
            Some(json!({ "value": 0.2, "note": "too samey" })),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    let written = row(&body, key);
    assert_eq!(written["value"], json!(0.2));
    assert_eq!(written["overridden"], json!(true));
    assert_eq!(written["note"], json!("too samey"));
    assert_eq!(written["updated_by"], json!("tuner"));

    let events = app.audit.events();
    let recorded = events
        .iter()
        .find(|e| e.action == "recsys.tunable.set")
        .expect("the write is audited");
    assert_eq!(recorded.target.as_deref(), Some(key));
    assert_eq!(recorded.detail["value"], json!(0.2));

    let (status, body) = app.call("DELETE", &path, Some(&bearer), None).await;
    assert_eq!(status, StatusCode::OK);
    let reset = row(&body, key);
    assert_eq!(reset["value"], json!(0.7));
    assert_eq!(reset["overridden"], json!(false));
    assert!(reset["note"].is_null());
}

/// Reading the panel is not permission to change it.
#[tokio::test]
async fn a_reader_without_the_write_grant_cannot_change_anything() {
    let app = TestApp::spawn_with(TestConfig::new().without_rate_limiting()).await;
    let reader = app
        .seed_user("looker", &[Permission::RecsysRead], AccountStatus::Active)
        .await;
    let bearer = app.bearer(reader);

    let (status, _) = app
        .call(
            "GET",
            "/v1/admin/recommendations/tunables",
            Some(&bearer),
            None,
        )
        .await;
    assert_eq!(status, StatusCode::OK, "read-only access still reads");

    let (status, _) = app
        .call(
            "PUT",
            "/v1/admin/recommendations/tunables/recsys.diversity.lambda",
            Some(&bearer),
            Some(json!({ "value": 0.5 })),
        )
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
}

/// Model health answers before a build has ever run, which is the state a fresh install is in.
#[tokio::test]
async fn model_health_reports_an_unbuilt_model_rather_than_failing() {
    let app = TestApp::spawn_with(TestConfig::new().without_rate_limiting()).await;
    let bearer = app.bearer(operator(&app).await);

    let (status, body) = app
        .call(
            "GET",
            "/v1/admin/recommendations/health",
            Some(&bearer),
            None,
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["stage"], json!("idle"));
    assert_eq!(body["building"], json!(false));
    assert_eq!(body["series_with_embedding"], json!(0));
    assert_eq!(body["repair_queue_depth"], json!(0));
    // The gap between these is the diagnosis the panel exists to show, so both have to be real
    // numbers rather than one being a stand-in for the other.
    assert!(body["series_total"].is_number());
    assert!(body["series_recommendable"].is_number());
}
