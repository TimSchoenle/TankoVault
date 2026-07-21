//! Provider registry contract (design: generalized multi-provider sync).
//!
//! Every external tracker (`AniList` today; a second provider is a drop-in later) implements
//! [`ExternalProvider`] and is registered under its stable [`ExternalProvider::slug`] in
//! [`crate::engine::SyncEngine`]. Status crosses this boundary as the shared
//! [`WatchStatus`] — each provider owns translating its own status vocabulary to/from it (see
//! `AniListStatus` in `crate::mapping`), so the engine never touches provider-specific enums.

use async_trait::async_trait;
use serde::Serialize;
use tankovault_domain::{ContentType, WatchStatus};
use time::OffsetDateTime;

/// Tokens returned by an `OAuth2` code exchange or refresh.
#[derive(Debug, Clone)]
pub(crate) struct OAuthTokens {
    pub(crate) access_token: String,
    pub(crate) refresh_token: Option<String>,
    pub(crate) expires_at: Option<OffsetDateTime>,
}

/// The authenticated account behind an access token. `id` is the provider's own opaque account
/// id, stringified at this boundary so the engine never needs provider-specific id types.
#[derive(Debug, Clone)]
pub(crate) struct Viewer {
    pub(crate) id: String,
    pub(crate) name: String,
}

/// One remote list entry, normalised for local matching. `status` and `external_id` are
/// already translated/stringified by the provider that produced this.
#[derive(Debug, Clone)]
pub(crate) struct RemoteEntry {
    pub(crate) external_id: String,
    pub(crate) titles: Vec<String>,
    pub(crate) status: WatchStatus,
    pub(crate) progress: f64,
    pub(crate) updated_at: OffsetDateTime,
    pub(crate) start_year: Option<i32>,
    pub(crate) content_type: ContentType,
}

/// A registered provider's identity, as listed by `GET /v1/sync/providers`.
#[derive(Debug, Clone, Serialize)]
pub(crate) struct ProviderInfo {
    pub(crate) slug: &'static str,
    pub(crate) name: &'static str,
}

/// An external tracker `SyncEngine` can link, pull from and push to. Implementors are stored as
/// `Box<dyn ExternalProvider>` in the engine's registry (dyn-safe: matches the existing
/// `SourceAdapter`/`ChallengeSolver` `#[async_trait]` dyn-trait precedent elsewhere in this
/// workspace).
#[async_trait]
pub(crate) trait ExternalProvider: Send + Sync {
    /// Stable key stored in `external_accounts.provider` / `sync_mappings.provider` (e.g.
    /// `"anilist"`).
    fn slug(&self) -> &'static str;
    /// User-facing display name (e.g. `"AniList"`).
    fn display_name(&self) -> &'static str;
    /// The OAuth consent URL to redirect a user to.
    fn authorize_url(&self) -> String;
    /// Exchange an OAuth `code` for tokens.
    async fn exchange_code(&self, code: &str) -> anyhow::Result<OAuthTokens>;
    /// Refresh an expired access token.
    async fn refresh(&self, refresh_token: &str) -> anyhow::Result<OAuthTokens>;
    /// The authenticated viewer behind `access_token`.
    async fn viewer(&self, access_token: &str) -> anyhow::Result<Viewer>;
    /// The viewer's full remote list.
    async fn fetch_list(
        &self,
        access_token: &str,
        viewer: &Viewer,
    ) -> anyhow::Result<Vec<RemoteEntry>>;
    /// Search for a remote entry by title, returning its external id if found.
    async fn search(&self, access_token: &str, title: &str) -> anyhow::Result<Option<String>>;
    /// Create or update a remote list entry.
    async fn save_entry(
        &self,
        access_token: &str,
        external_id: &str,
        status: WatchStatus,
        progress: f64,
    ) -> anyhow::Result<()>;
}
