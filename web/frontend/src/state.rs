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
    /// Whether the boot-time silent refresh has completed (guards the initial flash).
    pub ready: Signal<bool>,
}

impl Session {
    /// Create the signals. Call once inside a component (e.g. the router root).
    pub fn new() -> Self {
        Self {
            token: Signal::new(None),
            role: Signal::new(Role::User),
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

    /// Record a freshly-minted access token and decode its role claim.
    pub fn set_token(self, token: String) {
        let role = role_from_jwt(&token);
        let mut r = self.role;
        let mut t = self.token;
        r.set(role);
        t.set(Some(token));
    }

    /// Clear the session (sign out).
    pub fn clear(self) {
        let mut t = self.token;
        let mut r = self.role;
        t.set(None);
        r.set(Role::User);
    }

    pub fn mark_ready(self) {
        let mut ready = self.ready;
        ready.set(true);
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
