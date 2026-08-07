//! The desktop build's cookie jar, and the OS credential store behind it.
//!
//! The refresh credential is an `HttpOnly` cookie. On web the browser owns the jar and there is
//! nothing to decide; a native `reqwest` has no cookie store at all unless asked, and a jar that
//! only lives in memory ends the reader's session every time they close the app — which is what
//! this build did until now.
//!
//! **Why the OS credential store and not a file.** The property the web build gets for free is
//! that the refresh cookie is encrypted at rest, scoped to the user's login, and unreachable from
//! script. `settings.json` offers none of that: it is plain text readable by every process
//! running as the reader, so a long-lived bearer credential in it would be strictly worse than
//! signing out on exit. The Credential Manager, the Secret Service and the Keychain each offer
//! exactly the missing guarantee, so that is where this writes and nowhere else. Where the store
//! is unavailable — a headless Linux session with no Secret Service provider — every call is a
//! silent no-op and the app falls back to the old behaviour rather than to a file.
//!
//! **The access token is still memory-only, on both builds.** It is minted from the refresh
//! credential in seconds and persisting it would widen the window a stolen copy is useful for,
//! for nothing.
//!
//! Nothing in this module derives `Debug`. The values it holds are credentials, and a derived
//! impl is how one reaches a log.

use cookie::Cookie;
use reqwest::cookie::{CookieStore, Jar};
use reqwest::header::HeaderValue;
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, OnceLock};

/// The credential-store account the session document is filed under, beside the service name in
/// `crate::platform`. One account, not one per server: the app talks to a single server at a
/// time (`platform::server_origin`), and a second entry would be a credential for a server the
/// reader has moved off and cannot see.
const CREDENTIAL_ACCOUNT: &str = "session";

/// What is written to the credential store.
///
/// The `Set-Cookie` lines are kept **verbatim**, keyed by cookie name, rather than reduced to the
/// token they carry. The name and `Path` this server issues depend on its own `cookie_secure`
/// setting — `__Host-refresh_token` at `/` when it is on, an unprefixed name at `/v1/auth` when
/// it is not — and a frontend that hard-coded either would be mirroring a constant it has no
/// compile-time relationship with. Replaying the line the server sent needs to know none of that.
#[derive(serde::Serialize, serde::Deserialize, Default)]
struct StoredSession {
    /// Serialised origin of the server these came from, so a document left behind by a server
    /// the reader has since re-pointed away from is discarded instead of replayed.
    origin: String,
    /// `Set-Cookie` lines by cookie name. Empty means "signed out", and is never written — the
    /// entry is deleted instead.
    cookies: BTreeMap<String, String>,
}

impl StoredSession {
    /// Fold one response's `Set-Cookie` lines into this document, reporting whether it changed.
    ///
    /// Pure, and separate from the credential store for that reason: rotation, removal and the
    /// server switch are the three cases that decide whether a reader stays signed in, and they
    /// are asserted below rather than reasoned about.
    fn record(&mut self, origin: &str, lines: &[&str]) -> bool {
        if lines.is_empty() {
            return false;
        }
        // A response from a different server than the one on file: its cookies are the whole
        // session now, and the previous server's are not ours to keep.
        let mut changed = self.origin != origin;
        if changed {
            self.cookies.clear();
            origin.clone_into(&mut self.origin);
        }
        for line in lines {
            let Ok(cookie) = Cookie::parse(*line) else {
                continue;
            };
            let name = cookie.name().to_owned();
            changed |= if is_removal(&cookie) {
                self.cookies.remove(&name).is_some()
            } else {
                self.cookies.insert(name, (*line).to_owned()).as_deref() != Some(*line)
            };
        }
        changed
    }
}

/// Whether this `Set-Cookie` clears the cookie rather than setting it.
///
/// The two forms this server uses: an empty value (its logout removal cookie) and a non-positive
/// `Max-Age`. An `Expires` in the past would be a third, but recognising it needs a clock and
/// nothing issues one — a stale line replayed into the jar is dropped there on the same test.
fn is_removal(cookie: &Cookie<'_>) -> bool {
    cookie.value().is_empty() || cookie.max_age().is_some_and(|age| !age.is_positive())
}

/// A `reqwest` cookie jar that mirrors the session cookie into the OS credential store.
///
/// The mirroring hangs off [`CookieStore::set_cookies`] deliberately. That is the one point every
/// response carrying a `Set-Cookie` passes through, so sign-in, the fifteen-minute rotation, the
/// passkey and email-verification paths and logout are all covered without a call at any of them
/// — and a route added later is covered without anyone remembering this file exists.
pub(crate) struct SessionJar {
    jar: Jar,
    stored: Mutex<StoredSession>,
}

impl SessionJar {
    /// Build the jar, replaying the stored session into it if one is on file for the server this
    /// app is currently pointed at.
    ///
    /// Blocking, and it has to be. This runs from `provide_api` during the first render, which is
    /// strictly before the boot-time silent refresh in [`crate::components::Shell`] fires — that
    /// ordering is the whole reason the refresh finds a cookie to present. Deferring the read to
    /// keep the first frame prompt would race it, and losing that race signs the reader out on a
    /// start where they should have stayed in. The cost is that a locked keyring's unlock prompt
    /// holds the window until it is answered.
    fn restore() -> Self {
        let jar = Jar::default();
        let current = normalised_origin(&crate::platform::origin());
        let stored = match read_document() {
            Some(stored) if Some(&stored.origin) == current.as_ref() => stored,
            // On file, but for another server. Delete rather than keep: it is a live credential
            // for a host this install no longer talks to, and nothing will ever come back for it.
            Some(_) => {
                crate::platform::credential_delete(CREDENTIAL_ACCOUNT);
                StoredSession::default()
            }
            None => StoredSession::default(),
        };
        if let Ok(url) = reqwest::Url::parse(&stored.origin) {
            for line in stored.cookies.values() {
                jar.add_cookie_str(line, &url);
            }
        }
        Self {
            jar,
            stored: Mutex::new(stored),
        }
    }

    /// Forget the persisted session, for when the app has concluded there is no longer one —
    /// a `401` from refresh, a sign-out, a deleted account, a re-pointed server.
    ///
    /// Only the persisted half: the in-memory jar keeps whatever it holds, which is either
    /// already cleared by the server's removal cookie or a dead token the next sign-in overwrites
    /// under the same name.
    pub(crate) fn forget(&self) {
        if let Ok(mut stored) = self.stored.lock() {
            *stored = StoredSession::default();
        }
        crate::platform::credential_delete(CREDENTIAL_ACCOUNT);
    }

    /// Fold `lines` into the stored document and write it out if it moved.
    fn persist(&self, lines: &[&str], url: &reqwest::Url) {
        let Ok(mut stored) = self.stored.lock() else {
            return;
        };
        if !stored.record(&url.origin().ascii_serialization(), lines) {
            return;
        }
        if stored.cookies.is_empty() {
            crate::platform::credential_delete(CREDENTIAL_ACCOUNT);
        } else if let Ok(document) = serde_json::to_string(&*stored) {
            crate::platform::credential_set(CREDENTIAL_ACCOUNT, &document);
        }
    }
}

// `reqwest::Url` rather than `url::Url` throughout: it is the same type re-exported, and naming
// it this way keeps `url` out of this crate's direct dependencies for one signature.
impl CookieStore for SessionJar {
    fn set_cookies(
        &self,
        cookie_headers: &mut dyn Iterator<Item = &HeaderValue>,
        url: &reqwest::Url,
    ) {
        // Collected because the iterator is single-pass and both the jar and the credential store
        // need it. Non-UTF-8 headers are dropped here only — the jar still receives them, since
        // what it can parse is its own business.
        let headers: Vec<&HeaderValue> = cookie_headers.collect();
        self.jar.set_cookies(&mut headers.iter().copied(), url);

        let lines: Vec<&str> = headers.iter().filter_map(|h| h.to_str().ok()).collect();
        self.persist(&lines, url);
    }

    fn cookies(&self, url: &reqwest::Url) -> Option<HeaderValue> {
        self.jar.cookies(url)
    }
}

/// The process's one jar, shared by every client `super::build_http_client` makes.
///
/// **Shared, not per-client, and that is the whole point.** `build_http_client` runs again every
/// time the access token changes — which is every fifteen minutes, by design — and a jar owned by
/// the client would be discarded with it, taking the refresh cookie the *next* refresh depends
/// on. The session would then end at the first expiry after sign-in.
pub(crate) fn session_jar() -> Arc<SessionJar> {
    static JAR: OnceLock<Arc<SessionJar>> = OnceLock::new();
    Arc::clone(JAR.get_or_init(|| Arc::new(SessionJar::restore())))
}

/// The stored document, if the credential store has one that still parses.
fn read_document() -> Option<StoredSession> {
    let document = crate::platform::credential_get(CREDENTIAL_ACCOUNT)?;
    serde_json::from_str(&document).ok()
}

/// `origin` as a URL origin, or `None` when it is empty or unparseable — first run, or a settings
/// file that has been edited into nonsense.
///
/// Compared in this form rather than as the reader typed it: `https://tanko.example:443` and
/// `https://tanko.example` are the same server, and a string comparison would throw the
/// credential away on the next start for a port number.
fn normalised_origin(origin: &str) -> Option<String> {
    reqwest::Url::parse(origin)
        .ok()
        .map(|url| url.origin().ascii_serialization())
}

#[cfg(test)]
mod tests {
    use super::StoredSession;

    const ORIGIN: &str = "https://tanko.example";
    const SET: &str = "__Host-refresh_token=first; HttpOnly; Secure; SameSite=Strict; Path=/; \
                       Max-Age=2592000";
    const ROTATED: &str = "__Host-refresh_token=second; HttpOnly; Secure; SameSite=Strict; \
                           Path=/; Max-Age=2592000";
    const REMOVAL: &str = "__Host-refresh_token=; HttpOnly; Secure; SameSite=Strict; Path=/";

    #[test]
    fn a_set_cookie_is_recorded_verbatim_under_its_name() {
        let mut stored = StoredSession::default();
        assert!(stored.record(ORIGIN, &[SET]));
        assert_eq!(stored.origin, ORIGIN);
        assert_eq!(stored.cookies["__Host-refresh_token"], SET);
    }

    /// Rotation is the case that decides whether the reader is still signed in tomorrow: the
    /// server hands out a new refresh token every fifteen minutes and revokes the one it
    /// replaced, so a document that kept the *first* line would persist a token that is already
    /// dead and 401 on the next start.
    #[test]
    fn a_rotated_cookie_replaces_the_one_it_supersedes() {
        let mut stored = StoredSession::default();
        stored.record(ORIGIN, &[SET]);
        assert!(stored.record(ORIGIN, &[ROTATED]));
        assert_eq!(stored.cookies.len(), 1);
        assert_eq!(stored.cookies["__Host-refresh_token"], ROTATED);
    }

    /// Re-recording the same line must not report a change, or every response would rewrite the
    /// credential store.
    #[test]
    fn an_unchanged_cookie_reports_no_change() {
        let mut stored = StoredSession::default();
        stored.record(ORIGIN, &[SET]);
        assert!(!stored.record(ORIGIN, &[SET]));
    }

    /// Logout clears the cookie by setting it empty rather than by any header naming a deletion.
    /// Read as an ordinary value that would leave the revoked token on file, and the next start
    /// would replay a credential the reader explicitly signed out of.
    #[test]
    fn a_removal_cookie_drops_the_stored_line() {
        let mut stored = StoredSession::default();
        stored.record(ORIGIN, &[SET]);
        assert!(stored.record(ORIGIN, &[REMOVAL]));
        assert!(stored.cookies.is_empty(), "an empty value clears the entry");
    }

    #[test]
    fn a_non_positive_max_age_also_drops_it() {
        let mut stored = StoredSession::default();
        stored.record(ORIGIN, &[SET]);
        assert!(stored.record(ORIGIN, &["__Host-refresh_token=stale; Path=/; Max-Age=0"]));
        assert!(stored.cookies.is_empty());
    }

    /// Re-pointing the app at another server must not leave the previous one's credential in a
    /// document that now claims to describe the new one — the next start would replay it against
    /// the wrong host.
    #[test]
    fn a_response_from_another_origin_replaces_the_whole_document() {
        let mut stored = StoredSession::default();
        stored.record(ORIGIN, &[SET]);
        assert!(stored.record("https://other.example", &[ROTATED]));
        assert_eq!(stored.origin, "https://other.example");
        assert_eq!(stored.cookies.len(), 1);
        assert_eq!(stored.cookies["__Host-refresh_token"], ROTATED);
    }

    /// Most responses carry no `Set-Cookie` at all; they must not be mistaken for a server
    /// switch and wipe the session.
    #[test]
    fn a_response_with_no_set_cookie_changes_nothing() {
        let mut stored = StoredSession::default();
        stored.record(ORIGIN, &[SET]);
        assert!(!stored.record("https://other.example", &[]));
        assert_eq!(stored.origin, ORIGIN);
        assert_eq!(stored.cookies.len(), 1);
    }
}
