//! Owns the innermost fetcher: `wreq` with a browser emulation profile, an SSRF-safe DNS
//! resolver, per-hop redirect scheme and address validation, and a bounded text body.
//!
//! Providers fingerprint the TLS `ClientHello` and HTTP/2 frames and cross-check them against
//! `User-Agent`, so a browser user agent over a non-browser handshake is a stronger bot signal
//! than no disguise. `wreq`'s profile owns the matching header set, and this layer must never
//! hand-set a header a profile owns.
//!
//! A solved challenge session is the one case that sets a user agent per request, because its
//! clearance cookies are bound to it. That user agent gets a **client of its own**, with the
//! profile resolved from the user agent, so the handshake is the one the session was earned
//! with. A session whose browser no profile reproduces is not impersonated: see
//! [`client_for`](BaseHttpFetcher::client_for).

use crate::error::FetchError;
use crate::fetcher::Fetcher;
use crate::identity::{BrowserIdentity, BrowserPlatform};
use crate::ssrf::{self, SsrfResolver};
use crate::types::{FetchRequest, FetchResponse};
use async_trait::async_trait;
use flate2::read::MultiGzDecoder;
use futures::StreamExt;
use std::io::Read;
use std::sync::{Arc, RwLock};
use std::time::Duration;
use tankovault_domain::BrowserEmulation;
use url::Url;
use wreq_util::{Emulation, Platform, Profile};

/// The most redirects followed before erroring.
const MAX_REDIRECTS: usize = 5;
/// The hard cap on response body size, which guards memory and the no-binary-content invariant.
///
/// The solver path returns the browser's rendered body uncapped, so a cap much tighter than
/// the largest sitemap shard would make a provider's catalogue depend on whether its session
/// happened to be cached.
///
/// The bound that matters is concurrency times this value: `Politeness::MAX_CONCURRENCY` is 16
/// in-flight requests per provider, so a provider at the ceiling can hold 256 MiB of bodies.
/// That fits the worker's compose memory limit, and only sitemap shards come near this size,
/// fetched one at a time by a catalogue walk.
const MAX_BODY_BYTES: usize = 16 * 1024 * 1024;

/// Resolves a browser family to a concrete emulation profile.
///
/// It picks the newest build in each family, because fingerprints of long-superseded releases
/// are themselves anomalous. [`BrowserEmulation`] names families, not versions, so bumping
/// `wreq-util` moves every provider forward without a database migration.
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

/// Lists every build this workspace can reproduce, newest first within a family.
///
/// It lets a **solved session** be replayed by a client presenting the browser that earned it.
/// Clearance is bound to the handshake as well as the user agent, so an entry matches only the
/// exact major, and an unmatched major declines. Declining costs a solve per fetch and stays
/// coherent; approximating spends the solve and announces itself.
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

/// Returns whether a family has a profile for `platform`, mirroring `wreq-util`'s per-family
/// list.
///
/// A pair outside it, such as Firefox on Android, would build an emulation whose headers and
/// handshake disagree about the operating system.
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

/// Returns the emulation that reproduces `identity`, or `None` when this workspace cannot
/// present that browser, in which case the caller must not send its user agent either.
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

/// Returns whether this workspace can present `user_agent` over a matching handshake.
///
/// [`crate::solving`] asks before caching a solved session. A session whose browser has no
/// profile replays only with its user agent dropped, and its cookies are then not honoured.
pub(crate) fn can_reproduce(user_agent: &str) -> bool {
    BrowserIdentity::from_user_agent(user_agent)
        .is_some_and(|identity| emulation_for(identity).is_some())
}

/// Holds the policy every client this fetcher builds shares: the SSRF resolver, the redirect
/// rule and the timeouts.
///
/// A solved session's client is built after construction and must match the default one in
/// everything but the emulation profile.
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
        // Resolved before the branch because `follow`, `stop` and `error` consume `attempt`.
        match redirect_hop(&attempt.uri.to_string()) {
            Hop::Follow => attempt.follow(),
            Hop::Stop => attempt.stop(),
            Hop::Refuse(e) => attempt.error(e),
        }
    });

    let mut builder = wreq::Client::builder()
        .dns_resolver(Arc::new(SsrfResolver))
        .redirect(redirect)
        .connect_timeout(policy.connect_timeout)
        .timeout(policy.request_timeout)
        // Decoded here instead, by [`decode_gzip`], which is the only one of the two that reads
        // a whole multi-member stream. `send` compensates for what this turns off.
        .gzip(false);
    if let Some(emulation) = emulation {
        builder = builder.emulation(emulation);
    }
    builder
        .build()
        .map_err(|e| FetchError::Transport(e.to_string()))
}

/// Sends provider requests through `wreq`; one per provider, carrying its identity.
pub struct BaseHttpFetcher {
    client: wreq::Client,
    /// The identifiable bot user agent, present **iff** emulation is off.
    ///
    /// `None` means a profile owns the identity headers. The same field gates the default
    /// `Accept` and `Accept-Language`, so the two can never disagree.
    bot_user_agent: Option<String>,
    /// Clients built to match a solved session's browser, tagged with the identity each
    /// reproduces.
    ///
    /// A list, not a map: a provider's sessions come from one solver rotating through a handful
    /// of builds and platforms, so it holds single digits and a scan beats hashing.
    session_clients: RwLock<Vec<(BrowserIdentity, wreq::Client)>>,
    /// The policy a session client is built with, the same as the default client's.
    policy: ClientPolicy,
}

impl BaseHttpFetcher {
    /// Builds a base fetcher.
    ///
    /// `emulation` selects the browser to impersonate; `None` crawls as an identifiable bot
    /// sending `default_user_agent` verbatim. Under emulation `default_user_agent` is ignored,
    /// because only the profile's user agent matches the handshake.
    ///
    /// # Errors
    /// [`FetchError::Transport`] if the underlying client cannot be built.
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

    /// Returns the client to send a request with, and the user agent to set on it.
    ///
    /// A session's user agent is presented only by a client that reproduces the browser it
    /// names. When no profile does (an unknown family, a build newer than the table, an
    /// unsupported platform), the user agent is **dropped**, not sent over a contradicting
    /// handshake, and the request goes out as the provider's own identity. Its clearance cookies
    /// will then not be honoured; [`crate::solving`] refuses such a session before it gets here.
    fn client_for(&self, session_user_agent: Option<&str>) -> (wreq::Client, Option<String>) {
        let Some(user_agent) = session_user_agent else {
            return (self.client.clone(), self.bot_user_agent.clone());
        };
        // Not emulating: no profile owns the identity, so a session user agent is sent as is.
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

    /// Returns the client presenting `user_agent`'s browser, built and cached on first use, or
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
            // A client that will not build is a broken binary, not a property of this request,
            // and the provider's own client still works, so the fetch goes on without the
            // session identity.
            Err(e) => {
                tracing::warn!(error = %e, "could not build a session-matched client");
                None
            }
        }
    }
}

#[async_trait]
impl Fetcher for BaseHttpFetcher {
    /// Times and counts the request, failures included.
    ///
    /// This is the only choke point for provider traffic (every adapter, scan tier and solver
    /// path comes through here), so it is where "is this provider still answering" becomes a
    /// number.
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
        // Counted even when the request failed: time spent on requests that timed out was
        // still spent.
        crate::accounting::record(crate::accounting::Metered::Request(elapsed));

        result
    }
}

impl BaseHttpFetcher {
    async fn send(&self, req: FetchRequest) -> Result<FetchResponse, FetchError> {
        let url = Url::parse(&req.url).map_err(|_| FetchError::InvalidUrl(req.url.clone()))?;
        // Cheap pre-flight; the resolver enforces the address-range check at connect time.
        ssrf::validate_url(&url)?;

        // The client and the user agent are chosen together, so the handshake and the identity
        // header can never describe different browsers.
        let (client, user_agent) = self.client_for(req.user_agent.as_deref());
        let mut builder = client.get(url.as_str());
        if let Some(ua) = user_agent {
            builder = builder.header(wreq::header::USER_AGENT, ua);
        }
        if self.bot_user_agent.is_some() {
            // Not emulating: no profile owns the content-negotiation headers, so plausible
            // defaults are supplied. Under emulation they come from the profile.
            builder = builder
                .header(
                    wreq::header::ACCEPT,
                    "text/html,application/xhtml+xml,application/xml;q=0.9,application/json;q=0.8,*/*;q=0.5",
                )
                .header(wreq::header::ACCEPT_LANGUAGE, "en-US,en;q=0.8")
                // Spelled out because, with its gzip decoding off, the header `wreq` would
                // synthesise omits gzip, and a bot refusing the web's default encoding is slower
                // and more conspicuous. Under emulation the profile owns this header.
                .header(wreq::header::ACCEPT_ENCODING, "gzip, deflate, br, zstd");
        }
        for (k, v) in &req.headers {
            builder = builder.header(k, v);
        }

        let resp = builder.send().await.map_err(map_wreq_err)?;

        let status = resp.status().as_u16();
        let final_url = resp.uri().to_string();
        // `wreq` decodes brotli, zstd and deflate and strips both headers; gzip is left to
        // `decode_gzip`, so its headers are removed here once the body is decoded. Otherwise
        // `content-encoding` would call decoded text compressed, and `content-length` would
        // count bytes the caller does not hold.
        let gzipped = resp
            .headers()
            .get(wreq::header::CONTENT_ENCODING)
            .is_some_and(|v| {
                // `x-gzip` is the pre-RFC-2616 spelling of the same format, still sent by older
                // origins.
                v.as_bytes().eq_ignore_ascii_case(b"gzip")
                    || v.as_bytes().eq_ignore_ascii_case(b"x-gzip")
            });
        // Every header is kept, not an allowlist of the ones read today:
        // `FetchResponse::headers` promises the response headers.
        let mut headers = Vec::with_capacity(resp.headers().len());
        headers.extend(resp.headers().iter().map(|(k, v)| {
            (
                k.as_str().to_owned(),
                v.to_str().unwrap_or_default().to_owned(),
            )
        }));
        if gzipped {
            headers.retain(|(name, _)| {
                !name.eq_ignore_ascii_case("content-encoding")
                    && !name.eq_ignore_ascii_case("content-length")
            });
        }

        // Pre-size from `Content-Length`, clamped to the cap so a hostile header cannot force a
        // huge allocation for a small body.
        //
        // The header is a hint, never a bound: the streaming check below enforces
        // `MAX_BODY_BYTES`, since a server may send more than it announced.
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
        if gzipped {
            buf = decode_gzip(&buf)?;
        }
        // `from_utf8` first, so the happy path moves the buffer instead of copying it;
        // `from_utf8_lossy` handles the rare body with a bad byte.
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

/// Decodes a `gzip` body across **every** member of the stream.
///
/// The client's own gzip decoding is off ([`build_client`]) because it reads one member and
/// drops whatever follows. An origin that flushes mid-document emits a member per flush, so the
/// body would arrive cut at the first flush: a `200` with intact headers and, on a `WordPress`
/// origin that flushes after `wp_head()`, only the `<head>`. Downstream that reads as a site
/// redesign, not a truncated transfer.
///
/// A member cut in half is an error, so a truncated transfer is loud. The size cap is applied
/// again because the streaming loop counted compressed bytes.
///
/// # Errors
/// [`FetchError::Transport`] if the stream is not valid gzip, a member cut in half included,
/// [`FetchError::BodyTooLarge`] if it inflates past [`MAX_BODY_BYTES`].
fn decode_gzip(raw: &[u8]) -> Result<Vec<u8>, FetchError> {
    let cap = u64::try_from(MAX_BODY_BYTES).unwrap_or(u64::MAX);
    let mut out = Vec::new();
    // One byte past the cap, so a body landing exactly on it stays distinguishable from one
    // that ran over.
    MultiGzDecoder::new(raw)
        .take(cap.saturating_add(1))
        .read_to_end(&mut out)
        .map_err(|e| FetchError::Transport(format!("gzip decode failed: {e}")))?;
    if out.len() > MAX_BODY_BYTES {
        return Err(FetchError::BodyTooLarge);
    }
    Ok(out)
}

/// Folds a status into one of five label values.
///
/// The class, not the code: `provider_fetch_total` is already labelled by provider, and crossing
/// that with every status a broken site can return is an unbounded label source for a
/// distinction no panel makes.
fn status_class(status: u16) -> &'static str {
    match status {
        200..=299 => "2xx",
        300..=399 => "3xx",
        400..=499 => "4xx",
        500..=599 => "5xx",
        _ => "other",
    }
}

/// What the redirect policy does with one hop.
#[derive(Debug, PartialEq, Eq)]
enum Hop {
    /// Follow it.
    Follow,
    /// Return the redirect response itself: the target is not an `http(s)` URL.
    Stop,
    /// Fail the request: the target is an address the SSRF guard forbids.
    Refuse(ssrf::SsrfError),
}

/// Decides one redirect hop.
///
/// Security: the connector skips DNS for an IP-literal authority, so [`SsrfResolver`] never sees
/// a redirect to `http://169.254.169.254/`. The range check on the request URL in `send` does
/// not cover later hops either, so every hop is validated here or a provider could point the
/// crawler at an internal service.
fn redirect_hop(target: &str) -> Hop {
    // A scheme-relative or malformed target does not parse, and stops with the other non-HTTP
    // schemes.
    let Ok(url) = Url::parse(target) else {
        return Hop::Stop;
    };
    if !matches!(url.scheme(), "http" | "https") {
        return Hop::Stop;
    }
    match ssrf::validate_url(&url) {
        Ok(()) => Hop::Follow,
        Err(e) => Hop::Refuse(e),
    }
}

/// Returns the SSRF refusal behind a transport error, from the redirect policy or the resolver.
fn ssrf_cause(e: &wreq::Error) -> Option<ssrf::SsrfError> {
    let mut source = std::error::Error::source(e);
    while let Some(err) = source {
        if let Some(refusal) = err.downcast_ref::<ssrf::SsrfError>() {
            return Some(refusal.clone());
        }
        source = err.source();
    }
    None
}

#[expect(
    clippy::needless_pass_by_value,
    reason = "an owned-error mapper, passed directly to `.map_err`"
)]
fn map_wreq_err(e: wreq::Error) -> FetchError {
    if let Some(refusal) = ssrf_cause(&e) {
        FetchError::Ssrf(refusal)
    } else if e.is_timeout() {
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

    /// Under emulation the profile owns `User-Agent`; the configured bot string would
    /// contradict the TLS fingerprint.
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

    /// A solved session's user agent wins, sent by a client that reproduces its browser.
    ///
    /// The clearance cookie is bound to the user agent the solver's browser presented.
    #[test]
    fn a_session_user_agent_is_sent_with_a_client_that_matches_it() {
        let f = build(Some(BrowserEmulation::Chrome));
        let firefox = "Mozilla/5.0 (Windows NT 10.0; Win64; x64; rv:150.0) \
                       Gecko/20100101 Firefox/150.0";
        let (_, sent) = f.client_for(Some(firefox));
        assert_eq!(sent.as_deref(), Some(firefox));

        // The client is reused, not rebuilt per request.
        let _ = f.client_for(Some(firefox));
        assert_eq!(f.session_clients.read().expect("not poisoned").len(), 1);
    }

    /// A session whose browser no profile reproduces does not have its user agent sent.
    ///
    /// The bug: the deployed solver, Camoufox, is a Firefox that rotates its platform per solve,
    /// while providers default to a Chrome profile. Replaying its session sends a Firefox user
    /// agent over a Chrome `ClientHello` with `sec-ch-ua` headers Firefox does not implement, a
    /// stronger bot signal than crawling honestly.
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

    /// Without emulation a session user agent is sent verbatim: there is no handshake for it to
    /// contradict.
    #[test]
    fn an_unemulated_client_sends_a_session_user_agent_as_it_is() {
        let f = build(None);
        let (_, sent) = f.client_for(Some("SolverUA/2.0"));
        assert_eq!(sent.as_deref(), Some("SolverUA/2.0"));
        let (_, default) = f.client_for(None);
        assert_eq!(default.as_deref(), Some("TankoVaultBot/0.1"));
    }

    /// The deployed solver's user agents resolve to a profile.
    ///
    /// If `wreq-util` drops the Firefox build Camoufox ships, this fails instead of every provider
    /// silently degrading to a solve per fetch.
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

    /// Encodes one gzip member per fragment, as an origin flushing mid-document does.
    fn gzip_members(fragments: &[&str]) -> Vec<u8> {
        use flate2::Compression;
        use flate2::write::GzEncoder;
        use std::io::Write;

        let mut out = Vec::new();
        for fragment in fragments {
            let mut encoder = GzEncoder::new(Vec::new(), Compression::fast());
            encoder.write_all(fragment.as_bytes()).expect("encodes");
            out.extend_from_slice(&encoder.finish().expect("finishes"));
        }
        out
    }

    /// A body flushed as several gzip members decodes whole, not down to the first one.
    ///
    /// The bug: `wreq`'s decoder stops at the first member's end-of-stream marker, so a
    /// `WordPress` origin that flushes after `wp_head()` answers with only its `<head>`, and the
    /// adapter reports "required element not found: series title".
    #[test]
    fn a_gzip_body_decodes_past_its_first_member() {
        let head = "<html><head><title>Series</title></head>";
        let rest = "<body><h1 class=\"entry-title\">Series</h1></body></html>";
        let decoded = decode_gzip(&gzip_members(&[head, rest])).expect("decodes");
        assert_eq!(
            String::from_utf8(decoded).expect("utf-8"),
            format!("{head}{rest}")
        );
    }

    /// A member cut in half is an error, not the bytes that did arrive: a partial document must
    /// not pass as a success.
    #[test]
    fn a_half_written_gzip_member_is_an_error() {
        let whole = gzip_members(&["<html><head><title>Series</title></head>"]);
        let cut = &whole[..whole.len() - 8];
        assert!(matches!(
            decode_gzip(cut),
            Err(FetchError::Transport(_) | FetchError::BodyTooLarge)
        ));
    }

    /// The size cap counts inflated bytes; the streaming loop saw only compressed ones.
    #[test]
    fn the_size_cap_applies_to_the_inflated_body() {
        let bomb = "a".repeat(MAX_BODY_BYTES + 1);
        assert!(matches!(
            decode_gzip(&gzip_members(&[&bomb])),
            Err(FetchError::BodyTooLarge)
        ));
    }

    /// A redirect to an internal IP literal was followed: the request URL was range-checked, the
    /// redirect policy checked only the scheme, and the connector never resolves a literal, so
    /// a provider answering `302 Location: http://169.254.169.254/` reached the metadata service.
    #[test]
    fn a_redirect_to_an_internal_address_is_refused() {
        for target in [
            "http://127.0.0.1/admin",
            "http://169.254.169.254/latest/meta-data/",
            "http://10.0.0.5:8080/",
            "http://[::1]/",
            "http://[::ffff:127.0.0.1]/",
        ] {
            assert!(
                matches!(redirect_hop(target), Hop::Refuse(_)),
                "{target} must be refused"
            );
        }
    }

    #[test]
    fn a_redirect_to_a_public_host_is_followed_and_a_non_web_target_stops() {
        assert_eq!(redirect_hop("https://provider.test/manga/"), Hop::Follow);
        assert_eq!(redirect_hop("http://93.184.216.34/"), Hop::Follow);
        assert_eq!(redirect_hop("ftp://provider.test/file"), Hop::Stop);
        assert_eq!(redirect_hop("//provider.test/manga/"), Hop::Stop);
    }

    /// The bug: a refused redirect hop that `wreq` no longer carried in the error's `source()`
    /// chain would map to `TooManyRedirects` or `Transport`, and callers that branch on
    /// `FetchError::Ssrf` would treat a blocked internal target as an ordinary network fault.
    /// Driven through a real client, so the policy, the error chain and the mapper are all tested.
    #[tokio::test]
    async fn a_refused_redirect_hop_surfaces_as_an_ssrf_error() {
        use wiremock::{Mock, MockServer, ResponseTemplate, matchers::any};

        let server = MockServer::start().await;
        Mock::given(any())
            .respond_with(
                ResponseTemplate::new(302).insert_header("location", "http://169.254.169.254/"),
            )
            .mount(&server)
            .await;
        let policy = ClientPolicy {
            connect_timeout: Duration::from_secs(1),
            request_timeout: Duration::from_secs(1),
        };
        let client = build_client(None, &policy).expect("client builds");

        let err = client
            .get(server.uri())
            .send()
            .await
            .map_err(map_wreq_err)
            .expect_err("the redirect hop is refused");
        assert!(matches!(err, FetchError::Ssrf(_)), "got {err:?}");
    }
}
