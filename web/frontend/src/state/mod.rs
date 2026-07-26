//! Client session state (design §17.4).
//!
//! The access token lives only in memory — never `localStorage`, so an XSS foothold cannot
//! exfiltrate it — and is re-adopted from the httpOnly refresh cookie on boot.
//!
//! The session carries **identity only**. What the reader is allowed to do lives in
//! [`capabilities`], fetched from the server rather than decoded from the token: the backend
//! authorizes per capability and a grant can be revoked at any moment, neither of which a claim
//! baked into a 15-minute token can represent. See [`jwt`] for what is still read out of the
//! token (a display name) and why doing so unverified is safe.

pub(crate) mod capabilities;
mod jwt;
pub(crate) mod prefs;

use dioxus::prelude::*;

/// App-wide session, provided via context at the router root. Every field is a `Signal`,
/// which is `Copy`, so the whole struct is `Copy` and event handlers can capture it freely.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Session {
    /// In-memory access token; `None` when signed out.
    pub(crate) token: Signal<Option<String>>,
    /// The live display name. Seeded from the token on sign-in, but overridable so a profile
    /// rename shows everywhere *instantly*, without waiting for a new token.
    pub(crate) name: Signal<Option<String>>,
    /// Whether the boot-time silent refresh has settled. Guards the sign-in flash: until
    /// this flips, "signed out" only means "we haven't looked yet".
    pub(crate) ready: Signal<bool>,
}

impl Session {
    /// Create the signals. Call once inside a component (the router root).
    pub(crate) fn new() -> Self {
        Self {
            token: Signal::new(None),
            name: Signal::new(None),
            ready: Signal::new(false),
        }
    }

    pub(crate) fn is_authenticated(&self) -> bool {
        self.token.read().is_some()
    }

    /// The current token cloned out for an API call.
    pub(crate) fn token_value(&self) -> Option<String> {
        self.token.read().clone()
    }

    /// The signed-in user's display name: a local override (set right after a profile
    /// rename) if present, else the token's claim. Purely cosmetic — the server is
    /// authoritative.
    pub(crate) fn username(&self) -> Option<String> {
        if let Some(name) = self.name.read().clone() {
            return Some(name);
        }
        self.token.read().as_deref().and_then(jwt::username)
    }

    /// Override the display name shown across the UI, e.g. once a profile PATCH succeeds.
    /// Blank names are ignored so a stale token claim keeps showing instead of nothing.
    pub(crate) fn set_display_name(self, name: impl Into<String>) {
        let name = name.into();
        let mut current = self.name;
        current.set((!name.trim().is_empty()).then_some(name));
    }

    /// Record a freshly-minted access token, seeding the display name from it. A fresh token is
    /// authoritative, so it replaces any earlier local override.
    pub(crate) fn set_token(self, token: String) {
        let name = jwt::username(&token);
        let (mut name_sig, mut token_sig) = (self.name, self.token);
        name_sig.set(name);
        token_sig.set(Some(token));
    }

    /// Clear the session (sign out).
    pub(crate) fn clear(self) {
        let (mut token, mut name) = (self.token, self.name);
        token.set(None);
        name.set(None);
    }

    pub(crate) fn mark_ready(self) {
        let mut ready = self.ready;
        ready.set(true);
    }

    /// Milliseconds until the current token's `exp`, or `None` when signed out or the token
    /// won't decode. Drives the refresh schedule in [`crate::components::Shell`] — a
    /// client-side hint only; the server remains the authority on expiry.
    pub(crate) fn expires_in_ms(&self) -> Option<f64> {
        let token = self.token.read().clone()?;
        let exp = jwt::expires_at(&token)?;
        // `exp` is a unix-second timestamp; the precision `f64` loses at that magnitude is
        // far below the minute-scale granularity this scheduling hint is used at.
        #[allow(clippy::cast_precision_loss)]
        Some(exp as f64 * 1000.0 - js_sys::Date::now())
    }
}

impl Default for Session {
    fn default() -> Self {
        Self::new()
    }
}

/// The session for any descendant component.
pub(crate) fn use_session() -> Session {
    use_context::<Session>()
}
