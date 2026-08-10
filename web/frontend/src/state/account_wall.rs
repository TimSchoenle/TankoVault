//! Whether this deployment serves a signed-out visitor anything at all.
//!
//! The client half of the API's `accounts.required` flag. There is no anonymous endpoint that
//! announces it — a private deployment publishes nothing to the public, including the fact that
//! it is private — so the answer is *observed*: while signed out, the capability probe
//! ([`crate::components::Shell`]) is refused with `account_required` instead of the ordinary
//! `unauthorized`, and that is the difference between "your session ended" and "there is nothing
//! here without an account".
//!
//! **Not a security boundary.** The server refuses every request either way; this only decides
//! whether the app shows its sign-in screen or an error, which is the difference between a
//! private deployment and a broken-looking one.

use dioxus::prelude::*;

/// What the server has told us about anonymous access.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum Admission {
    /// Not observed yet — signed in, or the probe has not landed.
    ///
    /// Distinct from [`Self::Public`], and treated as public until proven otherwise: guessing
    /// the wall is up would bounce a reader on a perfectly public deployment to the sign-in
    /// screen for one render on every cold load, which is a worse failure than showing a screen
    /// a moment before redirecting away from it.
    #[default]
    Unknown,
    /// The deployment answers signed-out callers.
    Public,
    /// The deployment requires an account for everything but the sign-in surface.
    AccountRequired,
}

/// The observed admission policy, provided via context at the router root.
#[derive(Debug, Clone, Copy)]
pub(crate) struct AccountWall {
    inner: Signal<Admission>,
}

impl AccountWall {
    pub(crate) fn new() -> Self {
        Self {
            inner: Signal::new(Admission::Unknown),
        }
    }

    /// Adopt what the server's answer implied.
    pub(crate) fn set(self, admission: Admission) {
        let mut inner = self.inner;
        inner.set(admission);
    }

    /// Whether a signed-out visitor is to be sent to the sign-in screen.
    pub(crate) fn is_up(&self) -> bool {
        *self.inner.read() == Admission::AccountRequired
    }
}

impl Default for AccountWall {
    fn default() -> Self {
        Self::new()
    }
}

/// The observed admission policy for any descendant component.
pub(crate) fn use_account_wall() -> AccountWall {
    use_context::<AccountWall>()
}
