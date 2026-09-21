//! The legal endpoints, over the real router.
//!
//! # What these pin
//!
//! **That they answer without a token.** Registering is the act of accepting the Terms, so the
//! register form has to link them to a reader who by definition has no account yet. Putting
//! `/v1/legal` behind the same auth layer as the rest of `/v1` is a one-line mistake that every
//! other test in this suite would pass, because everywhere else a `401` is the correct answer.
//!
//! Gated behind the `integration` feature because they require Docker.
#![cfg(feature = "integration")]

use std::collections::BTreeMap;

use axum::body::Body;
use axum::http::header::{ACCEPT_LANGUAGE, ETAG, IF_NONE_MATCH, VARY};
use axum::http::{Request, StatusCode};
use tankovault_api_test_support::{TestApp, TestConfig};
use terrace_legal::{LegalConfig, LegalDocument};

/// Two locales of the Terms, served here, and an Imprint hosted elsewhere.
fn config() -> LegalConfig {
    let terms = LegalDocument {
        body: BTreeMap::from([
            (
                "de".to_owned(),
                "# Bedingungen\n\nDeutscher Text.\n".to_owned(),
            ),
            ("en".to_owned(), "# Terms\n\nEnglish body.\n".to_owned()),
        ]),
        updated: Some("2026-08-04".to_owned()),
        title: BTreeMap::from([("en".to_owned(), "Terms of Service".to_owned())]),
        ..LegalDocument::default()
    };
    let imprint = LegalDocument {
        url: Some("https://example.org/impressum".to_owned()),
        ..LegalDocument::default()
    };
    LegalConfig {
        documents: BTreeMap::from([("terms".to_owned(), terms), ("imprint".to_owned(), imprint)]),
        ..LegalConfig::default()
    }
}

async fn app() -> TestApp {
    TestApp::spawn_with(TestConfig::new().with_legal(config())).await
}

fn get(uri: &str) -> axum::http::request::Builder {
    Request::builder().method("GET").uri(uri)
}

#[tokio::test]
async fn the_documents_are_readable_without_an_account() {
    let app = app().await;

    let (status, body) = app.call("GET", "/v1/legal", None, None).await;
    assert_eq!(status, StatusCode::OK, "the index must not require a token");
    let entries = body.as_array().expect("an array of entries");
    assert_eq!(entries.len(), 2);

    let terms = entries
        .iter()
        .find(|e| e["slug"] == "terms")
        .expect("terms is published");
    assert_eq!(terms["kind"], "inline");
    assert_eq!(terms["updated"], "2026-08-04");
    assert_eq!(terms["locales"], serde_json::json!(["de", "en"]));

    // An externally hosted document appears with somewhere to go and nothing to serve.
    let imprint = entries
        .iter()
        .find(|e| e["slug"] == "imprint")
        .expect("imprint is published");
    assert_eq!(imprint["kind"], "external");
    assert_eq!(imprint["url"], "https://example.org/impressum");

    let (status, body) = app.call("GET", "/v1/legal/terms?lang=en", None, None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["locale"], "en");
    assert_eq!(body["format"], "markdown");
    assert_eq!(body["title"], "Terms of Service");
    assert!(
        body["body"]
            .as_str()
            .is_some_and(|b| b.contains("English body")),
        "the Markdown is served verbatim: {body}"
    );
}

/// A locale the operator did not publish falls back to one they did — and the response says
/// which, so the page can tell the reader rather than let them conclude the operator writes
/// their language like that.
#[tokio::test]
async fn an_unpublished_locale_falls_back_and_the_response_names_what_it_served() {
    let app = app().await;

    let (status, body) = app.call("GET", "/v1/legal/terms?lang=fr", None, None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        body["locale"], "de",
        "the first configured locale, stated in the answer"
    );
}

/// The title follows the body's locale.
///
/// The bug: the title had its own fallback chain, so a German reader of the only-English-titled
/// Terms got the German body under the English title "Terms of Service". Now a body served in
/// `de` carries the `de` title or none, and the client names it from its own catalogue.
#[tokio::test]
async fn the_title_is_never_in_another_language_than_the_body() {
    let app = app().await;

    let (status, body) = app.call("GET", "/v1/legal/terms?lang=de", None, None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["locale"], "de");
    assert_eq!(body["title"], serde_json::Value::Null);
}

/// A repeat request with the tag it was given is a `304`, and both answers say they vary by
/// `Accept-Language`.
///
/// The bug: the old handler sent an `ETag` it never compared, so every footer render re-fetched
/// the whole document, and it sent no `Vary`, so a shared cache could hand a German reader the
/// copy it negotiated for an English one.
#[tokio::test]
async fn a_matching_if_none_match_is_not_modified_and_every_answer_varies_by_language() {
    let app = app().await;

    let first = app
        .request(
            get("/v1/legal/terms")
                .header(ACCEPT_LANGUAGE, "de-AT, en;q=0.5")
                .body(Body::empty())
                .expect("request"),
        )
        .await;
    assert_eq!(first.status(), StatusCode::OK);
    assert_eq!(first.headers()[VARY], "Accept-Language");
    let etag = first.headers()[ETAG].clone();

    let second = app
        .request(
            get("/v1/legal/terms")
                .header(ACCEPT_LANGUAGE, "de-AT, en;q=0.5")
                .header(IF_NONE_MATCH, etag.clone())
                .body(Body::empty())
                .expect("request"),
        )
        .await;
    assert_eq!(second.status(), StatusCode::NOT_MODIFIED);
    assert_eq!(second.headers()[ETAG], etag);
    assert_eq!(second.headers()[VARY], "Accept-Language");

    let index = app
        .request(get("/v1/legal").body(Body::empty()).expect("request"))
        .await;
    assert_eq!(index.status(), StatusCode::OK);
    assert_eq!(index.headers()[VARY], "Accept-Language");
    assert!(index.headers().contains_key(ETAG));
}

/// An unconfigured slug is a 404 — the footer only ever links what the index returned, so this
/// is a hand-typed URL or a stale bookmark, not a broken link the app published. An external
/// document has no body here, so it is one too, in the same problem shape.
#[tokio::test]
async fn an_unconfigured_slug_or_an_external_document_is_not_found() {
    let app = app().await;

    for uri in ["/v1/legal/dmca", "/v1/legal/imprint", "/v1/legal/..%2Fetc"] {
        let (status, _) = app.call("GET", uri, None, None).await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{uri}");
    }
}

/// The common deployment: no `[legal]` section at all. An empty index, not an error — the
/// footer then publishes no Legal column and the register form omits its acceptance line.
#[tokio::test]
async fn an_instance_that_publishes_nothing_answers_with_an_empty_index() {
    let app = TestApp::spawn().await;

    let (status, body) = app.call("GET", "/v1/legal", None, None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body.as_array().map(Vec::len), Some(0));
}
