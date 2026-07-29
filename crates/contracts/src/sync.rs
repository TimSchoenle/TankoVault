//! The HTTP wire contract for the external-sync surface.
//!
//! These are the response bodies `services/sync` produces on its `/v1/sync/*` routes and
//! `services/api` re-publishes under `/v1/me/sync/*`. They live here, rather than privately
//! in the sync service, for one reason: `services/api` proxies those routes verbatim and so
//! cannot describe them from its own types. Without a shared definition its `#[utoipa::path]`
//! annotations had no `body`, the generated client had no methods for them, and the frontend
//! carried hand-written mirror structs that quietly drifted out of shape — silently dropping
//! the connected display name, the last-sync time and every persisted auto-sync setting.
//!
//! Because the producer returns these types and the API's schema annotations reference the
//! same ones, the generated client and the real payload cannot disagree.

use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

/// Whether a user has linked a provider and, when linked, who they are connected as.
///
/// Always `200`: an unlinked account reads `{ "linked": false }` rather than 404ing, so the
/// UI can render "not connected" without treating it as an error.
///
/// Published as `SyncAccountStatus` rather than under its Rust name: `tankovault_domain`
/// has an `AccountStatus` too (whether a *user account* is active or suspended), and two
/// unrelated types sharing one `OpenAPI` component name means the last one registered silently
/// replaces the other — which is exactly what happened when the domain enum was introduced.
/// The qualifier is on this one because it is the more specific of the two: an
/// external-tracker link status, not an account's own state.
#[derive(Debug, Clone, Default, Serialize, Deserialize, ToSchema)]
#[schema(as = SyncAccountStatus)]
pub struct AccountStatus {
    /// Whether a linked external account exists for this user and provider.
    pub linked: bool,
    /// The connected account's display name at the provider, when linked.
    #[serde(default)]
    pub username: Option<String>,
    /// RFC-3339 timestamp of the most recent successful sync, when there has been one.
    #[serde(default)]
    pub last_synced_at: Option<String>,
}

/// An account's persisted automatic-sync settings (design v2 §B.6/§B.8).
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct AccountSettings {
    /// Whether a linked external account exists; the other fields are inert without one.
    pub linked: bool,
    /// Whether the background engine syncs this account without being asked.
    pub auto_sync_enabled: bool,
    /// How to settle a local/remote disagreement — one of `local_wins`, `remote_wins`,
    /// `newest_wins`, `ask_me`.
    pub conflict_policy: String,
    /// Conflicts awaiting the user's decision, i.e. the badge count on the Sync panel.
    pub pending_conflicts: i64,
}

/// A registered external provider's identity, as listed by `GET /v1/me/sync/providers`.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct ProviderInfo {
    /// Stable key used in sync paths and mapping keys (e.g. `anilist`).
    pub slug: String,
    /// User-facing display name (e.g. `AniList`).
    pub name: String,
}

/// The provider's OAuth consent URL, for `GET /v1/me/sync/{provider}/authorize`.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct AuthorizeUrl {
    pub url: String,
}

/// One pending conflict awaiting the user's decision (design v2 §B.6 `GET /v1/me/sync/conflicts`).
///
/// Produced by `services/sync` from a repository row and re-published verbatim by
/// `services/api`. It lives here rather than on the row struct for the reason given in
/// [`crate::admin`]: a `SELECT` column rename must not be able to rewrite the public API
/// without a compile error. The published component name is pinned to `ConflictRow` — the
/// move is an internal layering fix and must not rename anything on the wire.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[schema(as = ConflictRow)]
pub struct ConflictView {
    pub id: uuid::Uuid,
    pub series_id: uuid::Uuid,
    pub series_title: String,
    pub provider: String,
    /// Which tracked field disagrees, e.g. `progress` or `status`.
    pub field: String,
    pub local_value: String,
    pub remote_value: String,
    #[serde(with = "time::serde::rfc3339")]
    #[schema(value_type = String)]
    pub detected_at: time::OffsetDateTime,
}

/// One row of the user-facing sync history (design v2 §B.6 `GET /v1/me/sync/history`).
/// See [`ConflictView`] for why it lives here.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[schema(as = HistoryRow)]
pub struct HistoryView {
    pub id: uuid::Uuid,
    pub series_id: uuid::Uuid,
    pub series_title: String,
    pub provider: String,
    /// What the engine did, e.g. `pull`, `push` or `resolve`.
    pub action: String,
    /// Free-form, action-specific detail (the changed field and its before/after values).
    #[schema(value_type = Object)]
    pub detail: serde_json::Value,
    #[serde(with = "time::serde::rfc3339")]
    #[schema(value_type = String)]
    pub created_at: time::OffsetDateTime,
}
