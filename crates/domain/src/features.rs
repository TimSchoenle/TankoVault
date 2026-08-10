//! The feature registry — every switchable capability in the product, its compiled default,
//! and its control-plane grouping. The database stores only overrides, so an empty override
//! table is a fully working deployment; see [`Feature::is_locked`] for the two that refuse off.

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
    /// Public catalogue browsing: series list, detail, chapters, tags, provider facet.
    #[serde(rename = "catalogue.browse")]
    CatalogueBrowse,
    /// Free-text and filtered search over the catalogue.
    #[serde(rename = "catalogue.search")]
    CatalogueSearch,
    /// The "you might like" recommendations on the home dashboard.
    #[serde(rename = "catalogue.recommendations")]
    CatalogueRecommendations,
    /// Whether adult-classified series may be shown to *anyone* on this deployment.
    ///
    /// The deployment-wide half of the adult gate; the per-reader opt-in is the other half, and
    /// both must be on. Off does not merely hide the preference — it closes the gate for every
    /// reader regardless of what they already opted into, which is what makes it usable as a
    /// kill switch on an instance whose audience turns out not to be what the operator assumed.
    #[serde(rename = "catalogue.adult_content")]
    CatalogueAdultContent,

    /// Whether an account is required to reach the application at all.
    ///
    /// Ships **off**, so a fresh deployment is public. On, it is the whole-product gate: every
    /// page and every route outside the sign-in surface refuses a caller with no account, and
    /// the web app sends a signed-out visitor to the sign-in screen instead of the catalogue.
    ///
    /// Unlike every other flag here, this one is enforced by *requiring* something rather than
    /// by withdrawing a route, so a refusal is `401`, not the usual `404`. What stays reachable
    /// while it is on — sign-in, registration, password reset, email confirmation and the legal
    /// documents — is the surface a visitor needs in order to *get* an account; see
    /// `services/api`'s `account_gate`.
    #[serde(rename = "accounts.required")]
    AccountsRequired,
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
    /// Passkeys: registering `WebAuthn` credentials and signing in with them instead of a
    /// password. Off hides the whole surface and refuses both ceremonies; already-registered
    /// credentials are kept, not deleted, so switching it back on restores them.
    #[serde(rename = "accounts.passkeys")]
    AccountsPasskeys,
    /// Two-factor authentication: enrolling an authenticator app or a hardware security key,
    /// and the step-up prompt that guards sensitive actions.
    ///
    /// Off hides the whole surface and refuses enrolment, but does **not** disarm the factors
    /// already enrolled — a sign-in still asks for the second leg, and a step-up is still
    /// required. Disarming them would turn a flag flip into a silent downgrade of every
    /// protected account, which is the one thing a feature switch must never do; an operator
    /// who genuinely needs that removes the enrolments.
    #[serde(rename = "accounts.mfa")]
    AccountsMfa,
    /// Require every account to hold a second factor, not just privileged ones.
    ///
    /// Ships **off**. Privileged accounts are required to enrol regardless of this flag —
    /// that requirement is in the authorization path, not here. This extends it to ordinary
    /// readers, and turning it on means every account without a factor is confined to the
    /// enrolment surface until it has one.
    #[serde(rename = "accounts.mfa_required")]
    AccountsMfaRequired,

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
    /// The standing duplicate sweep and the automatic, destructive merges it performs.
    ///
    /// Separate from [`Self::ScanningMergeQueue`]: an operator stopping a suspected
    /// over-merge must not also lose the review queue that shows the evidence.
    #[serde(rename = "scanning.auto_merge")]
    ScanningAutoMerge,

    /// The operator console's catalogue maintenance surface: the series list, bulk deletion
    /// and the catalogue purge.
    #[serde(rename = "admin.catalogue")]
    AdminCatalogue,
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
    /// User administration; see [`Feature::is_locked`].
    #[serde(rename = "admin.users")]
    AdminUsers,
    /// The feature-flag control plane itself; see [`Feature::is_locked`].
    #[serde(rename = "admin.feature_flags")]
    AdminFeatureFlags,
    /// The recommender's operator console: model health, the tuning registry, and the
    /// rebuild controls.
    #[serde(rename = "admin.recommendations")]
    AdminRecommendations,
}

impl Feature {
    /// Every feature, in the order the control plane lists them.
    #[must_use]
    pub const fn all() -> &'static [Self] {
        &[
            Self::CatalogueBrowse,
            Self::CatalogueSearch,
            Self::CatalogueRecommendations,
            Self::CatalogueAdultContent,
            Self::AccountsRequired,
            Self::AccountsRegistration,
            Self::AccountsPasswordReset,
            Self::AccountsEmailVerification,
            Self::AccountsProfile,
            Self::AccountsSessions,
            Self::AccountsPasskeys,
            Self::AccountsMfa,
            Self::AccountsMfaRequired,
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
            Self::ScanningAutoMerge,
            Self::AdminCatalogue,
            Self::AdminProviders,
            Self::AdminAdapterTest,
            Self::AdminSync,
            Self::AdminAudit,
            Self::AdminStats,
            Self::AdminUsers,
            Self::AdminFeatureFlags,
            Self::AdminRecommendations,
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
            Self::CatalogueAdultContent => "catalogue.adult_content",
            Self::AccountsRequired => "accounts.required",
            Self::AccountsRegistration => "accounts.registration",
            Self::AccountsPasswordReset => "accounts.password_reset",
            Self::AccountsEmailVerification => "accounts.email_verification",
            Self::AccountsProfile => "accounts.profile",
            Self::AccountsSessions => "accounts.sessions",
            Self::AccountsPasskeys => "accounts.passkeys",
            Self::AccountsMfa => "accounts.mfa",
            Self::AccountsMfaRequired => "accounts.mfa_required",
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
            Self::ScanningAutoMerge => "scanning.auto_merge",
            Self::AdminCatalogue => "admin.catalogue",
            Self::AdminProviders => "admin.providers",
            Self::AdminAdapterTest => "admin.adapter_test",
            Self::AdminSync => "admin.sync",
            Self::AdminAudit => "admin.audit",
            Self::AdminStats => "admin.stats",
            Self::AdminUsers => "admin.users",
            Self::AdminFeatureFlags => "admin.feature_flags",
            Self::AdminRecommendations => "admin.recommendations",
        }
    }

    /// Whether the feature ships on.
    ///
    /// Everything defaults to on: the flag system exists so an operator can *narrow* a
    /// working deployment, not so that a fresh install arrives inert. Three exceptions. Two
    /// ship off because they send data to third parties from a configuration the installer has
    /// not necessarily reviewed yet.
    ///
    /// [`Self::CatalogueAdultContent`] is the third and ships off for a different reason: it is
    /// the only flag whose default-on failure mode is showing adult material to an audience
    /// nobody decided to show it to. Every other flag defaults to the *working* state; this one
    /// defaults to the *safe* one, and turning it on is an operator's deliberate act.
    #[must_use]
    pub const fn default_enabled(self) -> bool {
        !matches!(
            self,
            Self::NotificationsWebhook
                | Self::NotificationsDiscord
                | Self::CatalogueAdultContent
                // Ships off because turning it on confines every account without a second
                // factor to the enrolment surface. That is the right end state for many
                // deployments and a catastrophic *first* impression for all of them: a fresh
                // install would lock its own installer out of everything but enrolment.
                | Self::AccountsMfaRequired
                // Ships off for the same class of reason: on, the deployment is private, and a
                // fresh install would answer its first visitor — the installer, before they have
                // registered — with a sign-in wall over an empty user table. Whether an instance
                // is public is the operator's decision to make, not a default to inherit.
                | Self::AccountsRequired
        )
    }

    /// Whether this feature may never be switched off.
    ///
    /// Both are recovery paths: disabling the flag surface removes the only way to switch
    /// anything back on, and disabling user administration removes the only way to grant
    /// access to it. Either would brick the deployment, recoverable only via a direct
    /// database edit, so the registry refuses rather than trusting a confirmation dialog.
    #[must_use]
    pub const fn is_locked(self) -> bool {
        matches!(self, Self::AdminFeatureFlags | Self::AdminUsers)
    }

    /// The control plane's grouping heading.
    #[must_use]
    pub const fn group(self) -> FeatureGroup {
        match self {
            Self::CatalogueBrowse
            | Self::CatalogueSearch
            | Self::CatalogueRecommendations
            | Self::CatalogueAdultContent => FeatureGroup::Catalogue,
            Self::AccountsRequired
            | Self::AccountsRegistration
            | Self::AccountsPasswordReset
            | Self::AccountsEmailVerification
            | Self::AccountsProfile
            | Self::AccountsSessions
            | Self::AccountsPasskeys
            | Self::AccountsMfa
            | Self::AccountsMfaRequired => FeatureGroup::Accounts,
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
            | Self::ScanningMergeQueue
            | Self::ScanningAutoMerge => FeatureGroup::Scanning,
            Self::AdminCatalogue
            | Self::AdminProviders
            | Self::AdminAdapterTest
            | Self::AdminSync
            | Self::AdminAudit
            | Self::AdminStats
            | Self::AdminUsers
            | Self::AdminFeatureFlags
            | Self::AdminRecommendations => FeatureGroup::Operations,
        }
    }

    /// Human-readable name shown in the control plane.
    #[must_use]
    pub const fn title(self) -> &'static str {
        match self {
            Self::CatalogueBrowse => "Catalogue browsing",
            Self::CatalogueSearch => "Catalogue search",
            Self::CatalogueRecommendations => "Recommendations",
            Self::CatalogueAdultContent => "Adult content",
            Self::AccountsRequired => "Require an account",
            Self::AccountsRegistration => "Self-service registration",
            Self::AccountsPasswordReset => "Password reset",
            Self::AccountsEmailVerification => "Email verification",
            Self::AccountsProfile => "Profile editing",
            Self::AccountsSessions => "Session management",
            Self::AccountsPasskeys => "Passkeys",
            Self::AccountsMfa => "Two-factor authentication",
            Self::AccountsMfaRequired => "Require two-factor for everyone",
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
            Self::ScanningAutoMerge => "Automatic duplicate merging",
            Self::AdminCatalogue => "Catalogue maintenance",
            Self::AdminProviders => "Provider management",
            Self::AdminAdapterTest => "Adapter dry-run",
            Self::AdminSync => "Sync administration",
            Self::AdminAudit => "Audit trail",
            Self::AdminStats => "System statistics",
            Self::AdminUsers => "User administration",
            Self::AdminFeatureFlags => "Feature flags",
            Self::AdminRecommendations => "Recommendation console",
        }
    }

    /// What switching this off actually does, in the operator's terms. Written to be read in
    /// the control plane immediately before someone flips a production switch, so it names
    /// the consequence rather than restating the title.
    #[must_use]
    #[expect(
        clippy::too_many_lines,
        reason = "one arm per feature, and the registry is the list: splitting it by group \
                  would put half the sentences somewhere a reader adding a feature never looks"
    )]
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
            Self::CatalogueAdultContent => {
                "Off (the default): adult-classified series are hidden from every reader on \
                 this deployment, including those who opted in and confirmed their age. On: \
                 each reader decides for themselves, and the default for a reader who has \
                 decided nothing is still hidden. Nobody is ever shown adult content by an \
                 operator turning this on alone."
            }
            Self::AccountsRequired => {
                "Off (the default): anyone can browse the catalogue signed out. On: the whole \
                 deployment is private — every page and every endpoint refuses a caller with no \
                 account, and a signed-out visitor lands on the sign-in screen. Signing in, \
                 registering, resetting a password, confirming an address and reading the legal \
                 documents stay open, or nobody could ever get an account."
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
            Self::AccountsPasskeys => {
                "Off: passkeys cannot be registered or used to sign in, and the account page \
                 hides them. Registered keys are kept, so switching it back on restores them; \
                 password sign-in is unaffected either way."
            }
            Self::AccountsMfa => {
                "Off: nobody can enrol an authenticator app or a security key, and the account \
                 page hides them. Factors already enrolled keep working — sign-in still asks \
                 for the second leg and sensitive actions still prompt — because switching a \
                 flag must not quietly downgrade an account that opted into protection."
            }
            Self::AccountsMfaRequired => {
                "Off (the default): two-factor is each reader's choice, and only accounts \
                 holding administrative permissions are required to enrol. On: every account \
                 must enrol before it can do anything but sign in and enrol. Expect support \
                 traffic the day you turn it on."
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
            Self::ScanningAutoMerge => {
                "Off: the duplicate sweep stops running, so no series is ever merged without an \
                 operator pressing merge. The review queue is unaffected."
            }
            Self::AdminCatalogue => {
                "Off: the catalogue maintenance surface closes — no operator series list, no \
                 bulk deletion and no purge. Nothing already in the catalogue changes."
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
            Self::AdminRecommendations => {
                "Off: the recommendation tuning and model-health surface closes. The \
                 recommender keeps building and serving on its stored values."
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
        assert_eq!(Feature::all().len(), 45);
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

    /// The exact set that ships off, so adding a feature cannot quietly join it.
    ///
    /// A fresh install is supposed to arrive working; every entry here is a deliberate
    /// exception with a reason in [`Feature::default_enabled`], and a sixth one appearing
    /// without that reason being written down is the failure this pins.
    #[test]
    fn only_third_party_egress_the_adult_gate_and_the_two_requirements_ship_off() {
        for &f in Feature::all() {
            let expected_off = matches!(
                f,
                Feature::NotificationsWebhook
                    | Feature::NotificationsDiscord
                    | Feature::CatalogueAdultContent
                    | Feature::AccountsMfaRequired
                    | Feature::AccountsRequired
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
