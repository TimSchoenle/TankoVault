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
use std::path::PathBuf;

use axum::http::StatusCode;
use tankovault_api_test_support::{TestApp, TestConfig};
use tankovault_config::{LegalConfig, LegalDocument};

/// A scratch directory holding two locales of one document, removed on drop.
struct Fixture {
    dir: PathBuf,
}

impl Fixture {
    fn new(name: &str) -> Self {
        let dir = std::env::temp_dir().join(format!("tv-legal-{name}-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("scratch dir");
        std::fs::write(dir.join("terms.en.md"), "# Terms\n\nEnglish body.\n").expect("en");
        std::fs::write(
            dir.join("terms.de.md"),
            "# Bedingungen\n\nDeutscher Text.\n",
        )
        .expect("de");
        Self { dir }
    }

    fn config(&self) -> LegalConfig {
        let terms = LegalDocument {
            sources: BTreeMap::from([
                ("de".to_owned(), PathBuf::from("terms.de.md")),
                ("en".to_owned(), PathBuf::from("terms.en.md")),
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
            dir: Some(self.dir.clone()),
            documents: BTreeMap::from([
                ("terms".to_owned(), terms),
                ("imprint".to_owned(), imprint),
            ]),
        }
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.dir).ok();
    }
}

#[tokio::test]
async fn the_documents_are_readable_without_an_account() {
    let fixture = Fixture::new("public");
    let app = TestApp::spawn_with(TestConfig::new().with_legal(fixture.config())).await;

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
    let fixture = Fixture::new("locale");
    let app = TestApp::spawn_with(TestConfig::new().with_legal(fixture.config())).await;

    let (status, body) = app.call("GET", "/v1/legal/terms?lang=fr", None, None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        body["locale"], "de",
        "the first configured locale, stated in the answer"
    );
}

/// An unconfigured slug is a 404 — the footer only ever links what the index returned, so this
/// is a hand-typed URL or a stale bookmark, not a broken link the app published.
#[tokio::test]
async fn an_unconfigured_slug_is_not_found() {
    let fixture = Fixture::new("missing");
    let app = TestApp::spawn_with(TestConfig::new().with_legal(fixture.config())).await;

    let (status, _) = app.call("GET", "/v1/legal/dmca", None, None).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
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
