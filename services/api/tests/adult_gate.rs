//! The adult-content gate, end to end: who can see a gated series through a real request.
//!
//! Every read surface resolves the same two-part answer — the deployment flag *and* the reader's
//! own opt-in — so these tests drive the routes rather than the repository. A gate that holds in
//! `crates/db` and leaks through a handler that forgot to ask is the failure worth catching.

#![cfg(feature = "integration")]

use axum::http::StatusCode;
use serde_json::json;
use tankovault_api_test_support::{TestApp, TestConfig};
use tankovault_config::MatchingConfig;
use tankovault_db::repo::catalog::{ChapterUpsert, ScannedSeries, SeriesUpsert, ingest_series};
use tankovault_domain::{
    AccountStatus, ContentType, Feature, MetadataPriority, ProviderId, SeriesId, SeriesStatus,
    UserId, normalize_title,
};
use tankovault_test_support::seed;

/// An app with the deployment half of the gate open, so the reader's half is what is under test.
async fn app_with_adult_allowed() -> TestApp {
    TestApp::spawn_with(
        TestConfig::new()
            .without_rate_limiting()
            .with_features_enabled(&[Feature::CatalogueAdultContent]),
    )
    .await
}

/// Ingest one series. `tags` decides whether the ingest classifier gates it.
async fn ingest(app: &TestApp, provider: ProviderId, title: &str, tags: &[&str]) -> SeriesId {
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
                release_year: Some(2016),
            },
            alt_titles: Vec::new(),
            tags: tags.iter().map(|t| (*t).to_owned()).collect(),
            authors: vec!["someone".to_owned()],
            chapters: vec![ChapterUpsert {
                number: 1.0,
                volume: None,
                title: None,
                path: "/c/1".to_owned(),
                published_at: None,
                access: tankovault_domain::ChapterAccess::Free,
                unlocks_at: None,
                access: tankovault_domain::ChapterAccess::Free,
                unlocks_at: None,
            }],
            content_hash: vec![1],
        },
        &MatchingConfig::default(),
        &MetadataPriority::default(),
        &tankovault_domain::TagBlocklist::default(),
        &tankovault_domain::AdultTagSet::defaults(),
    )
    .await
    .expect("ingest")
    .series_id
}

/// A catalogue of one gated and one ordinary series, plus a provider to hang them off.
async fn catalogue(app: &TestApp) -> (SeriesId, SeriesId) {
    let provider = seed::provider(&app.db, "alpha").create().await;
    let gated = ingest(app, provider, "Gated Work", &["Hentai", "Romance"]).await;
    let ordinary = ingest(app, provider, "Ordinary Work", &["Romance", "Drama"]).await;
    (gated, ordinary)
}

/// Turn a reader's opt-in on, attesting in the same request.
async fn opt_in(app: &TestApp, user: UserId) {
    let token = app.bearer(user);
    let (status, _) = app
        .call(
            "PUT",
            "/v1/me/content-prefs",
            Some(&token),
            Some(json!({ "adult_opt_in": true, "confirm_age": true })),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "opting in should succeed");
}

fn titles(body: &serde_json::Value) -> Vec<String> {
    body.as_array()
        .expect("an array body")
        .iter()
        .map(|s| s["title"].as_str().unwrap_or_default().to_owned())
        .collect()
}

/// An unauthenticated caller never sees a gated series, on any catalogue surface.
///
/// The bug this pins: the opt-in lives on an account, so a request with no account has nothing
/// to consult — and the natural way to write that resolution is to reach for the token, find
/// none, and fall through to whatever the local variable was initialised to. There is no
/// anonymous state in which adult content is correct, and the deployment flag being *on* is
/// exactly when this would go unnoticed, because the operator sees their own opted-in view.
#[tokio::test]
async fn an_anonymous_caller_never_sees_a_gated_series() {
    let app = app_with_adult_allowed().await;
    let (gated, _) = catalogue(&app).await;

    let (status, body) = app.call("GET", "/v1/series", None, None).await;
    assert_eq!(status, StatusCode::OK);
    let listed = titles(&body);
    assert!(
        listed.contains(&"Ordinary Work".to_owned()),
        "the ungated series must still be browsable: {listed:?}"
    );
    assert!(
        !listed.contains(&"Gated Work".to_owned()),
        "browse leaked a gated series to an anonymous caller: {listed:?}"
    );

    let (status, body) = app.call("GET", "/v1/series?query=Gated", None, None).await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        !titles(&body).contains(&"Gated Work".to_owned()),
        "search leaked a gated series to an anonymous caller"
    );

    let (status, _) = app
        .call("GET", &format!("/v1/series/{gated}"), None, None)
        .await;
    assert_eq!(
        status,
        StatusCode::NOT_FOUND,
        "a gated series must be indistinguishable from an id that does not exist"
    );
}

/// A signed-in reader who has not opted in is in exactly the anonymous position.
///
/// Having an account is not consent. The failure this rules out is a resolution that treats
/// "authenticated" as the condition instead of "authenticated *and* opted in".
#[tokio::test]
async fn a_signed_in_reader_who_never_opted_in_sees_nothing_gated() {
    let app = app_with_adult_allowed().await;
    let (gated, _) = catalogue(&app).await;
    let user = app.seed_user("reader", &[], AccountStatus::Active).await;
    let token = app.bearer(user);

    let (_, body) = app.call("GET", "/v1/series", Some(&token), None).await;
    assert!(
        !titles(&body).contains(&"Gated Work".to_owned()),
        "an account that never opted in must see the same catalogue as an anonymous caller"
    );

    let (status, _) = app
        .call("GET", &format!("/v1/series/{gated}"), Some(&token), None)
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

/// Opting in opens the gate, and opting back out closes it again.
#[tokio::test]
async fn opting_in_opens_the_gate_and_opting_out_closes_it() {
    let app = app_with_adult_allowed().await;
    let (gated, _) = catalogue(&app).await;
    let user = app.seed_user("reader", &[], AccountStatus::Active).await;
    let token = app.bearer(user);

    opt_in(&app, user).await;

    let (_, body) = app.call("GET", "/v1/series", Some(&token), None).await;
    assert!(
        titles(&body).contains(&"Gated Work".to_owned()),
        "an opted-in reader must see gated series"
    );
    let (status, body) = app
        .call("GET", &format!("/v1/series/{gated}"), Some(&token), None)
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        body["is_adult"], true,
        "a series a reader is entitled to see must still be labelled as gated"
    );

    let (status, _) = app
        .call(
            "PUT",
            "/v1/me/content-prefs",
            Some(&token),
            Some(json!({ "adult_opt_in": false })),
        )
        .await;
    assert_eq!(status, StatusCode::OK);

    let (_, body) = app.call("GET", "/v1/series", Some(&token), None).await;
    assert!(
        !titles(&body).contains(&"Gated Work".to_owned()),
        "opting out must take effect on the next request, not on the next cache expiry"
    );
}

/// The deployment flag overrides an opted-in reader.
///
/// The bug this pins: the two conditions are resolved in one place precisely so that neither can
/// be forgotten, and the tempting simplification is to treat the reader's stored preference as
/// the whole answer — which silently turns the operator's kill switch into a no-op for exactly
/// the readers it exists to cover.
#[tokio::test]
async fn the_deployment_flag_overrides_a_readers_opt_in() {
    // The flag at its shipped default of off.
    let app = TestApp::spawn_with(TestConfig::new().without_rate_limiting()).await;
    let (gated, _) = catalogue(&app).await;
    let user = app.seed_user("reader", &[], AccountStatus::Active).await;

    // Stored through the repository rather than the endpoint: this is an account that opted in
    // while some deployment allowed it, and is now being served by one that does not. Driving
    // the API instead would only prove the API refuses to *set* it, which is a weaker claim than
    // the one under test — that an already-stored opt-in does not open the gate.
    tankovault_db::repo::users::set_content_prefs(&app.db.pool, user, true, true)
        .await
        .expect("store the opt-in");

    let token = app.bearer(user);
    let (_, body) = app.call("GET", "/v1/series", Some(&token), None).await;
    assert!(
        !titles(&body).contains(&"Gated Work".to_owned()),
        "the deployment flag must close the gate for a reader who opted in"
    );
    let (status, _) = app
        .call("GET", &format!("/v1/series/{gated}"), Some(&token), None)
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

/// Opting in without attesting is refused, and leaves the preference untouched.
#[tokio::test]
async fn opting_in_without_attesting_is_refused() {
    let app = app_with_adult_allowed().await;
    let user = app.seed_user("reader", &[], AccountStatus::Active).await;
    let token = app.bearer(user);

    let (status, _) = app
        .call(
            "PUT",
            "/v1/me/content-prefs",
            Some(&token),
            Some(json!({ "adult_opt_in": true })),
        )
        .await;
    assert_eq!(
        status,
        StatusCode::CONFLICT,
        "an opt-in with no age attestation must be refused"
    );

    let (_, body) = app
        .call("GET", "/v1/me/content-prefs", Some(&token), None)
        .await;
    assert_eq!(
        body["adult_opt_in"], false,
        "a refused opt-in must not be partially applied"
    );
    assert_eq!(body["age_attested"], false);
}

/// The attestation survives opting out, so a reader is never asked to declare their age twice.
#[tokio::test]
async fn the_attestation_outlives_the_preference() {
    let app = app_with_adult_allowed().await;
    let user = app.seed_user("reader", &[], AccountStatus::Active).await;
    let token = app.bearer(user);

    opt_in(&app, user).await;
    let (_, body) = app
        .call(
            "PUT",
            "/v1/me/content-prefs",
            Some(&token),
            Some(json!({ "adult_opt_in": false })),
        )
        .await;
    assert_eq!(body["age_attested"], true, "opting out must not un-attest");

    // Back on, this time with no `confirm_age` at all: the stored attestation is enough.
    let (status, body) = app
        .call(
            "PUT",
            "/v1/me/content-prefs",
            Some(&token),
            Some(json!({ "adult_opt_in": true })),
        )
        .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "an account that already attested must not be asked again"
    );
    assert_eq!(body["adult_opt_in"], true);
}

/// A provider genre alone gates a series, with no `AniList` involvement at all.
///
/// The bug this pins: `series.is_adult` is written only by the `AniList` enrichment sweep, so
/// before the ingest classifier existed every series the sweep had not matched — most of a
/// freshly scanned catalogue, and permanently so for anything `AniList` does not carry — sat at
/// the column's `false` default and read as safe.
#[tokio::test]
async fn a_provider_genre_alone_gates_a_series() {
    let app = app_with_adult_allowed().await;
    let provider = seed::provider(&app.db, "alpha").create().await;
    ingest(&app, provider, "Scraped Only", &["Adult", "Action"]).await;

    let (_, body) = app.call("GET", "/v1/series", None, None).await;
    assert!(
        !titles(&body).contains(&"Scraped Only".to_owned()),
        "a series classified from its own genre chips must be gated without AniList"
    );
}
