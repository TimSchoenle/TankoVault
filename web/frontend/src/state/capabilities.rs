//! What the signed-in reader may do, and what this deployment offers.
//!
//! # Why this replaced the decoded role claim
//!
//! The rail and the console used to be gated on a `role` claim decoded out of the access
//! token. Two problems, both real:
//!
//! 1. **It could not express what the backend enforces.** The server authorizes per capability
//!    (`providers.delete`), so a three-tier role could only ever approximate which controls to
//!    show — and it approximated by over-showing, which is how a reader ends up clicking a
//!    button that 403s.
//! 2. **It could not change without a new token.** An administrator granting someone a
//!    permission had no effect on that person's UI until their access token was reissued.
//!
//! Capabilities are fetched from `GET /v1/me/capabilities` instead, keyed on the session token
//! so they refetch across sign-in, the boot-time silent refresh and sign-out. They carry both
//! the caller's permissions and the deployment's enabled features, because a control needs both
//! to be shown: the reader must be allowed *and* the feature must exist here.
//!
//! **None of this is a security boundary.** Every action is authorized again by the handler
//! that performs it; hiding a control the server would refuse is a courtesy. The one thing this
//! must not do is show something that cannot work.

use crate::wire::types::{Capabilities, Feature, Permission};
use dioxus::prelude::*;

/// The capability snapshot, plus whether it has been fetched yet.
///
/// `Loading` is a distinct state rather than "empty": until the fetch lands, "you hold nothing"
/// and "we have not asked" look identical, and treating the first as the second makes the whole
/// console flash into view a moment after every page load.
#[derive(Debug, Clone, PartialEq, Default)]
pub(crate) enum CapabilityState {
    /// Not fetched yet — either signed out, or the request is in flight.
    #[default]
    Loading,
    /// The server's answer.
    Ready(Capabilities),
}

/// App-wide capabilities, provided via context at the router root.
#[derive(Debug, Clone, Copy)]
pub(crate) struct CapabilitySet {
    inner: Signal<CapabilityState>,
}

impl CapabilitySet {
    pub(crate) fn new() -> Self {
        Self {
            inner: Signal::new(CapabilityState::Loading),
        }
    }

    /// Adopt a freshly fetched snapshot.
    pub(crate) fn set(self, capabilities: Capabilities) {
        let mut inner = self.inner;
        inner.set(CapabilityState::Ready(capabilities));
    }

    /// Forget everything — on sign-out, or when a fetch fails and the previous answer can no
    /// longer be trusted to describe the current session.
    pub(crate) fn clear(self) {
        let mut inner = self.inner;
        inner.set(CapabilityState::Loading);
    }

    /// Whether the snapshot has arrived. Views use this to hold back a "you have no access"
    /// message that would otherwise flash before the first fetch lands.
    pub(crate) fn is_ready(&self) -> bool {
        matches!(*self.inner.read(), CapabilityState::Ready(_))
    }

    /// Whether the reader holds `permission`.
    ///
    /// `false` while loading: showing a privileged control and then withdrawing it is worse
    /// than showing it a moment late.
    pub(crate) fn can(&self, permission: Permission) -> bool {
        match &*self.inner.read() {
            CapabilityState::Loading => false,
            CapabilityState::Ready(caps) => caps.permissions.contains(&permission),
        }
    }

    /// Whether the reader holds **any** of `permissions` — the "should this tab exist at all?"
    /// question, which is a union rather than a specific right.
    pub(crate) fn can_any(&self, permissions: &[Permission]) -> bool {
        permissions.iter().any(|p| self.can(*p))
    }

    /// Whether `feature` is switched on for this deployment.
    ///
    /// Unlike [`Self::can`], this defaults to **true** while loading. A feature flag describes
    /// the deployment, not the reader, and virtually every feature is on virtually everywhere;
    /// defaulting to off would blank out the entire app for one render on every page load. The
    /// cost of being wrong is a control that briefly appears and then 404s — the same thing
    /// that happens if an operator switches a feature off while someone is looking at it.
    pub(crate) fn has_feature(&self, feature: Feature) -> bool {
        match &*self.inner.read() {
            CapabilityState::Loading => true,
            CapabilityState::Ready(caps) => caps.features.contains(&feature),
        }
    }

    /// Whether the reader can reach the operator console at all.
    ///
    /// Any single operator-surface permission is enough: the console's own tabs each gate
    /// themselves, so someone holding only `merge.read` gets in and sees exactly one tab.
    pub(crate) fn is_staff(&self) -> bool {
        self.can_any(CONSOLE_PERMISSIONS)
    }

    /// The catalogue key of the word shown next to the reader's name in the rail and on Account.
    ///
    /// Derived rather than stored. There is no role to display any more, and inventing one from
    /// a grant set ("this looks like an admin") would re-introduce exactly the fiction the
    /// permission model removed. Two honest tiers: someone who can reach the console, and
    /// someone who cannot.
    pub(crate) fn label_key(&self) -> &'static str {
        if self.is_staff() {
            "enum.tier.staff"
        } else {
            "enum.tier.reader"
        }
    }
}

impl Default for CapabilitySet {
    fn default() -> Self {
        Self::new()
    }
}

/// Every permission that grants access to *some* console tab.
///
/// The union the rail tests to decide whether to show the Console link at all. Kept next to the
/// per-tab requirements below so adding a tab and making it reachable are one edit.
pub(crate) const CONSOLE_PERMISSIONS: &[Permission] = &[
    Permission::SystemStats,
    Permission::ProvidersRead,
    Permission::ScansRead,
    Permission::MergeRead,
    Permission::SyncAdminRead,
    Permission::UsersRead,
    Permission::AuditRead,
    Permission::FlagsRead,
    Permission::PrivacyRead,
];

/// The capabilities for any descendant component.
pub(crate) fn use_capabilities() -> CapabilitySet {
    use_context::<CapabilitySet>()
}
