//! The innermost fetcher: `wreq` with a browser emulation profile, a validating
//! (SSRF-safe) DNS resolver, per-hop redirect scheme validation, and a bounded text body.
//!
//! Emulation exists because providers fingerprint the TLS `ClientHello` and HTTP/2 frames and
//! cross-check them against `User-Agent` — a browser UA over a non-browser handshake is a
//! *stronger* bot signal than no disguise. `wreq`'s profile owns the matching header set, so
//! this layer must never hand-set a header a profile owns.
//!
//! A solved challenge session is the one case that sets a user-agent per request, because its
//! clearance cookies are bound to it. That user-agent therefore arrives with a **client of its
//! own**: the profile is resolved from the user-agent itself, so the handshake we present is the
//! one the session was earned with. A session whose browser no profile reproduces is not
//! impersonated at all — see [`client_for`].

use crate::error::FetchError;
use crate::fetcher::Fetcher;
use crate::identity::{BrowserIdentity, BrowserPlatform};
use crate::ssrf::{self, SsrfResolver};
use crate::types::{FetchRequest, FetchResponse};
use async_trait::async_trait;
use futures::StreamExt;
use std::sync::{Arc, RwLock};
use std::time::Duration;
use tankovault_domain::BrowserEmulation;
use url::Url;
use wreq_util::{Emulation, Platform, Profile};

/// Maximum redirects followed before erroring.
const MAX_REDIRECTS: usize = 5;
/// Hard cap on response body size (guards memory and the no-binary-content invariant).
///
/// Raised from 8 MiB, which was inconsistent rather than merely tight: a sitemap shard that the
/// **solver** path hands back at 26 MB — it returns the browser's rendered body with no cap at
/// all — was refused outright when the same document arrived on the plain path at 2.9 MB raw
/// and growing. One transport accepting a document the other rejects makes a provider's
/// catalogue depend on whether its session happened to be cached, which is not a property
/// anything upstream can reason about.
///
/// The bound that matters is concurrency × this value: `Politeness::MAX_CONCURRENCY` is 16
/// in-flight requests per provider, so a provider at the ceiling can hold 256 MiB of bodies at
/// once. That is inside the worker's compose memory limit, and no provider comes close in
/// practice — only sitemap shards are anywhere near this size, and they are fetched one at a
/// time by a catalogue walk.
const MAX_BODY_BYTES: usize = 16 * 1024 * 1024;

/// Resolve a domain-level browser family to a concrete emulation profile.
///
/// The newest build in each family is chosen deliberately: fingerprints of long-superseded
/// releases are themselves anomalous. Bumping `wreq-util` moves every provider forward
/// without a database migration — which is why [`BrowserEmulation`] names families, not
/// versions.
fn profile_for(browser: BrowserEmulation) -> Emulation {
    let (profile, platform) = match browser {
        BrowserEmulation::Chrome => (Profile::Chrome149, Platform::Windows),
        BrowserEmulation::Firefox => (Profile::Firefox151, Platform::Windows),
        BrowserEmulation::Safari => (Profile::Safari26_4, Platform::MacOS),
        BrowserEmulation::Edge => (Profile::Edge148, Platform::Windows),
        BrowserEmulation::OkHttp => (Profile::OkHttp5, Platform::Android),
    };
    Emulation::builder()
        .profile(profile)
        .platform(platform)
        .build()
}

/// Every build this workspace can reproduce, newest first within a family.
///
/// The table exists so a **solved session** can be replayed by a client that presents the browser
/// that earned it. Clearance is bound to the handshake as well as the user-agent, so an entry is
/// matched on the exact major and an unmatched major declines: "Firefox 150's user-agent over
/// Firefox 151's `ClientHello`" is precisely the contradiction this prevents, not a near-enough
/// answer. Declining costs a solve per fetch and stays coherent; approximating spends the solve
/// *and* announces itself.
const PROFILES: &[(BrowserEmulation, u32, Profile)] = &[
    (BrowserEmulation::Chrome, 149, Profile::Chrome149),
    (BrowserEmulation::Chrome, 148, Profile::Chrome148),
    (BrowserEmulation::Chrome, 147, Profile::Chrome147),
    (BrowserEmulation::Chrome, 146, Profile::Chrome146),
    (BrowserEmulation::Chrome, 145, Profile::Chrome145),
    (BrowserEmulation::Chrome, 144, Profile::Chrome144),
    (BrowserEmulation::Chrome, 143, Profile::Chrome143),
    (BrowserEmulation::Firefox, 151, Profile::Firefox151),
    (BrowserEmulation::Firefox, 150, Profile::Firefox150),
    (BrowserEmulation::Firefox, 149, Profile::Firefox149),
    (BrowserEmulation::Firefox, 148, Profile::Firefox148),
    (BrowserEmulation::Firefox, 147, Profile::Firefox147),
    (BrowserEmulation::Firefox, 146, Profile::Firefox146),
    (BrowserEmulation::Edge, 148, Profile::Edge148),
    (BrowserEmulation::Edge, 147, Profile::Edge147),
    (BrowserEmulation::Edge, 146, Profile::Edge146),
    (BrowserEmulation::Edge, 145, Profile::Edge145),
    (BrowserEmulation::Edge, 144, Profile::Edge144),
    (BrowserEmulation::Edge, 143, Profile::Edge143),
    (BrowserEmulation::Safari, 26, Profile::Safari26_4),
    (BrowserEmulation::Safari, 18, Profile::Safari18_5),
    (BrowserEmulation::OkHttp, 5, Profile::OkHttp5),
];

/// Which platforms a family has a profile for, mirroring `wreq-util`'s own per-family platform
/// list. A combination outside it (Firefox on Android, Safari anywhere but macOS) would build an
/// emulation whose headers and handshake disagree about the operating system.
const fn supports(family: BrowserEmulation, platform: BrowserPlatform) -> bool {
    match family {
        BrowserEmulation::Chrome => matches!(
            platform,
            BrowserPlatform::Windows
                | BrowserPlatform::MacOs
                | BrowserPlatform::Linux
                | BrowserPlatform::Android
        ),
        BrowserEmulation::Firefox => matches!(
            platform,
            BrowserPlatform::Windows | BrowserPlatform::MacOs | BrowserPlatform::Linux
        ),
        BrowserEmulation::Edge => {
            matches!(platform, BrowserPlatform::Windows | BrowserPlatform::MacOs)
        }
        // Desktop Safari only: iOS is a separate family of profiles in `wreq-util`, and nothing
        // in this workspace solves with one.
        BrowserEmulation::Safari => matches!(platform, BrowserPlatform::MacOs),
        BrowserEmulation::OkHttp => matches!(platform, BrowserPlatform::Android),
    }
}

/// The emulation that reproduces `identity`, or `None` when this workspace cannot present that
/// browser — in which case the caller must not present its user-agent either.
fn emulation_for(identity: BrowserIdentity) -> Option<Emulation> {
    if !supports(identity.family, identity.platform) {
        return None;
    }
    let profile = PROFILES
        .iter()
        .find(|(family, major, _)| *family == identity.family && *major == identity.major)
        .map(|(_, _, profile)| *profile)?;
    let platform = match identity.platform {
        BrowserPlatform::Windows => Platform::Windows,
        BrowserPlatform::MacOs => Platform::MacOS,
        BrowserPlatform::Linux => Platform::Linux,
        BrowserPlatform::Android => Platform::Android,
        BrowserPlatform::Ios => Platform::IOS,
    };
    Some(
        Emulation::builder()
            .profile(profile)
            .platform(platform)
            .build(),
    )
}

/// Whether this workspace can present `user_agent` over a handshake that matches it.
///
/// The question [`crate::solving`] asks before caching a solved session: a session whose browser
/// has no profile can still be replayed, but only with its user-agent dropped, and cookies bound
/// to a user-agent that is no longer sent are cookies that will not be honoured.
pub(crate) fn can_reproduce(user_agent: &str) -> bool {
    BrowserIdentity::from_user_agent(user_agent)
        .is_some_and(|identity| emulation_for(identity).is_some())
}

/// Client policy that every client this fetcher builds shares — the SSRF resolver, the redirect
/// rule and the timeouts. Retained because a solved session's client is built after construction
/// and must be identical to the default one in every respect except the emulation profile.
struct ClientPolicy {
    connect_timeout: Duration,
    request_timeout: Duration,
}

fn build_client(
    emulation: Option<Emulation>,
    policy: &ClientPolicy,
) -> Result<wreq::Client, FetchError> {
    let redirect = wreq::redirect::Policy::custom(|attempt| {
        if attempt.previous.len() >= MAX_REDIRECTS {
            return attempt.error("too many redirects");
        }
        // `wreq` 6 carries a `http::Uri` here rather than a `url::Url`, and `Uri::scheme`
        // is `Option<&Scheme>` — a scheme-relative or malformed target yields `None`, which
        // must fall to `stop()` with the other non-HTTP schemes rather than being unwrapped.
        // Resolved before the branch because `follow`/`stop` consume `attempt`.
        let http_scheme = matches!(attempt.uri.scheme_str(), Some("http" | "https"));
        if http_scheme {
            attempt.follow()
        } else {
            attempt.stop()
        }
    });

    let mut builder = wreq::Client::builder()
        .dns_resolver(Arc::new(SsrfResolver))
        .redirect(redirect)
        .connect_timeout(policy.connect_timeout)
        .timeout(policy.request_timeout);
    if let Some(emulation) = emulation {
        builder = builder.emulation(emulation);
    }
    builder.build().map_err(|e| FetchError::Transport(e.to_string()))
}

/// `wreq`-backed base fetcher. One per provider (carries that provider's identity).
pub struct BaseHttpFetcher {
    client: wreq::Client,
    /// The identifiable bot user-agent, present **iff** emulation is off. `None` therefore
    /// means "a profile owns the identity headers", which is also what gates the default
    /// `Accept`/`Accept-Language` below — one field, so the two can never disagree.
    bot_user_agent: Option<String>,
    /// Clients built to match a solved session's browser, tagged with the identity each
    /// reproduces.
    ///
    /// A list, not a map: a provider's sessions come from one solver, whose browser varies over
    /// the handful of builds and platforms it rotates through, so this holds single digits and a
    /// scan beats hashing.
    session_clients: RwLock<Vec<(BrowserIdentity, wreq::Client)>>,
    /// Retained so a session client is built with the policy the default one has.
    policy: ClientPolicy,
}

impl BaseHttpFetcher {
    /// Build a base fetcher.
    ///
    /// `emulation` selects the browser to impersonate; `None` crawls as an identifiable bot
    /// sending `default_user_agent` verbatim. `default_user_agent` is ignored whenever
    /// `emulation` is `Some`, because the profile's user-agent is the one that matches the
    /// handshake.
    ///
    /// # Errors
    /// Returns [`FetchError::Transport`] if the underlying client cannot be built.
    pub fn new(
        default_user_agent: impl Into<String>,
        emulation: Option<BrowserEmulation>,
        connect_timeout: Duration,
        request_timeout: Duration,
    ) -> Result<Self, FetchError> {
        let policy = ClientPolicy {
            connect_timeout,
            request_timeout,
        };
        let client = build_client(emulation.map(profile_for), &policy)?;

        Ok(Self {
            client,
            bot_user_agent: emulation.is_none().then(|| default_user_agent.into()),
            session_clients: RwLock::new(Vec::new()),
            policy,
        })
    }

    /// The client to send `req` with, and the user-agent to set on it.
    ///
    /// A solved session's user-agent is only presented by a client that reproduces the browser it
    /// names. When no profile does — an unknown family, a build newer than the table, a
    /// family/platform pair `wreq-util` has no profile for — the user-agent is **dropped** rather
    /// than sent over a contradicting handshake, and the request goes out as this provider's own
    /// configured identity. Its clearance cookies will not be honoured; that is the honest
    /// outcome, and [`crate::solving`] is where the session is refused before it gets this far.
    fn client_for(&self, session_user_agent: Option<&str>) -> (wreq::Client, Option<String>) {
        let Some(user_agent) = session_user_agent else {
            return (self.client.clone(), self.bot_user_agent.clone());
        };
        // Not emulating: no profile owns the identity, so a session user-agent is sent as-is.
        if self.bot_user_agent.is_some() {
            return (self.client.clone(), Some(user_agent.to_owned()));
        }

        if let Some(client) = self.matched_client(user_agent) {
            return (client, Some(user_agent.to_owned()));
        }
        tracing::debug!(
            %user_agent,
            "no emulation profile reproduces this session's browser; dropping its user-agent \
             rather than contradicting the handshake"
        );
        (self.client.clone(), None)
    }

    /// The client that presents `user_agent`'s browser, building and caching one on first use, or
    /// `None` when no profile reproduces it.
    fn matched_client(&self, user_agent: &str) -> Option<wreq::Client> {
        let identity = BrowserIdentity::from_user_agent(user_agent)?;
        if let Some((_, client)) = self
            .session_clients
            .read()
            .expect("session client cache is not poisoned")
            .iter()
            .find(|(cached, _)| *cached == identity)
        {
            return Some(client.clone());
        }

        let emulation = emulation_for(identity)?;
        match build_client(Some(emulation), &self.policy) {
            Ok(client) => {
                self.session_clients
                    .write()
                    .expect("session client cache is not poisoned")
                    .push((identity, client.clone()));
                Some(client)
            }
            // A client that will not build is a broken binary, not a property of this request, and
            // the provider's own client is still usable — so carry on without the session identity
            // rather than failing a fetch that can still succeed unauthenticated.
            Err(e) => {
                tracing::warn!(error = %e, "could not build a session-matched client");
                None
            }
        }
    }
}

#[async_trait]
impl Fetcher for BaseHttpFetcher {
    /// Times and counts the request around [`Self::send`].
    ///
    /// This is the workspace's only choke point for provider traffic — every adapter, every
    /// scan tier and both solver paths come through here — so it is where "is this provider
    /// still answering us" becomes a number instead of a log line.
    async fn get(&self, req: FetchRequest) -> Result<FetchResponse, FetchError> {
        let provider = req.provider_slug.clone();
        let started = std::time::Instant::now();
        let result = self.send(req).await;

        let outcome = match &result {
            Ok(resp) => status_class(resp.status),
            Err(_) => "error",
        };
        metrics::counter!(
            "provider_fetch_total",
            "provider" => provider.clone(),
            "outcome" => outcome,
        )
        .increment(1);
        let elapsed = started.elapsed();
        metrics::histogram!("provider_fetch_duration_seconds", "provider" => provider)
            .record(elapsed.as_secs_f64());
        // Counted even when the request failed: a scan task that spent four minutes on requests
        // that all timed out has spent four minutes, and a breakdown that omitted them would
        // report the time as unaccounted for.
        crate::accounting::record(crate::accounting::Metered::Request(elapsed));

        result
    }
}

impl BaseHttpFetcher {
    async fn send(&self, req: FetchRequest) -> Result<FetchResponse, FetchError> {
        let url = Url::parse(&req.url).map_err(|_| FetchError::InvalidUrl(req.url.clone()))?;
        // Cheap pre-flight; the resolver enforces the address-range check at connect time.
        ssrf::validate_url(&url)?;

        // `.as_str()`: `wreq` 6 takes `IntoUri` (a `http::Uri`), which `url::Url` does not
        // implement. Going through the string is the lossless hop — `Url` has already
        // normalised and, above, been SSRF-validated in that form.
        // The client and the user-agent are chosen together, so the handshake and the identity
        // header can never describe different browsers.
        let (client, user_agent) = self.client_for(req.user_agent.as_deref());
        let mut builder = client.get(url.as_str());
        if let Some(ua) = user_agent {
            builder = builder.header(wreq::header::USER_AGENT, ua);
        }
        if self.bot_user_agent.is_some() {
            // Not emulating: no profile owns the content-negotiation headers, so supply
            // plausible defaults. Under emulation these come from the profile and must not
            // be overwritten.
            builder = builder
                .header(
                    wreq::header::ACCEPT,
                    "text/html,application/xhtml+xml,application/xml;q=0.9,application/json;q=0.8,*/*;q=0.5",
                )
                .header(wreq::header::ACCEPT_LANGUAGE, "en-US,en;q=0.8");
        }
        for (k, v) in &req.headers {
            builder = builder.header(k, v);
        }

        let resp = builder.send().await.map_err(map_wreq_err)?;

        let status = resp.status().as_u16();
        let final_url = resp.uri().to_string();
        // Every header is materialised, deliberately not an allowlist of the few call sites
        // happen to read: `FetchResponse::headers` is documented as "response headers", and
        // narrowing it would make that a silent lie for whatever a future reader adds.
        let mut headers = Vec::with_capacity(resp.headers().len());
        headers.extend(resp.headers().iter().map(|(k, v)| {
            (
                k.as_str().to_owned(),
                v.to_str().unwrap_or_default().to_owned(),
            )
        }));

        // Pre-size from `Content-Length` when declared, clamped to the cap so a hostile header
        // cannot force an 8 MiB allocation for a 200-byte body.
        //
        // The header is a hint, never a bound: the streaming check below is what actually
        // enforces `MAX_BODY_BYTES`, since a server is free to send more than it announced.
        let declared = resp
            .content_length()
            .and_then(|n| usize::try_from(n).ok())
            .map_or(0, |n| n.min(MAX_BODY_BYTES));

        // Stream the body with a hard byte cap.
        let mut stream = resp.bytes_stream();
        let mut buf: Vec<u8> = Vec::with_capacity(declared);
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.map_err(map_wreq_err)?;
            if buf.len() + chunk.len() > MAX_BODY_BYTES {
                return Err(FetchError::BodyTooLarge);
            }
            buf.extend_from_slice(&chunk);
        }
        // `from_utf8` first so the happy path *moves* the buffer instead of copying up to
        // 8 MiB; `from_utf8_lossy` handles the rare body with a bad byte.
        let body = String::from_utf8(buf)
            .unwrap_or_else(|e| String::from_utf8_lossy(e.as_bytes()).into_owned());

        Ok(FetchResponse {
            status,
            url: final_url,
            headers,
            body,
            from_cache: false,
        })
    }
}

/// Fold a status into one of five label values.
///
/// The class, not the code: `provider_fetch_total` is already labelled by provider, and a
/// provider crossed with every status a hostile or broken site can return is an unbounded
/// label source for a distinction no panel makes.
fn status_class(status: u16) -> &'static str {
    match status {
        200..=299 => "2xx",
        300..=399 => "3xx",
        400..=499 => "4xx",
        500..=599 => "5xx",
        _ => "other",
    }
}

#[expect(
    clippy::needless_pass_by_value,
    reason = "an owned-error mapper, passed directly to `.map_err`"
)]
fn map_wreq_err(e: wreq::Error) -> FetchError {
    if e.is_timeout() {
        FetchError::Timeout
    } else if e.is_redirect() {
        FetchError::TooManyRedirects
    } else {
        FetchError::Transport(e.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn build(emulation: Option<BrowserEmulation>) -> BaseHttpFetcher {
        BaseHttpFetcher::new(
            "TankoVaultBot/0.1",
            emulation,
            Duration::from_secs(1),
            Duration::from_secs(1),
        )
        .expect("client builds")
    }

    /// The profile owns `User-Agent`; overriding it with the configured bot string would
    /// contradict the TLS fingerprint, so the field must be dropped when emulating.
    #[test]
    fn emulated_client_defers_to_the_profile_user_agent() {
        assert!(
            build(Some(BrowserEmulation::Chrome))
                .bot_user_agent
                .is_none()
        );
    }

    #[test]
    fn unemulated_client_sends_the_configured_bot_user_agent() {
        assert_eq!(
            build(None).bot_user_agent.as_deref(),
            Some("TankoVaultBot/0.1")
        );
    }

    /// A per-request user-agent (a solved challenge session) still wins — the clearance cookie is
    /// bound to the user-agent the solver's browser presented — but only together with a client
    /// that reproduces that browser.
    #[test]
    fn a_session_user_agent_is_sent_with_a_client_that_matches_it() {
        let f = build(Some(BrowserEmulation::Chrome));
        let firefox = "Mozilla/5.0 (Windows NT 10.0; Win64; x64; rv:150.0) \
                       Gecko/20100101 Firefox/150.0";
        let (_, sent) = f.client_for(Some(firefox));
        assert_eq!(sent.as_deref(), Some(firefox));

        // And the client is reused rather than rebuilt per request.
        let _ = f.client_for(Some(firefox));
        assert_eq!(f.session_clients.read().expect("not poisoned").len(), 1);
    }

    /// A session whose browser no profile reproduces must not have its user-agent sent at all.
    ///
    /// This is the defect the whole identity table exists for. The deployed solver is Camoufox —
    /// a **Firefox**, which rotates the platform it presents per solve — while every provider
    /// defaults to a Chrome profile. Replaying such a session used to mean a Firefox user-agent
    /// over a Chrome `ClientHello` with `sec-ch-ua` headers Firefox does not implement: three
    /// contradictions at once, and a stronger bot signal than crawling honestly. When no profile
    /// matches, the honest request is the provider's own identity and a failed clearance.
    #[test]
    fn an_unreproducible_session_user_agent_is_dropped_rather_than_contradicted() {
        let f = build(Some(BrowserEmulation::Chrome));
        for unreproducible in [
            // A build newer than the table.
            "Mozilla/5.0 (Windows NT 10.0; Win64; x64; rv:999.0) Gecko/20100101 Firefox/999.0",
            // A family/platform pair with no profile: Firefox on Android.
            "Mozilla/5.0 (Android 14; Mobile; rv:150.0) Gecko/20100101 Firefox/150.0",
            // Not a browser at all.
            "SolverUA/2.0",
        ] {
            let (_, sent) = f.client_for(Some(unreproducible));
            assert_eq!(sent, None, "{unreproducible} was presented anyway");
        }
        assert!(
            f.session_clients.read().expect("not poisoned").is_empty(),
            "a client was built for a browser we cannot reproduce"
        );
    }

    /// Without emulation nothing owns the identity, so a session user-agent is sent verbatim —
    /// there is no handshake for it to contradict.
    #[test]
    fn an_unemulated_client_sends_a_session_user_agent_as_it_is() {
        let f = build(None);
        let (_, sent) = f.client_for(Some("SolverUA/2.0"));
        assert_eq!(sent.as_deref(), Some("SolverUA/2.0"));
        let (_, default) = f.client_for(None);
        assert_eq!(default.as_deref(), Some("TankoVaultBot/0.1"));
    }

    /// The solver's own user-agents, captured 2026-08-12, resolve to a profile — the point of the
    /// exercise. If `wreq-util` ever drops the Firefox build Camoufox ships, this fails here
    /// rather than silently degrading every provider to a solve per fetch.
    #[test]
    fn the_deployed_solvers_browser_has_a_profile() {
        for ua in [
            "Mozilla/5.0 (Windows NT 10.0; Win64; x64; rv:150.0) Gecko/20100101 Firefox/150.0",
            "Mozilla/5.0 (Macintosh; Intel Mac OS X 10.15; rv:150.0) Gecko/20100101 Firefox/150.0",
        ] {
            let identity = BrowserIdentity::from_user_agent(ua).expect("parses");
            assert!(
                emulation_for(identity).is_some(),
                "no profile reproduces {identity:?}"
            );
        }
    }

    #[test]
    fn every_family_resolves_to_a_profile() {
        for browser in [
            BrowserEmulation::Chrome,
            BrowserEmulation::Firefox,
            BrowserEmulation::Safari,
            BrowserEmulation::Edge,
            BrowserEmulation::OkHttp,
        ] {
            let _ = profile_for(browser);
            assert!(build(Some(browser)).bot_user_agent.is_none());
        }
    }
}
