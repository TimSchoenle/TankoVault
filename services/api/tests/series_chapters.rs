//! `GET /v1/series/{id}/chapters/by-source` against the per-source route it replaced.
//!
//! The series screen renders both providers' lists side by side, so the batched answer has to be
//! the per-source answers — same sources, same order, same rows, same read-state. A drift here is
//! invisible in the response's own shape.

#![cfg(feature = "integration")]

use axum::http::StatusCode;
use serde_json::json;
use tankovault_api_test_support::{TestApp, TestConfig};
use tankovault_config::MatchingConfig;
use tankovault_db::repo::catalog::{ChapterUpsert, ScannedSeries, SeriesUpsert, ingest_series};
use tankovault_domain::{
    AccountStatus, ContentType, ProviderId, SeriesId, SeriesStatus, normalize_title,
};
use tankovault_test_support::seed;

/// Ingest `title` on `provider` carrying chapters 1..=`chapters`.
async fn ingest(app: &TestApp, provider: ProviderId, title: &str, chapters: u32) -> SeriesId {
    let numbers = (1..=chapters).map(|n| ChapterUpsert {
        number: f64::from(n),
        title: None,
        path: format!("/c/{n}"),
        published_at: None,
        access: tankovault_domain::ChapterAccess::Free,
        unlocks_at: None,
    });
    ingest_series(
        &app.db.pool,
        &ScannedSeries {
            provider_id: provider,
            source_path: format!("/s/{}", normalize_title(title).replace(' ', "-")),
            provider_title: Some(title.to_owned()),
            meta: SeriesUpsert {
                canonical_title: title.to_owned(),
                normalized_title: normalize_title(title),
                description: None,
                cover_url: None,
                content_type: ContentType::Manhwa,
                status: SeriesStatus::Ongoing,
                release_year: Some(2019),
            },
            alt_titles: Vec::new(),
            tags: vec!["action".to_owned()],
            authors: vec!["someone".to_owned()],
            chapters: numbers.collect(),
            content_hash: vec![1],
        },
        &MatchingConfig::default(),
        &tankovault_domain::MetadataPriority::default(),
        &tankovault_domain::TermBlocklist::default(),
        &tankovault_domain::AdultTagSet::defaults(),
    )
    .await
    .expect("ingest")
    .series_id
}

/// One title carried by three providers, which is what the batched route exists for.
async fn carried_by_three(app: &TestApp) -> SeriesId {
    let mut id = None;
    for (slug, chapters) in [("alpha", 5_u32), ("beta", 3), ("gamma", 7)] {
        let provider = seed::provider(&app.db, slug).create().await;
        id = Some(ingest(app, provider, "Widely Carried Work", chapters).await);
    }
    id.expect("three ingests")
}

fn source_ids(detail: &serde_json::Value) -> Vec<String> {
    detail["sources"]
        .as_array()
        .expect("sources array")
        .iter()
        .map(|s| s["id"].as_str().expect("source id").to_owned())
        .collect()
}

/// The batched route answers exactly what a call per source answers, in the detail's order.
///
/// The bug this pins: the series screen used to fetch one `.../chapters?source=` per source and
/// merge them client-side, so a well-carried title cost one request per provider — re-issued in
/// full on every read toggle. That spent the caller's rate-limit burst and the screen started
/// answering its own reads with `429`. Collapsing the fan-out into this route is only a fix while
/// the two agree: the moment the batched handler stops mirroring the per-source one — a different
/// provider grouping, a different order, a dropped early-access filter — the screen renders
/// chapters no single-source call would ever return.
#[tokio::test]
async fn the_batched_route_matches_a_call_per_source() {
    let app = TestApp::spawn_with(TestConfig::new().without_rate_limiting()).await;
    let id = carried_by_three(&app).await;

    let (status, detail) = app
        .call("GET", &format!("/v1/series/{id}"), None, None)
        .await;
    assert_eq!(status, StatusCode::OK);
    let sources = source_ids(&detail);
    assert_eq!(sources.len(), 3, "one source per provider: {sources:?}");

    let (status, batched) = app
        .call(
            "GET",
            &format!("/v1/series/{id}/chapters/by-source"),
            None,
            None,
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    let batched = batched.as_array().expect("an array body").clone();

    assert_eq!(
        batched
            .iter()
            .map(|e| e["source_id"].as_str().expect("source_id").to_owned())
            .collect::<Vec<_>>(),
        sources,
        "the batched route must key on, and order by, the detail's own sources"
    );

    for (entry, source) in batched.iter().zip(&sources) {
        let (status, single) = app
            .call(
                "GET",
                &format!("/v1/series/{id}/chapters?source={source}"),
                None,
                None,
            )
            .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(
            entry["chapters"], single,
            "batched and per-source lists disagree for {source}"
        );
    }
}

/// Read-state survives the batching: it is per series, but it is published per chapter.
///
/// The batched handler reads the frontier once for the whole series instead of once per source.
/// A refactor that loses the reader's identity on the way into the per-source projection would
/// still answer `200` with every chapter of every source — just silently unread, which on this
/// screen reads as "nothing has been marked" rather than as a failure.
#[tokio::test]
async fn read_state_is_populated_for_every_source() {
    let app = TestApp::spawn_with(TestConfig::new().without_rate_limiting()).await;
    let id = carried_by_three(&app).await;
    let user = app.seed_user("reader", &[], AccountStatus::Active).await;
    let token = app.bearer(user);

    let (status, _) = app
        .call(
            "PUT",
            &format!("/v1/me/progress/{id}"),
            Some(&token),
            Some(json!({ "last_read_whole_number": 2.0 })),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "seeding progress should succeed");

    let (status, batched) = app
        .call(
            "GET",
            &format!("/v1/series/{id}/chapters/by-source"),
            Some(&token),
            None,
        )
        .await;
    assert_eq!(status, StatusCode::OK);

    for entry in batched.as_array().expect("an array body") {
        for chapter in entry["chapters"].as_array().expect("chapters array") {
            let number = chapter["number"].as_f64().expect("number");
            assert_eq!(
                chapter["read"].as_bool(),
                Some(number <= 2.0),
                "chapter {number} of {} has the wrong read state",
                entry["source_id"]
            );
        }
    }

    // Anonymous callers have nothing to track, so the field is absent rather than `false`.
    let (_, anonymous) = app
        .call(
            "GET",
            &format!("/v1/series/{id}/chapters/by-source"),
            None,
            None,
        )
        .await;
    for entry in anonymous.as_array().expect("an array body") {
        for chapter in entry["chapters"].as_array().expect("chapters array") {
            assert!(
                chapter.get("read").is_none(),
                "read-state leaked to an anonymous caller"
            );
        }
    }
}
