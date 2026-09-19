//! The catalogue purge runs detached from the request that starts it.
//!
//! # The bug this exists to stop
//!
//! The purge used to delete inside the request, batch after batch until a deadline, and the
//! console called it in a loop. On a large catalogue one batch alone outlasted the 30 s request
//! timeout, the handler future was dropped mid-transaction, the batch rolled back, and "Wipe the
//! entire catalogue" failed with a `408` on every attempt without removing a single row. The run
//! now outlives the request: `POST` claims and answers at once, and the work is read back from
//! `GET`. A handler that awaited the run again would pass the access matrix and fail here.
//!
//! Gated behind the `integration` feature (requires Docker).
#![cfg(feature = "integration")]

use axum::http::StatusCode;
use serde_json::{Value, json};
use std::time::{Duration, Instant};
use tankovault_api_test_support::{TestApp, TestConfig};
use tankovault_domain::{AccountStatus, Permission};
use tankovault_test_support::seed;

const PURGE: &str = "/v1/admin/catalogue/purge";

async fn operator(app: &TestApp) -> (String, String) {
    let user = app
        .seed_user(
            "purger",
            &[Permission::CatalogueRead, Permission::CatalogueDelete],
            AccountStatus::Active,
        )
        .await;
    (app.bearer(user), app.enrolled_and_elevated(user).await)
}

/// Poll the status route until the run releases its claim.
async fn settled(app: &TestApp, bearer: &str) -> Value {
    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        let (status, body) = app.call("GET", PURGE, Some(bearer), None).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        if body["running"] == false {
            return body;
        }
        assert!(Instant::now() < deadline, "purge still running: {body}");
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

#[tokio::test]
async fn a_started_purge_empties_the_catalogue_after_the_request_has_answered() {
    let app = TestApp::spawn_with(TestConfig::new().without_rate_limiting()).await;
    let (bearer, step_up) = operator(&app).await;
    let provider = seed::provider(&app.db, "alpha").create().await;
    let titles: Vec<String> = (0..25).map(|n| format!("Purged Series {n}")).collect();
    for title in &titles {
        seed::series(&app.db, provider, title)
            .chapters(&[1.0, 2.0])
            .create()
            .await;
    }

    let (status, body) = app
        .call_elevated(
            "POST",
            PURGE,
            Some(&bearer),
            Some(&step_up),
            Some(json!({ "scope": "everything", "confirm": "everything" })),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["started"], true, "{body}");

    let done = settled(&app, &bearer).await;
    assert_eq!(done["stopped"], "done", "{done}");
    assert_eq!(done["scope"], "everything", "{done}");
    assert_eq!(done["removed"]["series"], 25, "{done}");
    assert_eq!(done["removed"]["chapters"], 50, "{done}");
    assert_eq!(done["remaining"], 0, "{done}");

    let totals = tankovault_db::repo::catalog::maintenance::totals(&app.db.pool)
        .await
        .expect("catalogue totals");
    assert_eq!(totals.series_total, 0);
    assert_eq!(totals.chapters_total, 0);
}

#[tokio::test]
async fn a_mismatched_confirmation_starts_nothing() {
    let app = TestApp::spawn_with(TestConfig::new().without_rate_limiting()).await;
    let (bearer, step_up) = operator(&app).await;

    let (status, _) = app
        .call_elevated(
            "POST",
            PURGE,
            Some(&bearer),
            Some(&step_up),
            Some(json!({ "scope": "everything", "confirm": "chapters" })),
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);

    let (_, body) = app.call("GET", PURGE, Some(&bearer), None).await;
    assert_eq!(body["running"], false, "{body}");
    assert!(body["scope"].is_null(), "{body}");
}
