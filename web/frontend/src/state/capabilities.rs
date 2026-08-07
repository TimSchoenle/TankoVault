//! What the signed-in reader may do, and what this deployment offers.
//!
//! Capabilities are fetched from `GET /v1/me/capabilities`, keyed on the session token so they
//! refetch across sign-in, the boot-time silent refresh and sign-out. A control needs both: the
//! reader must be allowed *and* the feature must exist here.
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
    ///
    /// The super user grant answers yes to everything, mirroring `PermissionSet::has` on the
    /// backend. The wire carries the single token rather than an expansion, so this is the one
    /// place the implication has to be repeated — without it the deployment owner would be
    /// handed a console with every panel hidden.
    pub(crate) fn can(&self, permission: Permission) -> bool {
        match &*self.inner.read() {
            CapabilityState::Loading => false,
            CapabilityState::Ready(caps) => {
                caps.permissions.contains(&Permission::SystemSuperuser)
                    || caps.permissions.contains(&permission)
            }
        }
    }

    /// Whether the reader holds **any** of `permissions` — the "should this tab exist at all?"
    /// question, which is a union rather than a specific right.
    pub(crate) fn can_any(&self, permissions: &[Permission]) -> bool {
        permissions.iter().any(|p| self.can(*p))
    }

    /// Whether `feature` is switched on for this deployment.
    ///
    /// Unlike [`Self::can`], this defaults to **true** while loading — defaulting to off would
    /// blank the whole app for one render on every page load. The cost of being wrong is a
    /// control that briefly appears and then 404s, same as an operator flipping the flag off
    /// mid-session.
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
    /// Derived from capabilities, not stored — there is no role to display.
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

/// Every permission that grants access to *some* console tab — kept beside the per-tab
/// requirements so adding one stays a single edit.
pub(crate) const CONSOLE_PERMISSIONS: &[Permission] = &[
    Permission::SystemStats,
    Permission::ProvidersRead,
    Permission::ScansRead,
    Permission::MergeRead,
    Permission::RecsysRead,
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
