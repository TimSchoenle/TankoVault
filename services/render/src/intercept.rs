//! Owns applying the SSRF policy to every network request the browser makes, through CDP's `Fetch`
//! domain on the browser target.
//!
//! The pre-flight check on the URL a caller names covers one request. Chromium then follows
//! redirects, loads subresources and runs page script, each of which is a fetch to a URL the page
//! chose. Interception enabled on the browser target pauses all of them, in every tab, before a
//! connection is made; each is continued only once its host has resolved to public addresses.
//!
//! What this does not cover, and the compose network isolation exists for: traffic the `Fetch`
//! domain never sees (WebSocket, WebRTC, `<link rel=preconnect>`), and a DNS rebind between this
//! process's lookup and Chromium's own (`SECURITY.md`).

use std::sync::{Arc, Weak};

use chromiumoxide::browser::Browser;
use chromiumoxide::cdp::browser_protocol::fetch::{
    ContinueRequestParams, EnableParams, EventRequestPaused, FailRequestParams,
};
use chromiumoxide::cdp::browser_protocol::network::ErrorReason;
use futures::StreamExt;
use tankovault_domain::ssrf::{self, SsrfError};
use tokio::sync::Semaphore;

/// What to do with one paused request before any DNS is consulted.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Plan {
    /// Makes no network connection (`data:`, `blob:`, `about:`), so there is nothing to vet.
    Continue,
    /// Refused on the URL alone: a disallowed scheme, no host, or a forbidden IP literal.
    Fail(SsrfError),
    /// A hostname: continue only if every address it resolves to is public.
    Resolve(String),
}

/// Decides a request URL as far as is possible without I/O.
pub(crate) fn plan(raw: &str) -> Plan {
    if let Some((scheme, _)) = raw.split_once(':')
        && matches!(
            scheme.to_ascii_lowercase().as_str(),
            "data" | "blob" | "about"
        )
    {
        return Plan::Continue;
    }
    match ssrf::validate_str(raw) {
        Err(e) => Plan::Fail(e),
        Ok(url) => url
            .domain()
            .map_or(Plan::Continue, |domain| Plan::Resolve(domain.to_owned())),
    }
}

/// Caps lookups in flight: `getaddrinfo` runs on the blocking pool and cannot be cancelled, so a
/// page firing requests at slow-resolving names could otherwise exhaust that pool and stall every
/// render's pre-flight check.
static LOOKUPS: Semaphore = Semaphore::const_new(32);

/// The full per-request decision: [`plan`], then a lookup when the host is a name.
///
/// # Errors
/// Returns the [`SsrfError`] that refuses the request.
pub(crate) async fn admit(raw: &str) -> Result<(), SsrfError> {
    match plan(raw) {
        Plan::Continue => Ok(()),
        Plan::Fail(e) => Err(e),
        Plan::Resolve(host) => {
            let _slot = LOOKUPS
                .acquire()
                .await
                .map_err(|_| SsrfError::Resolution(host.clone()))?;
            let addrs = ssrf::lookup(&host).await?;
            ssrf::require_all_public(&host, &addrs)
        }
    }
}

/// Enables interception on `browser` and answers every paused request until its connection
/// ends.
///
/// Must complete before the browser is handed out: a page that navigates before this returns is
/// not intercepted. The task holds only a [`Weak`] so it does not keep a replaced browser alive.
///
/// # Errors
/// Returns an error when the listener cannot be registered or `Fetch.enable` is refused.
pub(crate) async fn install(browser: &Arc<Browser>) -> anyhow::Result<()> {
    // Listener first, so no request paused between the two calls goes unanswered.
    let mut paused = browser.event_listener::<EventRequestPaused>().await?;
    browser.execute(EnableParams::default()).await?;

    let weak = Arc::downgrade(browser);
    tokio::spawn(async move {
        while let Some(event) = paused.next().await {
            // Each request resolves on its own task, so a slow lookup never stalls the stream.
            tokio::spawn(answer(Weak::clone(&weak), event));
        }
    });
    Ok(())
}

async fn answer(browser: Weak<Browser>, event: Arc<EventRequestPaused>) {
    let verdict = admit(&event.request.url).await;
    let Some(browser) = browser.upgrade() else {
        return;
    };
    let id = event.request_id.clone();
    let sent = match verdict {
        Ok(()) => browser
            .execute(ContinueRequestParams::new(id))
            .await
            .map(drop),
        Err(e) => {
            tracing::warn!(url = %event.request.url, error = %e, "blocked a browser request");
            browser
                .execute(FailRequestParams::new(id, ErrorReason::BlockedByClient))
                .await
                .map(drop)
        }
    };
    if let Err(e) = sent {
        tracing::debug!(error = %e, "could not answer a paused request");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn local_schemes_make_no_connection_and_pass() {
        assert_eq!(plan("data:text/plain,hi"), Plan::Continue);
        assert_eq!(plan("blob:https://example.com/uuid"), Plan::Continue);
        assert_eq!(plan("about:blank"), Plan::Continue);
    }

    /// Regression: only the caller's URL was checked, so a redirect or subresource pointing at
    /// an internal literal (the metadata endpoint, a compose peer's IP) was fetched unchecked.
    #[test]
    fn forbidden_literals_and_schemes_fail_without_a_lookup() {
        for raw in [
            "http://169.254.169.254/latest/meta-data/",
            "http://127.0.0.1:6379/",
            "http://[::1]/",
            "http://2130706433/",
            "file:///etc/passwd",
            "ftp://example.com/",
        ] {
            assert!(matches!(plan(raw), Plan::Fail(_)), "{raw} should fail");
        }
    }

    #[test]
    fn public_literals_continue_and_names_resolve() {
        assert_eq!(plan("https://1.1.1.1/x"), Plan::Continue);
        assert_eq!(
            plan("https://cdn.example.com/a.js"),
            Plan::Resolve("cdn.example.com".to_owned())
        );
        assert_eq!(
            plan("http://redis:6379/"),
            Plan::Resolve("redis".to_owned())
        );
    }

    /// Regression: a hostname was never resolved, so `localhost` and compose service names
    /// reached internal listeners through the browser.
    #[tokio::test]
    async fn a_name_resolving_to_loopback_is_refused() {
        assert!(matches!(
            admit("http://localhost:8084/").await,
            Err(SsrfError::ForbiddenAddress(_))
        ));
    }
}
