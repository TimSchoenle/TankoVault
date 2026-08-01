//! `AniList` `OAuth2` + GraphQL provider (design §15).
//!
//! | module | owns |
//! |---|---|
//! | [`client`] | the HTTP client, `OAuth2`, and the paced + `429`-retried GraphQL round trip |
//! | [`graphql`] | the query/mutation documents and the typed operations that send them |
//! | [`parse`] | the `AniList`-shaped types and the pure JSON → type functions, with the tests |
//!
//! This file holds only the endpoint defaults and the [`ExternalProvider`] impl — the boundary
//! at which `AniList`'s numeric ids and its own status vocabulary become the shared
//! `RemoteEntry`/`WatchStatus` types the engine sees.

mod client;
mod graphql;
mod parse;

use anyhow::Context;
use async_trait::async_trait;
use secrecy::SecretString;

use tankovault_domain::WatchStatus;

use crate::mapping::{AniListStatus, progress_to_int};
use crate::provider::{ExternalProvider, OAuthTokens, RemoteEntry, RemoteMetadata, Viewer};

pub(crate) use client::AniListClient;

/// Default `AniList` GraphQL endpoint.
pub(crate) const DEFAULT_GRAPHQL_URL: &str = "https://graphql.anilist.co";
/// Default `AniList` OAuth base (authorize + token live under here).
pub(crate) const DEFAULT_OAUTH_BASE: &str = "https://anilist.co/api/v2/oauth";
/// The provider key used in `external_accounts` / `sync_mappings`.
pub(crate) const PROVIDER: &str = "anilist";

#[async_trait]
impl ExternalProvider for AniListClient {
    fn slug(&self) -> &'static str {
        PROVIDER
    }

    fn display_name(&self) -> &'static str {
        "AniList"
    }

    fn authorize_url(&self) -> String {
        self.authorize_url()
    }

    async fn exchange_code(&self, code: &str) -> anyhow::Result<OAuthTokens> {
        self.exchange_code(code).await
    }

    async fn refresh(&self, refresh_token: &SecretString) -> anyhow::Result<OAuthTokens> {
        self.refresh(refresh_token).await
    }

    async fn viewer(&self, access_token: &SecretString) -> anyhow::Result<Viewer> {
        self.viewer(access_token).await
    }

    async fn fetch_list(
        &self,
        access_token: &SecretString,
        viewer: &Viewer,
    ) -> anyhow::Result<Vec<RemoteEntry>> {
        let user_id: i64 = viewer
            .id
            .parse()
            .context("AniList viewer id was not numeric")?;
        Ok(self
            .fetch_media_list(access_token, user_id)
            .await?
            .into_iter()
            .map(Into::into)
            .collect())
    }

    async fn search(
        &self,
        access_token: &SecretString,
        title: &str,
    ) -> anyhow::Result<Option<String>> {
        Ok(self
            .search_media(access_token, title)
            .await?
            .map(|id| id.to_string()))
    }

    fn supports_public_metadata(&self) -> bool {
        true
    }

    async fn fetch_public_metadata_by_title(
        &self,
        title: &str,
    ) -> anyhow::Result<Option<RemoteMetadata>> {
        Ok(self.fetch_metadata_by_title(title).await?.map(Into::into))
    }

    async fn fetch_public_metadata_by_id(
        &self,
        external_id: &str,
    ) -> anyhow::Result<Option<RemoteMetadata>> {
        let media_id: i64 = external_id
            .parse()
            .context("AniList external id was not numeric")?;
        Ok(self.fetch_metadata_by_id(media_id).await?.map(Into::into))
    }

    async fn save_entry(
        &self,
        access_token: &SecretString,
        external_id: &str,
        status: WatchStatus,
        progress: f64,
    ) -> anyhow::Result<()> {
        let media_id: i64 = external_id
            .parse()
            .context("AniList external id was not numeric")?;
        self.save_entry(
            access_token,
            media_id,
            AniListStatus::from_watch_status(status),
            progress_to_int(progress),
        )
        .await
    }
}
