//! `GET /v1/series` paging headers: `X-Total-Count` on request, `X-Next-Cursor` from the page.

#![cfg(feature = "integration")]

use axum::body::Body;
use axum::http::{Request, StatusCode};
use tankovault_api_test_support::{TestApp, TestConfig};
use tankovault_test_support::seed;

/// The two paging headers of one browse request, as `(X-Total-Count, X-Next-Cursor)`.
async fn paging_headers(app: &TestApp, query: &str) -> (Option<i64>, Option<i64>) {
    let request = Request::builder()
        .method("GET")
        .uri(format!("/v1/series?{query}"))
        .body(Body::empty())
        .expect("build request");
    let response = app.request(request).await;
    assert_eq!(response.status(), StatusCode::OK, "{query}");
    let header = |name: &str| {
        response
            .headers()
            .get(name)
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.parse::<i64>().ok())
    };
    (header("x-total-count"), header("x-next-cursor"))
}

/// **The next-page cursor is right with and without a count, at every page boundary.**
///
/// `X-Next-Cursor` used to be derived from the total, so a request that skips the count
/// (`with_total=false`, which Discover sends for every page after a window's first) would have
/// ended the list one page early or offered an empty page when the matches were an exact multiple
/// of the page size. Without the parameter both headers must behave exactly as before, because
/// installed desktop builds read the total on every page.
#[tokio::test]
async fn the_next_cursor_does_not_depend_on_the_count() {
    let app = TestApp::spawn_with(TestConfig::new().without_rate_limiting()).await;
    let provider = seed::provider(&app.db, "alpha").create().await;
    for title in ["Berserk", "Frieren", "Monster", "Pluto", "Vagabond"] {
        seed::series(&app.db, provider, title)
            .chapters(&[1.0])
            .create()
            .await;
    }

    for (query, want) in [
        ("limit=2&page=0", (Some(5), Some(1))),
        ("limit=2&page=1", (Some(5), Some(2))),
        ("limit=2&page=2", (Some(5), None)),
        ("limit=5&page=0", (Some(5), None)),
        ("limit=2&page=0&with_total=false", (None, Some(1))),
        ("limit=2&page=2&with_total=false", (None, None)),
        ("limit=5&page=0&with_total=false", (None, None)),
        ("limit=2&page=1&with_total=true", (Some(5), Some(2))),
    ] {
        assert_eq!(paging_headers(&app, query).await, want, "{query}");
    }
}
