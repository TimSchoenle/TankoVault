//! `GET /v1/branding`, over the real router.
//!
//! # What these pin
//!
//! **That an operator's identity actually reaches the client.** Every surface that used to spell
//! this project's name out now renders what this endpoint returns, so a field that silently kept
//! its shipped value would put the wrong name on a fork's sign-in screen with nothing failing.
//!
//! **That it answers without a token.** The sign-in card draws the wordmark and the footer; a
//! client that had to authenticate first would show the shipped identity to exactly the readers
//! who have not signed in yet.
//!
//! Gated behind the `integration` feature because they require Docker.
#![cfg(feature = "integration")]

use axum::http::StatusCode;
use tankovault_api_test_support::{TestApp, TestConfig};
use tankovault_config::{BrandingConfig, CopyrightConfig, LicenceConfig, WordmarkConfig};

#[tokio::test]
async fn an_unconfigured_deployment_publishes_the_shipped_identity() {
    let app = TestApp::spawn_with(TestConfig::new()).await;

    let (status, body) = app.call("GET", "/v1/branding", None, None).await;
    assert_eq!(
        status,
        StatusCode::OK,
        "the identity must not require a token"
    );
    assert_eq!(body["name"], "TankoVault");
    assert_eq!(body["wordmark"]["lead"], "Tankō");
    assert_eq!(body["wordmark"]["accent"], "Vault");
    assert_eq!(body["licence"]["name"], "PolyForm Noncommercial 1.0.0");
    assert_eq!(body["copyright"]["holder"], "Tim Schönle");

    // Unset, so the server resolves it rather than serving an empty string the footer would
    // print as `© ` with nothing after it.
    let year = body["copyright"]["year"].as_str().expect("a year");
    assert_eq!(year.len(), 4, "got {year}");
    assert!(year.chars().all(|c| c.is_ascii_digit()), "got {year}");
}

/// Every field an operator can set has to arrive changed. A view that forwarded some fields and
/// defaulted others would pass a spot check on `name` while leaving this project's licence and
/// copyright on a fork's footer.
#[tokio::test]
async fn a_rebranded_deployment_publishes_its_own_identity() {
    let branding = BrandingConfig {
        name: "MangaBox".to_owned(),
        wordmark: WordmarkConfig {
            lead: Some("Manga".to_owned()),
            accent: Some("Box".to_owned()),
        },
        tagline: Some("everything you read, in one place".to_owned()),
        copyright: CopyrightConfig {
            holder: "Example Collective".to_owned(),
            year: Some("2024–2026".to_owned()),
            notice: None,
        },
        licence: LicenceConfig {
            name: "AGPL-3.0-or-later".to_owned(),
            url: Some("https://mangabox.example/licence".to_owned()),
        },
        project_url: "https://git.example/mangabox".to_owned(),
        releases_url: "https://git.example/mangabox/releases".to_owned(),
        bot_user_agent: Some("MangaBoxBot/1.0".to_owned()),
    };
    let app = TestApp::spawn_with(TestConfig::new().with_branding(branding)).await;

    let (status, body) = app.call("GET", "/v1/branding", None, None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["name"], "MangaBox");
    assert_eq!(body["wordmark"]["lead"], "Manga");
    assert_eq!(body["wordmark"]["accent"], "Box");
    assert_eq!(body["tagline"], "everything you read, in one place");
    assert_eq!(body["copyright"]["holder"], "Example Collective");
    assert_eq!(body["copyright"]["year"], "2024–2026");
    assert_eq!(body["licence"]["name"], "AGPL-3.0-or-later");
    assert_eq!(body["licence"]["url"], "https://mangabox.example/licence");
    assert_eq!(body["project_url"], "https://git.example/mangabox");
    assert_eq!(
        body["releases_url"],
        "https://git.example/mangabox/releases"
    );

    // The crawler identity is the worker's business and names hosts this deployment crawls;
    // publishing it to every reader would be a detail nobody asked this endpoint for.
    assert!(
        !body.to_string().contains("MangaBoxBot"),
        "the crawler user-agent must not be published to clients: {body}"
    );
}

/// A deployment whose name is not two words gets its name whole, not this project's accent half
/// left standing beside it.
#[tokio::test]
async fn a_rename_without_a_split_draws_one_word() {
    let branding = BrandingConfig {
        name: "Shelf".to_owned(),
        ..BrandingConfig::default()
    };
    let app = TestApp::spawn_with(TestConfig::new().with_branding(branding)).await;

    let (_, body) = app.call("GET", "/v1/branding", None, None).await;
    assert_eq!(body["wordmark"]["lead"], "Shelf");
    assert!(body["wordmark"]["accent"].is_null(), "{body}");
}
