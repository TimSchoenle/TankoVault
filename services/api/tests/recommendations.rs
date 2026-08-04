//! The personalised shelf, end to end through the real router.
//!
//! The model's own tests (`crates/recsys`, `services/control-plane/tests/recsys_build.rs`) prove
//! that a built index separates thematic clusters. What they cannot show is whether the *request
//! path* uses it correctly: that affinity is derived from the right rows, that the profile is
//! rebuilt when the watchlist moves, that the reader is never shown what they already track, and
//! that a refusal sticks. Every one of those is a seam between components that each pass their
//! own tests.
//!
//! Opt-in: gated behind the `integration` feature because it requires Docker.
#![cfg(feature = "integration")]

use axum::http::StatusCode;
use serde_json::json;
use tankovault_api_test_support::TestApp;
use tankovault_config::MatchingConfig;
use tankovault_control_plane::recsys::{BuildBudget, build};
use tankovault_db::repo::catalog::{ChapterUpsert, ScannedSeries, SeriesUpsert, ingest_series};
use tankovault_db::repo::tracking::watchlist_upsert;
use tankovault_domain::{
    AccountStatus, ContentType, ProviderId, SeriesId, SeriesStatus, UserId, WatchStatus,
    normalize_title,
};
use tankovault_test_support::seed;

fn budget() -> BuildBudget {
    BuildBudget {
        batch: 4,
        incremental_max: 100,
        dense_input_cap: 64,
        hnsw_m: 16,
        hnsw_ef_construction: 64,
    }
}

struct Fixture {
    title: &'static str,
    tags: &'static [&'static str],
    authors: &'static [&'static str],
}

const DUNGEON: &[Fixture] = &[
    Fixture {
        title: "Solo Leveling",
        tags: &["action", "dungeon", "regression", "leveling"],
        authors: &["chugong"],
    },
    Fixture {
        title: "The Gamer",
        tags: &["action", "dungeon", "leveling", "system"],
        authors: &["sung-sang-young"],
    },
    Fixture {
        title: "Tower of God",
        tags: &["action", "dungeon", "system", "tower"],
        authors: &["slime"],
    },
    Fixture {
        title: "Omniscient Reader",
        tags: &["action", "dungeon", "regression", "system"],
        authors: &["sing-shong"],
    },
];

const ROMANCE: &[Fixture] = &[
    Fixture {
        title: "Fruits Basket",
        tags: &["romance", "slice-of-life", "supernatural", "drama"],
        authors: &["natsuki-takaya"],
    },
    Fixture {
        title: "Kimi ni Todoke",
        tags: &["romance", "slice-of-life", "school", "drama"],
        authors: &["karuho-shiina"],
    },
    Fixture {
        title: "Ao Haru Ride",
        tags: &["romance", "school", "drama", "slice-of-life"],
        authors: &["io-sakisaka"],
    },
];

async fn ingest(app: &TestApp, provider: ProviderId, fixture: &Fixture) -> SeriesId {
    ingest_series(
        &app.db.pool,
        &ScannedSeries {
            provider_id: provider,
            source_path: format!("/s/{}", normalize_title(fixture.title).replace(' ', "-")),
            provider_title: Some(fixture.title.to_owned()),
            meta: SeriesUpsert {
                canonical_title: fixture.title.to_owned(),
                normalized_title: normalize_title(fixture.title),
                description: None,
                cover_url: None,
                content_type: ContentType::Manhwa,
                status: SeriesStatus::Ongoing,
                release_year: Some(2016),
            },
            alt_titles: Vec::new(),
            tags: fixture.tags.iter().map(|t| (*t).to_owned()).collect(),
            authors: fixture.authors.iter().map(|a| (*a).to_owned()).collect(),
            chapters: (1..=12)
                .map(|n| ChapterUpsert {
                    number: f64::from(n),
                    volume: None,
                    title: None,
                    path: format!("/c/{n}"),
                    published_at: None,
                })
                .collect(),
            content_hash: vec![1],
        },
        &MatchingConfig::default(),
    )
    .await
    .expect("ingest")
    .series_id
}

/// A catalogue, a built model, and a reader. Returns the two clusters and the reader.
async fn world(app: &TestApp) -> (Vec<SeriesId>, Vec<SeriesId>, UserId) {
    let provider = seed::provider(&app.db, "alpha").create().await;
    let mut dungeon = Vec::new();
    for fixture in DUNGEON {
        dungeon.push(ingest(app, provider, fixture).await);
    }
    let mut romance = Vec::new();
    for fixture in ROMANCE {
        romance.push(ingest(app, provider, fixture).await);
    }
    build(&app.db.pool, budget(), true).await.expect("build");
    let user = app.seed_user("reader", &[], AccountStatus::Active).await;
    (dungeon, romance, user)
}

async fn track(app: &TestApp, user: UserId, series: SeriesId, status: WatchStatus) {
    watchlist_upsert(&app.db.pool, user, series, status, true)
        .await
        .expect("watchlist");
}

fn ids(body: &serde_json::Value) -> Vec<String> {
    body.as_array()
        .expect("an array")
        .iter()
        .map(|item| item["id"].as_str().expect("id").to_owned())
        .collect()
}

/// **The shelf reflects what the reader finished, not what is popular.**
#[tokio::test]
async fn the_shelf_is_drawn_from_what_the_reader_has_read() {
    let app = TestApp::spawn().await;
    let (dungeon, romance, user) = world(&app).await;
    track(&app, user, dungeon[0], WatchStatus::Completed).await;

    let (status, body) = app
        .call(
            "GET",
            "/v1/me/recommendations",
            Some(&app.bearer(user)),
            None,
        )
        .await;
    assert_eq!(status, StatusCode::OK);

    let returned = ids(&body);
    assert!(!returned.is_empty(), "the shelf must not be empty");

    let dungeon_rest: Vec<String> = dungeon[1..].iter().map(SeriesId::to_string).collect();
    let romance_ids: Vec<String> = romance.iter().map(SeriesId::to_string).collect();
    let first_dungeon = returned.iter().position(|id| dungeon_rest.contains(id));
    let first_romance = returned.iter().position(|id| romance_ids.contains(id));
    assert!(
        first_dungeon.is_some(),
        "a reader who finished a dungeon series must be offered another"
    );
    assert!(
        first_dungeon < first_romance || first_romance.is_none(),
        "dungeon must outrank romance; got dungeon at {first_dungeon:?}, romance at {first_romance:?}"
    );
}

/// A reader is never shown what they already track.
///
/// Obvious, and the single most visible way a recommender can look broken — a "you might like"
/// rail full of things sitting in the reader's own watchlist.
#[tokio::test]
async fn the_shelf_never_contains_something_already_tracked() {
    let app = TestApp::spawn().await;
    let (dungeon, romance, user) = world(&app).await;
    for id in dungeon.iter().chain(romance.iter()) {
        track(&app, user, *id, WatchStatus::Reading).await;
    }

    let (status, body) = app
        .call(
            "GET",
            "/v1/me/recommendations",
            Some(&app.bearer(user)),
            None,
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        body.as_array().map(Vec::len),
        Some(0),
        "with the whole catalogue tracked there is nothing left to recommend"
    );
}

/// **A refusal sticks, and takes effect on the next request.**
///
/// The shelf is cached, so a dismissal that only wrote a row would keep showing the dismissed
/// series until the cache expired — up to six hours of the product ignoring an explicit "no".
#[tokio::test]
async fn dismissing_a_recommendation_removes_it_immediately() {
    let app = TestApp::spawn().await;
    let (dungeon, _, user) = world(&app).await;
    track(&app, user, dungeon[0], WatchStatus::Completed).await;
    let bearer = app.bearer(user);

    let (_, body) = app
        .call("GET", "/v1/me/recommendations", Some(&bearer), None)
        .await;
    let first = ids(&body);
    let dismissed = first.first().expect("a non-empty shelf").clone();

    let (status, _) = app
        .call(
            "POST",
            &format!("/v1/me/recommendations/{dismissed}/feedback"),
            Some(&bearer),
            Some(json!({ "verdict": "not_interested" })),
        )
        .await;
    assert_eq!(status, StatusCode::NO_CONTENT);

    let (_, body) = app
        .call("GET", "/v1/me/recommendations", Some(&bearer), None)
        .await;
    assert!(
        !ids(&body).contains(&dismissed),
        "the dismissed series must be gone on the very next request, not when the cache expires"
    );
}

#[tokio::test]
async fn an_unknown_verdict_is_refused() {
    let app = TestApp::spawn().await;
    let (dungeon, _, user) = world(&app).await;
    let (status, _) = app
        .call(
            "POST",
            &format!("/v1/me/recommendations/{}/feedback", dungeon[0]),
            Some(&app.bearer(user)),
            Some(json!({ "verdict": "maybe_later" })),
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}

/// **A watchlist change invalidates the profile.**
///
/// The invariant is enforced by a database trigger rather than by every write site remembering,
/// and this is what says the trigger is wired. Without it a reader's recommendations silently
/// stop reflecting what they read — no error, no symptom, just a shelf that stops moving.
#[tokio::test]
async fn tracking_a_new_series_reshapes_the_shelf() {
    let app = TestApp::spawn().await;
    let (dungeon, romance, user) = world(&app).await;
    let bearer = app.bearer(user);

    track(&app, user, romance[0], WatchStatus::Completed).await;
    let (_, body) = app
        .call("GET", "/v1/me/recommendations", Some(&bearer), None)
        .await;
    let romance_shelf = ids(&body);

    // Now the reader finishes a dungeon series as well; the shelf must move toward it.
    track(&app, user, dungeon[0], WatchStatus::Completed).await;
    let (_, body) = app
        .call("GET", "/v1/me/recommendations", Some(&bearer), None)
        .await;
    let mixed_shelf = ids(&body);

    assert_ne!(
        romance_shelf, mixed_shelf,
        "the cached shelf must not survive a watchlist change"
    );
    let dungeon_rest: Vec<String> = dungeon[1..].iter().map(SeriesId::to_string).collect();
    assert!(
        mixed_shelf.iter().any(|id| dungeon_rest.contains(id)),
        "after finishing a dungeon series the shelf must include others"
    );
}

/// A reader with no history still gets something: the catalogue's popularity prior.
#[tokio::test]
async fn a_reader_with_no_history_falls_back_to_the_prior() {
    let app = TestApp::spawn().await;
    let (_, _, user) = world(&app).await;

    let (status, body) = app
        .call(
            "GET",
            "/v1/me/recommendations",
            Some(&app.bearer(user)),
            None,
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        !ids(&body).is_empty(),
        "a cold reader must still be shown something rather than an empty rail"
    );
    assert!(
        body[0]["because_series_id"].is_null(),
        "the prior explains nothing, and must not claim to"
    );
}

/// The taste profile is inspectable by the person it describes.
#[tokio::test]
async fn the_reader_can_read_their_own_profile() {
    let app = TestApp::spawn().await;
    let (dungeon, _, user) = world(&app).await;
    track(&app, user, dungeon[0], WatchStatus::Completed).await;

    let (status, body) = app
        .call("GET", "/v1/me/taste", Some(&app.bearer(user)), None)
        .await;
    assert_eq!(status, StatusCode::OK);

    let likes = body["likes"].as_array().expect("likes");
    assert!(!likes.is_empty(), "a reader with history has a profile");
    let values: Vec<&str> = likes.iter().filter_map(|f| f["value"].as_str()).collect();
    assert!(
        values.contains(&"dungeon"),
        "the profile must name what the reader actually reads, got {values:?}"
    );
    assert_eq!(
        body["seeds"].as_array().map(Vec::len),
        Some(1),
        "one completed series is one seed"
    );
}

/// **Dropping a series must not make it a seed.**
///
/// "More like this" pointed at something the reader abandoned is the most obviously wrong thing a
/// recommender can do, and it is what happens if seeds are taken by absolute affinity — which is
/// the correct ordering for building the *profile*, and the wrong one for choosing seeds.
#[tokio::test]
async fn a_dropped_series_never_becomes_a_seed() {
    let app = TestApp::spawn().await;
    let (dungeon, romance, user) = world(&app).await;
    // Dropped at chapter 1: the strongest negative the model can express.
    track(&app, user, romance[0], WatchStatus::Dropped).await;
    track(&app, user, dungeon[0], WatchStatus::Completed).await;

    let (_, body) = app
        .call("GET", "/v1/me/taste", Some(&app.bearer(user)), None)
        .await;
    let seeds: Vec<&str> = body["seeds"]
        .as_array()
        .expect("seeds")
        .iter()
        .filter_map(serde_json::Value::as_str)
        .collect();
    assert!(
        !seeds.contains(&romance[0].to_string().as_str()),
        "an abandoned series must never seed a recommendation"
    );
    assert!(
        seeds.contains(&dungeon[0].to_string().as_str()),
        "the completed series must seed"
    );

    let avoids: Vec<&str> = body["avoids"]
        .as_array()
        .expect("avoids")
        .iter()
        .filter_map(|f| f["value"].as_str())
        .collect();
    assert!(
        avoids.contains(&"romance"),
        "what the reader dropped must show up as something they avoid, got {avoids:?}"
    );
}
