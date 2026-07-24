//! Client session state (design §17.4).
//!
//! The access token lives only in memory (never `localStorage`) and is refreshed via the
//! httpOnly refresh cookie on boot. The RBAC role is decoded from the JWT payload so the
//! left rail can reveal the operator Console only to operators/admins; the server remains
//! the true authority (every admin route is RBAC-gated).

use base64::Engine;
use dioxus::prelude::*;

/// RBAC role, mirroring `tankovault_domain::UserRole` tokens.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    User,
    Operator,
    Admin,
}

impl Role {
    fn parse(s: &str) -> Self {
        match s {
            "admin" => Self::Admin,
            "operator" => Self::Operator,
            _ => Self::User,
        }
    }

    /// True when this role satisfies the operator tier (operator or admin).
    pub fn is_operator(self) -> bool {
        matches!(self, Self::Operator | Self::Admin)
    }

    /// True only for the admin tier (create/delete provider and other destructive ops).
    pub fn is_admin(self) -> bool {
        matches!(self, Self::Admin)
    }
}

/// App-wide session, provided via context at the router root. `Signal` is `Copy`, so the
/// whole struct is `Copy` and can be captured freely by event handlers.
#[derive(Debug, Clone, Copy)]
pub struct Session {
    /// In-memory access token; `None` when signed out.
    pub token: Signal<Option<String>>,
    /// Decoded RBAC role (defaults to `User`).
    pub role: Signal<Role>,
    /// The live display name. Seeded from the JWT on sign-in, but freely overridable so a
    /// profile rename reflects everywhere *instantly*, without waiting for a new token.
    pub name: Signal<Option<String>>,
    /// Whether the boot-time silent refresh has completed (guards the initial flash).
    pub ready: Signal<bool>,
}

impl Session {
    /// Create the signals. Call once inside a component (e.g. the router root).
    pub fn new() -> Self {
        Self {
            token: Signal::new(None),
            role: Signal::new(Role::User),
            name: Signal::new(None),
            ready: Signal::new(false),
        }
    }

    pub fn is_authenticated(&self) -> bool {
        self.token.read().is_some()
    }

    /// The current token cloned out for an API call.
    pub fn token_value(&self) -> Option<String> {
        self.token.read().clone()
    }

    /// The signed-in user's display name. Prefers a locally-set override (e.g. straight
    /// after a profile rename) and otherwise falls back to the JWT claim
    /// (`username`/`name`/`sub`, whichever is present). Purely cosmetic — the server is
    /// authoritative.
    pub fn username(&self) -> Option<String> {
        if let Some(name) = self.name.read().clone() {
            return Some(name);
        }
        self.token.read().as_deref().and_then(username_from_jwt)
    }

    /// Override the display name shown across the UI, e.g. right after the profile PATCH
    /// succeeds. Ignores blank names so a stale token claim keeps showing instead.
    pub fn set_display_name(self, name: impl Into<String>) {
        let name = name.into();
        let mut n = self.name;
        n.set((!name.trim().is_empty()).then_some(name));
    }

    /// Record a freshly-minted access token, decoding its role claim and seeding the display
    /// name from it (a fresh token is authoritative, so any earlier override is replaced).
    pub fn set_token(self, token: String) {
        let role = role_from_jwt(&token);
        let name = username_from_jwt(&token);
        let mut r = self.role;
        let mut t = self.token;
        let mut n = self.name;
        r.set(role);
        n.set(name);
        t.set(Some(token));
    }

    /// Clear the session (sign out).
    pub fn clear(self) {
        let mut t = self.token;
        let mut r = self.role;
        let mut n = self.name;
        t.set(None);
        r.set(Role::User);
        n.set(None);
    }

    pub fn mark_ready(self) {
        let mut ready = self.ready;
        ready.set(true);
    }

    /// Milliseconds remaining before the current access token's `exp` claim is reached, or
    /// `None` when signed out or the token can't be decoded. Used to schedule the background
    /// refresh in [`crate::components::Shell`] — purely a client-side scheduling hint, since
    /// the server is the actual authority on expiry.
    pub fn token_expires_in_ms(&self) -> Option<f64> {
        let token = self.token.read().clone()?;
        let exp = exp_from_jwt(&token)?;
        Some(exp as f64 * 1000.0 - js_sys::Date::now())
    }
}

impl Default for Session {
    fn default() -> Self {
        Self::new()
    }
}

/// Convenience accessor for the session inside any descendant component.
pub fn use_session() -> Session {
    use_context::<Session>()
}

/// Decode the `role` claim from a JWT without verifying the signature (the server verifies
/// on every request; this is purely for showing/hiding UI). Falls back to `User`.
fn role_from_jwt(token: &str) -> Role {
    let Some(payload_b64) = token.split('.').nth(1) else {
        return Role::User;
    };
    let Ok(bytes) = base64::engine::general_purpose::URL_SAFE_NO_PAD.decode(payload_b64) else {
        return Role::User;
    };
    let Ok(value) = serde_json::from_slice::<serde_json::Value>(&bytes) else {
        return Role::User;
    };
    value
        .get("role")
        .and_then(|r| r.as_str())
        .map(Role::parse)
        .unwrap_or(Role::User)
}

/// Decode a human-facing display name from a JWT payload without verifying the signature.
fn username_from_jwt(token: &str) -> Option<String> {
    let payload_b64 = token.split('.').nth(1)?;
    let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(payload_b64)
        .ok()?;
    let value = serde_json::from_slice::<serde_json::Value>(&bytes).ok()?;
    ["username", "name", "sub"]
        .iter()
        .find_map(|k| value.get(*k).and_then(|v| v.as_str()))
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
}

/// Decode the `exp` claim (unix seconds) from a JWT payload without verifying the signature.
fn exp_from_jwt(token: &str) -> Option<i64> {
    let payload_b64 = token.split('.').nth(1)?;
    let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(payload_b64)
        .ok()?;
    let value = serde_json::from_slice::<serde_json::Value>(&bytes).ok()?;
    value.get("exp").and_then(|v| v.as_i64())
}
