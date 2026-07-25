//! Provider registry contract (design: generalized multi-provider sync).
//!
//! Every external tracker (`AniList` today; a second provider is a drop-in later) implements
//! [`ExternalProvider`] and is registered under its stable [`ExternalProvider::slug`] in
//! [`crate::engine::SyncEngine`]. Status crosses this boundary as the shared
//! [`WatchStatus`] — each provider owns translating its own status vocabulary to/from it (see
//! `AniListStatus` in `crate::mapping`), so the engine never touches provider-specific enums.

use async_trait::async_trait;
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
    /// Genres, matched against a local series' tags for extra matching confidence.
    pub(crate) tags: Vec<String>,
    /// Story/art staff names, matched against a local series' authors.
    pub(crate) authors: Vec<String>,
}

/// Public catalogue metadata for one remote work, fetched **without** a user token from a
/// provider's public API. This is what the tokenless enrichment worker uses to fill in a
/// local series' description, cover, alternative titles, genres and credits.
#[derive(Debug, Clone)]
pub(crate) struct RemoteMetadata {
    pub(crate) external_id: String,
    /// Candidate titles (primary first, then every alternative/synonym), non-blank only.
    pub(crate) titles: Vec<String>,
    pub(crate) description: Option<String>,
    pub(crate) cover_url: Option<String>,
    pub(crate) start_year: Option<i32>,
    pub(crate) content_type: ContentType,
    pub(crate) tags: Vec<String>,
    pub(crate) authors: Vec<String>,
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

    /// Whether this provider exposes a public (token-free) metadata API that the enrichment
    /// worker can use. Defaults to `false`; providers that support it override this.
    fn supports_public_metadata(&self) -> bool {
        false
    }

    /// Fetch public catalogue metadata for a work by title, **without** any user token.
    /// Returns `Ok(None)` when nothing matches (or the provider has no public API).
    async fn fetch_public_metadata_by_title(
        &self,
        _title: &str,
    ) -> anyhow::Result<Option<RemoteMetadata>> {
        Ok(None)
    }

    /// Fetch public catalogue metadata for a known external id, **without** any user token.
    async fn fetch_public_metadata_by_id(
        &self,
        _external_id: &str,
    ) -> anyhow::Result<Option<RemoteMetadata>> {
        Ok(None)
    }

    /// Create or update a remote list entry.
    async fn save_entry(
        &self,
        access_token: &str,
        external_id: &str,
        status: WatchStatus,
        progress: f64,
    ) -> anyhow::Result<()>;
}
