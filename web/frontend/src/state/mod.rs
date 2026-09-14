//! Client session state (design §17.4).
//!
//! The access token lives only in memory — never `localStorage`, so an XSS foothold cannot
//! exfiltrate it — and is re-adopted from the httpOnly refresh cookie on boot. That holds on
//! both builds; what differs is only where the *refresh* cookie is kept between runs, which is
//! the browser's own store on web and the OS credential store on desktop
//! (`crate::api::session_store`).
//!
//! The session carries **identity only**. What the reader is allowed to do lives in
//! [`capabilities`], fetched from the server rather than decoded from the token: the backend
//! authorizes per capability and a grant can be revoked at any moment, neither of which a claim
//! baked into a 15-minute token can represent. See [`jwt`] for what is still read out of the
//! token (a display name) and why doing so unverified is safe.

pub(crate) mod account_wall;
pub(crate) mod branding;
pub(crate) mod capabilities;
mod jwt;
pub(crate) mod legal;
pub(crate) mod prefs;
pub(crate) mod source_order;
pub(crate) mod step_up;

use dioxus::prelude::*;

/// App-wide session, provided via context at the router root. Every field is a `Signal`,
/// which is `Copy`, so the whole struct is `Copy` and event handlers can capture it freely.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Session {
    /// In-memory access token; `None` when signed out.
    ///
    /// Reading it subscribes to every renewal. A resource that only needs to know *who* is
    /// signed in goes through [`Self::is_authenticated`] or [`Self::token_value`] instead.
    pub(crate) token: Signal<Option<String>>,
    /// Who the token speaks for: `Some(sub)` while signed in, `None` otherwise.
    ///
    /// A memo, so it notifies only when the account changes — sign-in, sign-out, a different
    /// reader — and **not** when the token is merely renewed. Every screen's resources used to
    /// subscribe to the raw token, so the silent refresh every ~14 minutes refetched the whole
    /// visible screen at once; on Home that was five heavy statements per reader, in step.
    identity: Memo<Option<String>>,
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
        let token: Signal<Option<String>> = Signal::new(None);
        Self {
            token,
            identity: Memo::new(move || {
                token
                    .read()
                    .as_deref()
                    .map(|t| jwt::subject(t).unwrap_or_default())
            }),
            name: Signal::new(None),
            ready: Signal::new(false),
        }
    }

    /// Whether someone is signed in. Subscribes to the account, not to token renewals.
    pub(crate) fn is_authenticated(&self) -> bool {
        self.identity.read().is_some()
    }

    /// The token to send with a request, subscribing the caller to the account only.
    ///
    /// The value is the current token, renewals included; what is *not* tracked is the renewal
    /// itself, so a resource built on this refetches when the reader changes and not every time
    /// the silent refresh lands.
    pub(crate) fn token_value(&self) -> Option<String> {
        let _account = self.identity.read();
        self.token.peek().clone()
    }

    /// Whether the boot-time silent refresh has settled, so an absent token is an *answer*
    /// rather than "we have not looked yet".
    pub(crate) fn is_settled(&self) -> bool {
        *self.ready.read()
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
    ///
    /// On desktop this also forgets the refresh credential in the OS credential store. That
    /// belongs here rather than at the call sites because this is what "the session is over"
    /// means in this app — a sign-out, a `401` from refresh, a deleted account and a re-pointed
    /// server all arrive through it, and a credential left behind by any one of them would sign
    /// the reader back in on the next start.
    pub(crate) fn clear(self) {
        let (mut token, mut name) = (self.token, self.name);
        token.set(None);
        name.set(None);
        #[cfg(feature = "desktop")]
        crate::api::forget_session();
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
        #[expect(
            clippy::cast_precision_loss,
            reason = "a unix-second timestamp; the precision f64 loses there is far below the \
                      minute-scale granularity this scheduling hint is used at"
        )]
        Some(exp as f64 * 1000.0 - crate::platform::now_ms())
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

#[cfg(test)]
mod tests {
    use super::Session;
    use base64::Engine as _;
    use dioxus::dioxus_core::{NoOpMutations, ReactiveContext, ScopeId, VirtualDom};
    use dioxus::prelude::*;
    use std::sync::atomic::{AtomicU32, Ordering};
    use std::sync::Arc;

    fn token(sub: &str, exp: i64) -> String {
        let payload = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .encode(format!(r#"{{"sub":"{sub}","exp":{exp}}}"#));
        format!("header.{payload}.signature")
    }

    /// A token renewal for the same account must not re-run what the account keys.
    ///
    /// The bug this pins: every `use_resource` built its client from the raw token signal, so the
    /// silent refresh every ~14 minutes restarted every resource on screen at once. Home is five
    /// statements recomputing the reader's unread state over the whole watchlist, and production
    /// logged them in a burst on exactly that cadence, 6–30 s each. Sign-in, sign-out and a change
    /// of account must still re-run it.
    #[test]
    fn a_renewal_does_not_rerun_what_the_signed_in_account_keys() {
        let mut dom = VirtualDom::new(|| rsx! {});
        dom.rebuild(&mut NoOpMutations);
        dom.in_scope(ScopeId::APP, || {
            let session = Session::new();
            let runs = Arc::new(AtomicU32::new(0));
            let context = ReactiveContext::new_with_callback(
                {
                    let runs = Arc::clone(&runs);
                    move || {
                        runs.fetch_add(1, Ordering::Relaxed);
                    }
                },
                ScopeId::APP,
                std::panic::Location::caller(),
            );
            let subscribe = || {
                context.reset_and_run_in(|| {
                    let _ = session.token_value();
                    let _ = session.is_authenticated();
                });
            };
            subscribe();

            // Each step forces the identity memo to settle before counting, which is what the
            // runtime would do on its next turn.
            let step = |next: Option<String>| {
                match next {
                    Some(t) => session.set_token(t),
                    None => session.clear(),
                }
                let _ = session.identity.peek();
                let _ = session.is_authenticated();
                let fired = runs.swap(0, Ordering::Relaxed);
                subscribe();
                fired
            };

            assert!(step(Some(token("reader", 1_000))) > 0, "signing in");
            assert_eq!(step(Some(token("reader", 2_000))), 0, "a renewal");
            assert_eq!(step(Some(token("reader", 3_000))), 0, "another renewal");
            assert!(
                step(Some(token("someone-else", 4_000))) > 0,
                "a change of account"
            );
            assert!(step(None) > 0, "signing out");
        });
    }
}
