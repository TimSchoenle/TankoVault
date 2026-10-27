//! The HTTP wire contract for the external-sync surface.
//!
//! Response bodies `services/sync` produces on `/v1/sync/*` and `services/api` re-publishes
//! verbatim under `/v1/me/sync/*`. Both services' `#[utoipa::path]` annotations name these types,
//! so the frontend's generated client cannot drift from either.

use serde::{Deserialize, Serialize};
use tankovault_domain::Feature;
use utoipa::ToSchema;

/// Whether a user has linked a provider and, when linked, who they are connected as.
///
/// Always `200`: an unlinked account reads `{ "linked": false }`, so the UI renders "not
/// connected" without treating it as an error.
///
/// Published as `SyncAccountStatus` because `tankovault_domain` has an `AccountStatus` too,
/// for whether a user account is active or suspended. Two types sharing one `OpenAPI`
/// component name means the last one registered silently replaces the other. The qualifier
/// goes on this one as the more specific of the two.
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

/// How to settle a local and remote disagreement when a series exists on both sides
/// (design v2 §B.3).
///
/// Declared once here, where `utoipa` publishes it and `progenitor` generates the frontend's
/// copy, so a policy added to the service appears in the picker and a misspelled token fails to
/// compile. The schema names the four `snake_case` tokens, so an unknown token is a `422` at
/// the edge, not a value read as `newest_wins` in the merge.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum ConflictPolicy {
    /// Local progress and status are authoritative.
    LocalWins,
    /// The remote (`AniList`) value is authoritative.
    RemoteWins,
    /// Whichever side was updated most recently wins.
    #[default]
    NewestWins,
    /// Conflicts are queued for the user to resolve.
    AskMe,
}

impl ConflictPolicy {
    /// Every policy, in the order a picker offers them.
    ///
    /// Listed by hand, since Rust cannot enumerate an enum's variants without a derive macro.
    /// `every_policy_is_listed_once_and_round_trips` catches a variant missing from it.
    pub const ALL: [Self; 4] = [
        Self::LocalWins,
        Self::RemoteWins,
        Self::NewestWins,
        Self::AskMe,
    ];

    /// Returns the wire and persistence token, identical to the `serde` representation.
    ///
    /// ```
    /// use tankovault_contracts::sync::ConflictPolicy;
    ///
    /// assert_eq!(ConflictPolicy::AskMe.as_str(), "ask_me");
    /// assert_eq!("ask_me".parse(), Ok(ConflictPolicy::AskMe));
    /// assert!("ask-me".parse::<ConflictPolicy>().is_err());
    /// ```
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
/// Carries the offending token: the value comes from a database column or a request body, and
/// the operator reading the log needs to know which string was rejected.
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

    /// Parses a token as [`ConflictPolicy::as_str`] renders it, and refuses any other.
    ///
    /// Searching [`ConflictPolicy::ALL`] makes parsing the exact inverse of rendering, with no
    /// fallback arm that could turn a typo into a policy change.
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
    /// The provider's own consent page, already carrying this deployment's client id
    /// and redirect. Send the user to it; do not rebuild it.
    pub url: String,
}

/// One pending conflict awaiting the user's decision (design v2 §B.6 `GET /v1/me/sync/conflicts`).
///
/// `services/sync` builds it from a repository row and `services/api` re-publishes it verbatim.
/// It is a type of its own for the reason given in [`crate::admin`], and keeps the published
/// component name `ConflictRow`.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[schema(as = ConflictRow)]
pub struct ConflictView {
    /// The conflict, which is what a resolve call names.
    pub id: uuid::Uuid,
    /// The local series both sides disagree about.
    pub series_id: uuid::Uuid,
    /// Its canonical title, joined in so the list renders from one fetch.
    pub series_title: String,
    /// Which external tracker, as a slug.
    pub provider: String,
    /// Which tracked field disagrees, e.g. `progress` or `status`.
    pub field: String,
    /// What this deployment holds for `field`, rendered as text.
    pub local_value: String,
    /// What the tracker holds for it, rendered the same way.
    pub remote_value: String,
    /// When a sync first found the two disagreeing.
    #[serde(with = "time::serde::rfc3339")]
    #[schema(value_type = String)]
    pub detected_at: time::OffsetDateTime,
}

/// One row of the user-facing sync history (design v2 §B.6 `GET /v1/me/sync/history`).
/// See [`ConflictView`] for why it lives here.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[schema(as = HistoryRow)]
pub struct HistoryView {
    /// The history row.
    pub id: uuid::Uuid,
    /// The local series the action touched.
    pub series_id: uuid::Uuid,
    /// Its canonical title, joined in so the list renders from one fetch.
    pub series_title: String,
    /// Which external tracker, as a slug.
    pub provider: String,
    /// What the engine did, e.g. `pull`, `push` or `resolve`.
    pub action: String,
    /// Free-form, action-specific detail (the changed field and its before/after values).
    #[schema(value_type = Object)]
    pub detail: serde_json::Value,
    /// When the action was taken, which is what the list is ordered by.
    #[serde(with = "time::serde::rfc3339")]
    #[schema(value_type = String)]
    pub created_at: time::OffsetDateTime,
}

/// Acknowledgement for a sync route that answers `204 No Content`.
///
/// `services/sync` returns a bare `204` for linking an account and for patching settings.
/// `services/api` republishes those as `200 {"ok": true}`, because its `Upstream::decode`
/// synthesises this body for an empty upstream response. Naming the body here makes that
/// synthesis part of the contract, so a change on either side breaks a type before it reaches
/// the SPA.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, ToSchema)]
#[schema(as = SyncAck)]
pub struct Ack {
    /// Always `true`. A failure arrives as a status code, never as `false` here.
    pub ok: bool,
}

impl Default for Ack {
    fn default() -> Self {
        Self { ok: true }
    }
}

/// Whether unlinking an account or clearing a mapping removed anything.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, ToSchema)]
#[schema(as = SyncRemoved)]
pub struct Removed {
    /// `false` when there was nothing to remove, which is not an error.
    pub removed: bool,
}

/// Whether resolving a conflict settled one.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, ToSchema)]
#[schema(as = SyncResolved)]
pub struct Resolved {
    /// `false` when the conflict was already settled, which is not an error.
    pub resolved: bool,
}

/// Whether flagging a decision recorded the flag.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, ToSchema)]
#[schema(as = SyncFlagged)]
pub struct Flagged {
    /// `false` when the decision already carried a flag.
    pub flagged: bool,
}

/// The outcome of a pull from the provider into the local library.
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

/// The outcome of a push from the local library to the provider.
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

/// The outcome of a tokenless metadata-enrichment sweep.
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
    /// Which external tracker, as a slug.
    pub provider: String,
    /// Whether this provider accepted the push. One failure does not fail the others.
    pub ok: bool,
    /// Why it refused, `null` when it did not.
    pub error: Option<String>,
}

/// Which side a revert put back.
///
/// An enum for the reason given on [`ConflictPolicy`]: the vocabulary crosses two services and
/// the console, and only a published type keeps the three in step.
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
    /// A match between a series and a remote media record was undone.
    Match,
}

/// What a revert put back, for the operator who asked for it.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[schema(as = SyncRevertReport)]
pub struct RevertReport {
    /// The journal row that was undone.
    pub decision_id: uuid::Uuid,
    /// Which side the revert wrote to.
    pub restored: RestoredSide,
    /// The value the restored side now holds, for the console to show without re-reading.
    pub value: Option<String>,
    /// Set when the revert also refused a title match permanently.
    pub blocked_match: bool,
}

/// Returns the feature flag gating each route of the external-sync surface, keyed on the path
/// suffix beneath the surface's mount point.
///
/// `services/api` (`/v1/me/sync`) and `services/sync` (`/v1/sync`) both serve this surface. One
/// suffix-keyed mapping gates a new route at both hops or at neither. A tier that does not serve
/// a suffix still gates it, so nothing depends on which routes a tier mounts.
///
/// `""` is the whole-surface rule. `RouteFeatures` resolves the longest prefix first, so a
/// specific suffix wins over it whatever the order here.
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

    /// `ALL` is the only hand-maintained part of the vocabulary, so this keeps it in step with
    /// the `match` in `as_str`: every entry is distinct, and every entry survives `as_str` then
    /// `from_str`. A variant missing from `ALL` would leave the picker short.
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

    /// A misspelled policy is an error. Read as a default, it would become "newest wins", the
    /// one policy that can overwrite a user's local progress without asking.
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

    /// An account with no persisted policy and a service with no configured seed both land on
    /// the default. `AskMe` would queue every disagreement, and `LocalWins` or `RemoteWins`
    /// would silently pick a side.
    #[test]
    fn the_default_is_newest_wins() {
        assert_eq!(ConflictPolicy::default(), ConflictPolicy::NewestWins);
    }
}

/// Starts a re-verification of every stored `AniList` mapping. Internal, sent by `api` to
/// `sync`.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct MappingReverifyRequest {
    /// Report the failing mappings without removing them.
    pub dry_run: bool,
    /// The operator who started it, recorded on each match block a real run writes.
    pub actor: Option<tankovault_domain::UserId>,
}

/// Whether a re-verification was started. Internal, sent by `sync` to `api`.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct MappingReverifyStarted {
    /// `false` when another run holds the claim.
    pub started: bool,
}

/// Why a stored mapping failed re-verification.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum MappingReverifyReason {
    /// The record is not a format a library holds, such as a light novel, or has no title.
    NotALibraryRecord,
    /// None of the record's titles is the series' title or one of its sources' listing titles.
    TitlesDiffer,
}

/// One mapping that failed re-verification.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct MappingReverifyFailure {
    /// The local series.
    pub series_id: tankovault_domain::SeriesId,
    /// The provider slug.
    pub provider: String,
    /// The provider's id for the record it was mapped to.
    pub external_id: String,
    /// Why it failed.
    pub reason: MappingReverifyReason,
}
