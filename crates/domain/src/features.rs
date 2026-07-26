//! The feature registry — every switchable capability in the product, declared once.
//!
//! # The two kinds of switch, and why this is not the other one
//!
//! `tankovault-service` already has *configuration* toggles (metrics, audit, rate limiting).
//! Those are resolved once at wiring time: audit off installs a no-op sink, rate limiting off
//! leaves the layer unmounted. That contract is deliberate and must not be broken.
//!
//! A **feature flag** is a different thing and needs the opposite property: an operator turns
//! it on or off from the control plane while the process is running, and the change must take
//! effect without a redeploy. So a flag *is* consulted at request time. What keeps that from
//! degenerating into `if cfg.x` scattered through handlers is that the consultation is
//! **declarative**: an HTTP route names the feature it belongs to in one table next to the
//! route registration, and one middleware enforces every entry. Background loops, which have
//! no route to hang a declaration on, check their own flag at the top of each iteration —
//! the loop *is* the feature there, so that check is the same declaration in a different
//! shape.
//!
//! # Defaults live in code, overrides live in the database
//!
//! Each [`Feature`] carries a compiled default. The database stores only *overrides*, so:
//!
//! - adding a feature needs no migration and no seed step — it appears in the control plane
//!   immediately, at its declared default;
//! - deleting the override row is a meaningful operation ("go back to the shipped default"),
//!   distinct from explicitly setting the same value;
//! - an empty `feature_flag_overrides` table is a fully working deployment.
//!
//! # Locked features
//!
//! A few features cannot be switched off, and the registry says so rather than relying on an
//! operator's judgement. Disabling the feature-flag surface itself, or the user
//! administration that could re-enable it, would lock every administrator out of the
//! deployment with no in-band recovery. [`Feature::is_locked`] refuses those, and the API
//! rejects the write.

use serde::{Deserialize, Serialize};
use std::fmt;
use std::str::FromStr;
use utoipa::ToSchema;

/// A switchable product capability.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, ToSchema,
)]
#[non_exhaustive]
pub enum Feature {
    // --- catalogue ---
    /// Public catalogue browsing: series list, detail, chapters, tags, provider facet.
    #[serde(rename = "catalogue.browse")]
    CatalogueBrowse,
    /// Free-text and filtered search over the catalogue.
    #[serde(rename = "catalogue.search")]
    CatalogueSearch,
    /// The "you might like" recommendations on the home dashboard.
    #[serde(rename = "catalogue.recommendations")]
    CatalogueRecommendations,

    // --- accounts ---
    /// Self-service registration. Off makes the deployment invite-only: existing accounts
    /// keep working, new ones can only be created by an administrator.
    #[serde(rename = "accounts.registration")]
    AccountsRegistration,
    /// The forgot-password / reset-password flow.
    #[serde(rename = "accounts.password_reset")]
    AccountsPasswordReset,
    /// Email confirmation before first sign-in. Off lets an account sign in immediately.
    #[serde(rename = "accounts.email_verification")]
    AccountsEmailVerification,
    /// Editing one's own display name and email.
    #[serde(rename = "accounts.profile")]
    AccountsProfile,
    /// Viewing and revoking one's own login sessions.
    #[serde(rename = "accounts.sessions")]
    AccountsSessions,

    // --- privacy / data protection ---
    /// Self-service personal-data export (GDPR Art. 20).
    #[serde(rename = "privacy.self_export")]
    PrivacySelfExport,
    /// Self-service account erasure (GDPR Art. 17). Off routes erasure through the
    /// data-subject request queue instead of removing the right.
    #[serde(rename = "privacy.self_erasure")]
    PrivacySelfErasure,
    /// The data-subject request workflow: users file requests, operators fulfil them.
    #[serde(rename = "privacy.requests")]
    PrivacyRequests,

    // --- tracking ---
    /// The watchlist.
    #[serde(rename = "tracking.watchlist")]
    TrackingWatchlist,
    /// Per-series and per-chapter reading progress.
    #[serde(rename = "tracking.progress")]
    TrackingProgress,
    /// The reading feed and continue-reading rail.
    #[serde(rename = "tracking.feed")]
    TrackingFeed,
    /// Personal reading statistics.
    #[serde(rename = "tracking.stats")]
    TrackingStats,

    // --- notifications ---
    /// In-app notification rows and the unread badge.
    #[serde(rename = "notifications.in_app")]
    NotificationsInApp,
    /// The live SSE push that updates the badge without a reload.
    #[serde(rename = "notifications.live")]
    NotificationsLive,
    /// Per-user notification preferences.
    #[serde(rename = "notifications.preferences")]
    NotificationsPreferences,
    /// Outbound email notifications (SMTP).
    #[serde(rename = "notifications.email")]
    NotificationsEmail,
    /// Outbound generic JSON webhook notifications.
    #[serde(rename = "notifications.webhook")]
    NotificationsWebhook,
    /// Outbound Discord notifications.
    #[serde(rename = "notifications.discord")]
    NotificationsDiscord,

    // --- external sync ---
    /// External tracker sync as a whole (`AniList` and any future provider). Off hides the
    /// entire surface; the finer flags below shape *how* it syncs.
    #[serde(rename = "sync.external")]
    SyncExternal,
    /// Push progress to linked trackers immediately when a chapter is marked read.
    #[serde(rename = "sync.auto_push")]
    SyncAutoPush,
    /// The scheduled reconciliation that pulls remote changes back automatically.
    #[serde(rename = "sync.scheduled_pull")]
    SyncScheduledPull,
    /// The `ask_me` conflict-review queue.
    #[serde(rename = "sync.conflict_review")]
    SyncConflictReview,
    /// The user-facing sync history log.
    #[serde(rename = "sync.history")]
    SyncHistory,

    // --- scanning ---
    /// The periodic scheduler sweeps. Off means scans only ever run on demand.
    #[serde(rename = "scanning.scheduler")]
    ScanningScheduler,
    /// Operator-triggered scan runs.
    #[serde(rename = "scanning.manual")]
    ScanningManual,
    /// Full catalogue scans, as opposed to the cheap latest-feed pass.
    #[serde(rename = "scanning.full")]
    ScanningFull,
    /// Automatic canonicalisation merge-candidate recording and the review queue.
    #[serde(rename = "scanning.merge_queue")]
    ScanningMergeQueue,

    // --- operator surfaces ---
    /// The operator console's provider lifecycle surface.
    #[serde(rename = "admin.providers")]
    AdminProviders,
    /// The live adapter dry-run and challenge re-solve.
    #[serde(rename = "admin.adapter_test")]
    AdminAdapterTest,
    /// Operator visibility into other users' external sync state.
    #[serde(rename = "admin.sync")]
    AdminSync,
    /// The audit trail surface.
    #[serde(rename = "admin.audit")]
    AdminAudit,
    /// The system statistics surface.
    #[serde(rename = "admin.stats")]
    AdminStats,
    /// User administration. **Locked** — see the module docs.
    #[serde(rename = "admin.users")]
    AdminUsers,
    /// The feature-flag control plane itself. **Locked** — see the module docs.
    #[serde(rename = "admin.feature_flags")]
    AdminFeatureFlags,
}

impl Feature {
    /// Every feature, in the order the control plane lists them.
    #[must_use]
    pub const fn all() -> &'static [Self] {
        &[
            Self::CatalogueBrowse,
            Self::CatalogueSearch,
            Self::CatalogueRecommendations,
            Self::AccountsRegistration,
            Self::AccountsPasswordReset,
            Self::AccountsEmailVerification,
            Self::AccountsProfile,
            Self::AccountsSessions,
            Self::PrivacySelfExport,
            Self::PrivacySelfErasure,
            Self::PrivacyRequests,
            Self::TrackingWatchlist,
            Self::TrackingProgress,
            Self::TrackingFeed,
            Self::TrackingStats,
            Self::NotificationsInApp,
            Self::NotificationsLive,
            Self::NotificationsPreferences,
            Self::NotificationsEmail,
            Self::NotificationsWebhook,
            Self::NotificationsDiscord,
            Self::SyncExternal,
            Self::SyncAutoPush,
            Self::SyncScheduledPull,
            Self::SyncConflictReview,
            Self::SyncHistory,
            Self::ScanningScheduler,
            Self::ScanningManual,
            Self::ScanningFull,
            Self::ScanningMergeQueue,
            Self::AdminProviders,
            Self::AdminAdapterTest,
            Self::AdminSync,
            Self::AdminAudit,
            Self::AdminStats,
            Self::AdminUsers,
            Self::AdminFeatureFlags,
        ]
    }

    /// The persisted key. Stable forever — it is a database primary key and appears in audit
    /// records.
    #[must_use]
    pub const fn key(self) -> &'static str {
        match self {
            Self::CatalogueBrowse => "catalogue.browse",
            Self::CatalogueSearch => "catalogue.search",
            Self::CatalogueRecommendations => "catalogue.recommendations",
            Self::AccountsRegistration => "accounts.registration",
            Self::AccountsPasswordReset => "accounts.password_reset",
            Self::AccountsEmailVerification => "accounts.email_verification",
            Self::AccountsProfile => "accounts.profile",
            Self::AccountsSessions => "accounts.sessions",
            Self::PrivacySelfExport => "privacy.self_export",
            Self::PrivacySelfErasure => "privacy.self_erasure",
            Self::PrivacyRequests => "privacy.requests",
            Self::TrackingWatchlist => "tracking.watchlist",
            Self::TrackingProgress => "tracking.progress",
            Self::TrackingFeed => "tracking.feed",
            Self::TrackingStats => "tracking.stats",
            Self::NotificationsInApp => "notifications.in_app",
            Self::NotificationsLive => "notifications.live",
            Self::NotificationsPreferences => "notifications.preferences",
            Self::NotificationsEmail => "notifications.email",
            Self::NotificationsWebhook => "notifications.webhook",
            Self::NotificationsDiscord => "notifications.discord",
            Self::SyncExternal => "sync.external",
            Self::SyncAutoPush => "sync.auto_push",
            Self::SyncScheduledPull => "sync.scheduled_pull",
            Self::SyncConflictReview => "sync.conflict_review",
            Self::SyncHistory => "sync.history",
            Self::ScanningScheduler => "scanning.scheduler",
            Self::ScanningManual => "scanning.manual",
            Self::ScanningFull => "scanning.full",
            Self::ScanningMergeQueue => "scanning.merge_queue",
            Self::AdminProviders => "admin.providers",
            Self::AdminAdapterTest => "admin.adapter_test",
            Self::AdminSync => "admin.sync",
            Self::AdminAudit => "admin.audit",
            Self::AdminStats => "admin.stats",
            Self::AdminUsers => "admin.users",
            Self::AdminFeatureFlags => "admin.feature_flags",
        }
    }

    /// Whether the feature ships on.
    ///
    /// Everything defaults to on: the flag system exists so an operator can *narrow* a
    /// working deployment, not so that a fresh install arrives inert. Two exceptions ship
    /// off because they send data to third parties from a configuration the installer has
    /// not necessarily reviewed yet.
    #[must_use]
    pub const fn default_enabled(self) -> bool {
        !matches!(
            self,
            Self::NotificationsWebhook | Self::NotificationsDiscord
        )
    }

    /// Whether this feature may never be switched off.
    ///
    /// Both locked features are recovery paths. Turning off the flag surface removes the only
    /// way to turn anything back on; turning off user administration removes the only way to
    /// grant someone the permission to reach the flag surface. Either would brick the
    /// deployment from the operator's side, recoverable only by editing the database
    /// directly, so the registry refuses instead of trusting a confirmation dialog.
    #[must_use]
    pub const fn is_locked(self) -> bool {
        matches!(self, Self::AdminFeatureFlags | Self::AdminUsers)
    }

    /// The control plane's grouping heading.
    #[must_use]
    pub const fn group(self) -> FeatureGroup {
        match self {
            Self::CatalogueBrowse | Self::CatalogueSearch | Self::CatalogueRecommendations => {
                FeatureGroup::Catalogue
            }
            Self::AccountsRegistration
            | Self::AccountsPasswordReset
            | Self::AccountsEmailVerification
            | Self::AccountsProfile
            | Self::AccountsSessions => FeatureGroup::Accounts,
            Self::PrivacySelfExport | Self::PrivacySelfErasure | Self::PrivacyRequests => {
                FeatureGroup::Privacy
            }
            Self::TrackingWatchlist
            | Self::TrackingProgress
            | Self::TrackingFeed
            | Self::TrackingStats => FeatureGroup::Tracking,
            Self::NotificationsInApp
            | Self::NotificationsLive
            | Self::NotificationsPreferences
            | Self::NotificationsEmail
            | Self::NotificationsWebhook
            | Self::NotificationsDiscord => FeatureGroup::Notifications,
            Self::SyncExternal
            | Self::SyncAutoPush
            | Self::SyncScheduledPull
            | Self::SyncConflictReview
            | Self::SyncHistory => FeatureGroup::Sync,
            Self::ScanningScheduler
            | Self::ScanningManual
            | Self::ScanningFull
            | Self::ScanningMergeQueue => FeatureGroup::Scanning,
            Self::AdminProviders
            | Self::AdminAdapterTest
            | Self::AdminSync
            | Self::AdminAudit
            | Self::AdminStats
            | Self::AdminUsers
            | Self::AdminFeatureFlags => FeatureGroup::Operations,
        }
    }

    /// Human-readable name shown in the control plane.
    #[must_use]
    pub const fn title(self) -> &'static str {
        match self {
            Self::CatalogueBrowse => "Catalogue browsing",
            Self::CatalogueSearch => "Catalogue search",
            Self::CatalogueRecommendations => "Recommendations",
            Self::AccountsRegistration => "Self-service registration",
            Self::AccountsPasswordReset => "Password reset",
            Self::AccountsEmailVerification => "Email verification",
            Self::AccountsProfile => "Profile editing",
            Self::AccountsSessions => "Session management",
            Self::PrivacySelfExport => "Self-service data export",
            Self::PrivacySelfErasure => "Self-service account deletion",
            Self::PrivacyRequests => "Data-subject requests",
            Self::TrackingWatchlist => "Watchlist",
            Self::TrackingProgress => "Reading progress",
            Self::TrackingFeed => "Reading feed",
            Self::TrackingStats => "Reading statistics",
            Self::NotificationsInApp => "In-app notifications",
            Self::NotificationsLive => "Live notification stream",
            Self::NotificationsPreferences => "Notification preferences",
            Self::NotificationsEmail => "Email notifications",
            Self::NotificationsWebhook => "Webhook notifications",
            Self::NotificationsDiscord => "Discord notifications",
            Self::SyncExternal => "External tracker sync",
            Self::SyncAutoPush => "Automatic progress push",
            Self::SyncScheduledPull => "Scheduled reconciliation",
            Self::SyncConflictReview => "Sync conflict review",
            Self::SyncHistory => "Sync history",
            Self::ScanningScheduler => "Scan scheduler",
            Self::ScanningManual => "Manual scans",
            Self::ScanningFull => "Full catalogue scans",
            Self::ScanningMergeQueue => "Merge queue",
            Self::AdminProviders => "Provider management",
            Self::AdminAdapterTest => "Adapter dry-run",
            Self::AdminSync => "Sync administration",
            Self::AdminAudit => "Audit trail",
            Self::AdminStats => "System statistics",
            Self::AdminUsers => "User administration",
            Self::AdminFeatureFlags => "Feature flags",
        }
    }

    /// What switching this off actually does, in the operator's terms. Written to be read in
    /// the control plane immediately before someone flips a production switch, so it names
    /// the consequence rather than restating the title.
    #[must_use]
    pub const fn description(self) -> &'static str {
        match self {
            Self::CatalogueBrowse => {
                "Off: the public catalogue returns nothing — series, chapter and tag \
                 endpoints stop answering. Signed-in tracking data is untouched."
            }
            Self::CatalogueSearch => "Off: the search screen and query parameter are rejected.",
            Self::CatalogueRecommendations => {
                "Off: the home dashboard drops its recommendation rail."
            }
            Self::AccountsRegistration => {
                "Off: the deployment becomes invite-only. Existing accounts keep working; \
                 new ones must be created by an administrator."
            }
            Self::AccountsPasswordReset => {
                "Off: forgotten passwords must be reset by an administrator."
            }
            Self::AccountsEmailVerification => {
                "Off: new accounts can sign in immediately without confirming their address."
            }
            Self::AccountsProfile => "Off: users cannot change their own name or email.",
            Self::AccountsSessions => {
                "Off: users cannot list or revoke their own sessions. Sign-out still works."
            }
            Self::PrivacySelfExport => {
                "Off: users cannot download their own data. File an access request instead."
            }
            Self::PrivacySelfErasure => {
                "Off: users cannot delete their own account directly. Erasure requests still \
                 reach the privacy queue, so the right is preserved — it becomes mediated."
            }
            Self::PrivacyRequests => {
                "Off: the data-subject request queue closes. Only turn this off if requests \
                 are handled entirely outside this system."
            }
            Self::TrackingWatchlist => "Off: watchlist reads and writes are rejected.",
            Self::TrackingProgress => {
                "Off: chapters can no longer be marked read and progress stops being served."
            }
            Self::TrackingFeed => "Off: the reading feed and continue-reading rail go away.",
            Self::TrackingStats => "Off: personal reading statistics stop being served.",
            Self::NotificationsInApp => {
                "Off: no notification rows are written and the list stops answering."
            }
            Self::NotificationsLive => {
                "Off: the SSE stream closes; unread counts update on reload instead."
            }
            Self::NotificationsPreferences => {
                "Off: notification preferences are frozen at their current values."
            }
            Self::NotificationsEmail => "Off: no notification email is sent.",
            Self::NotificationsWebhook => "Off: no outbound webhook is called.",
            Self::NotificationsDiscord => "Off: no Discord message is posted.",
            Self::SyncExternal => {
                "Off: the whole external-tracker surface closes — no linking, pulling or \
                 pushing. Stored links and tokens are kept, so turning it back on resumes."
            }
            Self::SyncAutoPush => {
                "Off: marking a chapter read no longer reaches linked trackers immediately. \
                 A manual or scheduled sync still does."
            }
            Self::SyncScheduledPull => {
                "Off: remote changes are only picked up when a user syncs manually."
            }
            Self::SyncConflictReview => {
                "Off: the ask-me conflict queue stops accepting entries; accounts set to \
                 ask-me fall back to their provider's default resolution."
            }
            Self::SyncHistory => "Off: the per-user sync log stops being served.",
            Self::ScanningScheduler => {
                "Off: no periodic sweeps. Scans only run when someone triggers one."
            }
            Self::ScanningManual => "Off: operators cannot trigger scan runs.",
            Self::ScanningFull => {
                "Off: only the cheap latest-feed pass runs; full catalogue walks are refused."
            }
            Self::ScanningMergeQueue => {
                "Off: duplicate-series candidates stop being surfaced for review."
            }
            Self::AdminProviders => {
                "Off: the provider lifecycle surface closes. Existing providers keep scanning."
            }
            Self::AdminAdapterTest => {
                "Off: live dry-runs against provider sites are refused. Useful when a \
                 provider has asked for no unscheduled traffic."
            }
            Self::AdminSync => "Off: operators lose visibility into other users' sync state.",
            Self::AdminAudit => {
                "Off: the audit trail stops being served. Records are still written."
            }
            Self::AdminStats => "Off: the system statistics surface closes.",
            Self::AdminUsers => {
                "Cannot be switched off: it is the only way to grant the permission that \
                 reaches this page."
            }
            Self::AdminFeatureFlags => {
                "Cannot be switched off: it is the only way to switch anything back on."
            }
        }
    }
}

impl fmt::Display for Feature {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.key())
    }
}

/// Error raised when a stored override names a feature this build does not have.
#[derive(Debug, Clone, thiserror::Error)]
#[error("unknown feature: {key:?}")]
pub struct ParseFeatureError {
    pub key: String,
}

impl FromStr for Feature {
    type Err = ParseFeatureError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::all()
            .iter()
            .copied()
            .find(|f| f.key() == s)
            .ok_or_else(|| ParseFeatureError { key: s.to_owned() })
    }
}

/// The control plane's grouping of features into sections.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, ToSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum FeatureGroup {
    Catalogue,
    Accounts,
    Privacy,
    Tracking,
    Notifications,
    Sync,
    Scanning,
    Operations,
}

impl FeatureGroup {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Catalogue => "catalogue",
            Self::Accounts => "accounts",
            Self::Privacy => "privacy",
            Self::Tracking => "tracking",
            Self::Notifications => "notifications",
            Self::Sync => "sync",
            Self::Scanning => "scanning",
            Self::Operations => "operations",
        }
    }
}

impl fmt::Display for FeatureGroup {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;

    #[test]
    fn every_key_round_trips_and_is_unique() {
        let mut seen = BTreeSet::new();
        for &f in Feature::all() {
            assert_eq!(Feature::from_str(f.key()).unwrap(), f);
            assert!(seen.insert(f.key()), "duplicate key {}", f.key());
        }
        assert_eq!(seen.len(), Feature::all().len());
    }

    #[test]
    fn all_lists_every_variant() {
        assert_eq!(Feature::all().len(), 37);
    }

    #[test]
    fn serde_uses_the_persisted_key() {
        assert_eq!(
            serde_json::to_string(&Feature::SyncAutoPush).unwrap(),
            "\"sync.auto_push\""
        );
    }

    #[test]
    fn the_recovery_paths_are_locked_and_default_on() {
        for locked in [Feature::AdminFeatureFlags, Feature::AdminUsers] {
            assert!(locked.is_locked());
            assert!(locked.default_enabled());
        }
    }

    #[test]
    fn only_third_party_egress_ships_off() {
        for &f in Feature::all() {
            let expected_off = matches!(
                f,
                Feature::NotificationsWebhook | Feature::NotificationsDiscord
            );
            assert_eq!(!f.default_enabled(), expected_off, "{f} default");
        }
    }

    #[test]
    fn every_feature_is_described() {
        for &f in Feature::all() {
            assert!(!f.title().is_empty(), "{f} has no title");
            assert!(f.description().len() > 20, "{f} needs a real description");
        }
    }
}
