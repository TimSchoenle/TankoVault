//! The HTTP wire contract for the external-sync surface.
//!
//! Response bodies `services/sync` produces on `/v1/sync/*` and `services/api` re-publishes
//! verbatim under `/v1/me/sync/*`. Shared here so both services' `#[utoipa::path]` annotations
//! reference the same types instead of the frontend hand-mirroring structs that drift.

use serde::{Deserialize, Serialize};
use tankovault_domain::Feature;
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

/// How to settle a local/remote disagreement when a series exists on both sides
/// (design v2 §B.3).
///
/// # Why this is here rather than in `services/sync`
///
/// It was a `pub(crate)` enum in the sync service and a **bare string** on the wire, which put
/// the vocabulary in three places that nothing connected: the service's enum, a prose list in
/// this file's doc comment, and a closed enumeration the frontend maintained by hand
/// (FRONTEND F10). A policy added to the service would have compiled everywhere and then
/// silently failed to appear in the picker; a token misspelled in the frontend would have been
/// rejected by the service at the far end of a round trip, if at all. Declaring it once here —
/// where `utoipa` publishes it, `progenitor` generates it and the frontend consumes the
/// generated form — makes the compiler the connection in both directions.
///
/// The JSON representation is unchanged: `snake_case`, the same four tokens the wire always
/// carried. What changed is that the *schema* now says so, so an unknown token is a `422` at
/// the edge instead of a value that reaches the merge and is quietly read as `newest_wins`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum ConflictPolicy {
    /// Local progress/status is authoritative.
    LocalWins,
    /// The remote (`AniList`) value is authoritative.
    RemoteWins,
    /// Whichever side was updated most recently wins.
    #[default]
    NewestWins,
    /// Genuine conflicts are queued for the user to resolve rather than auto-picked.
    AskMe,
}

impl ConflictPolicy {
    /// Every policy, in the order a picker should offer them.
    ///
    /// Hand-listed because Rust cannot enumerate an enum's variants without a derive macro.
    /// [`ConflictPolicy::as_str`]'s exhaustive `match` forces a new variant to be added here to
    /// compile, and `every_policy_is_listed_once_and_round_trips` catches one missing from it.
    pub const ALL: [Self; 4] = [
        Self::LocalWins,
        Self::RemoteWins,
        Self::NewestWins,
        Self::AskMe,
    ];

    /// The wire and persistence token, identical to the `serde` representation.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::LocalWins => "local_wins",
            Self::RemoteWins => "remote_wins",
            Self::NewestWins => "newest_wins",
            Self::AskMe => "ask_me",
        }
    }
}

impl std::fmt::Display for ConflictPolicy {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// A policy token that names nothing.
///
/// Carries the offending token: the value comes from a database column or a request body, so
/// the operator reading the log needs to know *which* string was rejected.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnknownConflictPolicy(pub String);

impl std::fmt::Display for UnknownConflictPolicy {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "unknown conflict policy `{}`; expected one of {}",
            self.0,
            ConflictPolicy::ALL
                .iter()
                .map(|p| p.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        )
    }
}

impl std::error::Error for UnknownConflictPolicy {}

impl std::str::FromStr for ConflictPolicy {
    type Err = UnknownConflictPolicy;

    /// Derived from [`ConflictPolicy::ALL`] and [`ConflictPolicy::as_str`] rather than written
    /// as a second `match`, so parsing is the exact inverse of rendering by construction — a
    /// hand-written `match` with a `_ => NewestWins` fallback previously let a typo silently
    /// become a policy change instead of an error.
    fn from_str(token: &str) -> Result<Self, Self::Err> {
        Self::ALL
            .into_iter()
            .find(|p| p.as_str() == token)
            .ok_or_else(|| UnknownConflictPolicy(token.to_owned()))
    }
}

/// An account's persisted automatic-sync settings (design v2 §B.6/§B.8).
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct AccountSettings {
    /// Whether a linked external account exists; the other fields are inert without one.
    pub linked: bool,
    /// Whether the background engine syncs this account without being asked.
    pub auto_sync_enabled: bool,
    /// How to settle a local/remote disagreement.
    pub conflict_policy: ConflictPolicy,
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

/// Acknowledgement for a sync route that answers `204 No Content`.
///
/// `services/sync` returns a bare `204` for linking an account and for patching settings;
/// `services/api` republishes those as `200 {"ok": true}` because its `Upstream::decode`
/// synthesises this body for an empty upstream response. That synthesis is the contract, not
/// an accident: naming it here means a change to either side breaks a type rather than
/// silently altering what the SPA receives. `ok` is always `true` — a failure is a status
/// code, never a `false` here.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, ToSchema)]
#[schema(as = SyncAck)]
pub struct Ack {
    pub ok: bool,
}

impl Default for Ack {
    fn default() -> Self {
        Self { ok: true }
    }
}

/// Whether unlinking an account or clearing a mapping actually removed anything.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, ToSchema)]
#[schema(as = SyncRemoved)]
pub struct Removed {
    pub removed: bool,
}

/// Whether resolving a conflict settled one.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, ToSchema)]
#[schema(as = SyncResolved)]
pub struct Resolved {
    pub resolved: bool,
}

/// Whether flagging a decision recorded the flag.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, ToSchema)]
#[schema(as = SyncFlagged)]
pub struct Flagged {
    pub flagged: bool,
}

/// Outcome of a pull (provider → local).
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, ToSchema)]
#[schema(as = SyncPullReport)]
pub struct PullReport {
    /// Entries returned by the provider.
    pub fetched: usize,
    /// Entries resolved to a canonical local series.
    pub matched: usize,
    /// Local progress rows written.
    pub updated: usize,
    /// Entries with no confident local match (skipped).
    pub unmatched: usize,
}

/// Outcome of a push (local → provider).
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, ToSchema)]
#[schema(as = SyncPushReport)]
pub struct PushReport {
    /// Local watchlist entries examined.
    pub considered: usize,
    /// Remote entries created or updated.
    pub pushed: usize,
    /// Watchlist entries with no resolvable remote media (skipped).
    pub unmapped: usize,
}

/// Outcome of a tokenless metadata-enrichment sweep.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, ToSchema)]
#[schema(as = SyncEnrichReport)]
pub struct EnrichReport {
    /// Series examined this sweep.
    pub scanned: usize,
    /// Series that received metadata from at least one provider.
    pub enriched: usize,
    /// Series no public provider could resolve.
    pub unresolved: usize,
}

/// One provider's outcome from a targeted single-series push.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[schema(as = SyncProviderPushOutcome)]
pub struct ProviderPushOutcome {
    pub provider: String,
    pub ok: bool,
    pub error: Option<String>,
}

/// Which side a revert put back.
///
/// An enum rather than the `&'static str` the sync service used, for the reason spelled out on
/// [`ConflictPolicy`]: a bare string on the wire is a vocabulary nothing connects, and this one
/// crosses two services and the console. It also cannot stay a `&'static str` here —
/// `services/api` has to *deserialize* it, and a borrowed field cannot outlive the response body.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
#[schema(as = SyncRestoredSide)]
pub enum RestoredSide {
    /// A local progress value was put back.
    LocalProgress,
    /// A local watch status was put back.
    LocalStatus,
    /// A watchlist entry was reinstated.
    WatchlistEntry,
    /// A remote entry was written back at the provider.
    RemoteEntry,
    /// A series↔remote-media match was undone.
    Match,
}

/// What a revert put back, for the operator who asked for it.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[schema(as = SyncRevertReport)]
pub struct RevertReport {
    pub decision_id: uuid::Uuid,
    pub restored: RestoredSide,
    /// The value the restored side now holds, for the console to show without re-reading.
    pub value: Option<String>,
    /// Set when the revert also refused a title match permanently.
    pub blocked_match: bool,
}

/// The feature flag gating each route of the external-sync surface, keyed on the path **suffix**
/// beneath the surface's mount point.
///
/// `services/api` (`/v1/me/sync`) and `services/sync` (`/v1/sync`) both serve this surface;
/// declaring the mapping once, suffix-keyed, means adding a route gates it at both hops or
/// neither, instead of the two tiers' tables drifting apart (ARCH-18). A tier that does not
/// serve a suffix still gates it, so nothing depends on which routes a given tier mounts.
///
/// `""` is the whole-surface rule; `RouteFeatures` resolves longest-prefix-first, so specific
/// suffixes win over it regardless of order here.
#[must_use]
pub const fn sync_route_features() -> &'static [(&'static str, Feature)] {
    &[
        ("", Feature::SyncExternal),
        ("/push-series", Feature::SyncAutoPush),
        ("/conflicts", Feature::SyncConflictReview),
        ("/history", Feature::SyncHistory),
    ]
}

#[cfg(test)]
mod tests {
    use super::{ConflictPolicy, UnknownConflictPolicy};

    /// `ALL` is the only hand-maintained part of the vocabulary, so this is what stops it
    /// drifting from the `match` in `as_str`: every entry must be distinct, and every entry
    /// must survive `as_str` → `from_str`. A variant added to `as_str` but forgotten here
    /// leaves the picker short; one added here but not to `as_str` cannot compile.
    #[test]
    fn every_policy_is_listed_once_and_round_trips() {
        for policy in ConflictPolicy::ALL {
            assert_eq!(
                policy.as_str().parse::<ConflictPolicy>(),
                Ok(policy),
                "`{policy}` does not survive its own token"
            );
            assert_eq!(
                ConflictPolicy::ALL.iter().filter(|p| **p == policy).count(),
                1,
                "`{policy}` is listed more than once in ConflictPolicy::ALL"
            );
        }
    }

    /// The regression this type exists for. The sync service used to parse with a
    /// `_ => NewestWins` arm and the frontend with a `_ => NewestWins` arm of its own, so a
    /// misspelled policy was not an error anywhere — it silently became "newest wins", which
    /// is the one policy that can overwrite a user's local progress without asking.
    #[test]
    fn an_unknown_token_is_an_error_rather_than_a_default() {
        let err = "newest-wins".parse::<ConflictPolicy>().unwrap_err();
        assert_eq!(err, UnknownConflictPolicy("newest-wins".to_owned()));
        assert!(
            err.to_string().contains("newest_wins"),
            "the error must name the accepted set, or an operator reading a log cannot act \
             on it: {err}"
        );
    }

    /// The token is the `serde` representation, not a second spelling of it. If they diverge,
    /// a value written by `as_str` into the database stops deserializing off the wire.
    #[test]
    fn the_token_is_the_serde_representation() {
        for policy in ConflictPolicy::ALL {
            let json = serde_json::to_string(&policy).expect("policies serialize");
            assert_eq!(json, format!("\"{}\"", policy.as_str()));
            assert_eq!(
                serde_json::from_str::<ConflictPolicy>(&json).expect("policies deserialize"),
                policy
            );
        }
    }

    /// The default is load-bearing: an account with no persisted policy and a service with no
    /// configured seed both land here, and `AskMe` would queue every disagreement while
    /// `LocalWins`/`RemoteWins` would silently pick a side.
    #[test]
    fn the_default_is_newest_wins() {
        assert_eq!(ConflictPolicy::default(), ConflictPolicy::NewestWins);
    }
}
