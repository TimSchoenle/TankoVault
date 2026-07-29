//! Inbound HTTP rate limiting.
//!
//! Distinct from the *outbound* crawl politeness in `tankovault-fetch`, which paces the
//! requests this system makes to third-party providers. Nothing previously limited what
//! callers could do to us: an unauthenticated client could hammer `/v1/auth/login`
//! without bound, which is the online password-guessing control this closes.
//!
//! ## Shape
//!
//! - A [`RouteClassifier`] maps each matched route to a [`RouteClass`], so credential
//!   endpoints get a far tighter budget than reads without every route needing its own
//!   configuration entry.
//! - A [`RateLimitStore`] holds the counters. [`memory::MemoryStore`] is process-local and
//!   correct for one replica; [`redis::RedisStore`] shares them so the limit holds across
//!   a fleet.
//! - [`RateLimiter`] binds the two together and is mounted as an axum middleware.
//!
//! ## Client identity
//!
//! Buckets are keyed by **client IP**, deliberately not by anything the client supplies.
//! Keying on a bearer token would let an attacker mint a fresh bucket per request by
//! sending a different (even invalid) token, and keying on any header is only safe behind
//! a proxy that overwrites it — hence
//! [`RateLimitConfig::trust_forwarded_for`](tankovault_config::RateLimitConfig::trust_forwarded_for)
//! defaulting to `false`.
//!
//! A service that has already *verified* a principal in an outer layer can insert a
//! [`Principal`] request extension; the limiter prefers it, giving per-user rather than
//! per-IP accounting for authenticated traffic behind shared NAT.

pub mod memory;
#[cfg(feature = "redis")]
pub mod redis;

use async_trait::async_trait;
use axum::extract::{ConnectInfo, MatchedPath, Request};
use axum::http::{HeaderMap, Method, StatusCode, header};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;
use std::time::Duration;
use tankovault_config::{RateLimitConfig, RateLimitPolicy};

/// A verified principal, for per-user rather than per-IP accounting.
///
/// Inserted as a request extension by a service's own authentication layer. The limiter
/// never derives this from client-supplied data itself — see the module docs.
#[derive(Debug, Clone)]
pub struct Principal(pub String);

/// The budget a route draws from.
///
/// A small closed set rather than per-route configuration: operators tune three numbers,
/// and a new endpoint inherits a sensible class instead of silently defaulting to
/// unlimited because nobody added it to a config map.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RouteClass {
    /// Ordinary reads and writes.
    Global,
    /// Credential handling: login, registration, password reset, token refresh.
    Auth,
    /// Cheap to request, expensive to serve: data export, scan triggers, sync push/pull.
    Expensive,
}

impl RouteClass {
    /// Dense index, so a store can hold one limiter per class in an array.
    const COUNT: usize = 3;

    const fn index(self) -> usize {
        match self {
            Self::Global => 0,
            Self::Auth => 1,
            Self::Expensive => 2,
        }
    }

    /// Label used in metrics and log lines.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Global => "global",
            Self::Auth => "auth",
            Self::Expensive => "expensive",
        }
    }

    /// The policy this class draws from.
    #[must_use]
    pub const fn policy(self, cfg: &RateLimitConfig) -> RateLimitPolicy {
        match self {
            Self::Global => cfg.global,
            Self::Auth => cfg.auth,
            Self::Expensive => cfg.expensive,
        }
    }

    /// Every class, in index order.
    const ALL: [Self; Self::COUNT] = [Self::Global, Self::Auth, Self::Expensive];
}

/// Maps matched routes to their [`RouteClass`] by longest-prefix match.
///
/// Matching is done on axum's [`MatchedPath`] (the route *pattern*, `/v1/series/{id}`) so
/// a classification cannot be dodged by varying a path parameter.
/// One classification rule: a route-pattern prefix, the class it grants, and whether it
/// is restricted to mutating requests.
#[derive(Debug, Clone)]
struct Rule {
    prefix: String,
    class: RouteClass,
    /// When set, the rule only applies to mutating methods (POST/PUT/PATCH/DELETE). Safe
    /// reads under the same prefix fall through to a broader class. This is what lets one
    /// path serve both a cheap console read and an expensive operator action: `GET
    /// /v1/admin/scans` (the console's scan-queue overview) and `POST /v1/admin/scans`
    /// (triggering a run) share one route pattern, so classifying the path alone would
    /// drag the read into the tight expensive budget.
    writes_only: bool,
}

#[derive(Debug, Clone, Default)]
pub struct RouteClassifier {
    /// Rules kept sorted longest-prefix-first so the first match is the most specific.
    rules: Vec<Rule>,
}

impl RouteClassifier {
    /// An empty classifier: everything is [`RouteClass::Global`].
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Classify routes under `prefix` as credential handling.
    #[must_use]
    pub fn auth(self, prefix: impl Into<String>) -> Self {
        self.rule(prefix, RouteClass::Auth, false)
    }

    /// Classify all routes under `prefix` as expensive, regardless of method.
    ///
    /// Use for a path that is expensive however it is called — a data export or a
    /// POST-only trigger whose pattern no cheap read shares.
    #[must_use]
    pub fn expensive(self, prefix: impl Into<String>) -> Self {
        self.rule(prefix, RouteClass::Expensive, false)
    }

    /// Classify only *mutating* requests under `prefix` as expensive; leave reads on the
    /// broader budget.
    ///
    /// Use where a heavy action shares its route pattern with a cheap read the UI polls —
    /// notably the operator console's admin listings, whose `GET`s must not be throttled
    /// alongside the `POST`s that kick off real work.
    #[must_use]
    pub fn expensive_write(self, prefix: impl Into<String>) -> Self {
        self.rule(prefix, RouteClass::Expensive, true)
    }

    fn rule(mut self, prefix: impl Into<String>, class: RouteClass, writes_only: bool) -> Self {
        self.rules.push(Rule {
            prefix: prefix.into(),
            class,
            writes_only,
        });
        // Longest first, so `/v1/me/export` wins over a hypothetical `/v1/me` rule.
        self.rules.sort_by(|a, b| {
            b.prefix
                .len()
                .cmp(&a.prefix.len())
                .then_with(|| a.prefix.cmp(&b.prefix))
        });
        self
    }

    /// The class for a matched route pattern and request method.
    ///
    /// A `writes_only` rule is skipped for a safe method (`GET`/`HEAD`/`OPTIONS`/`TRACE`),
    /// so matching continues to any broader rule and, failing that, falls back to
    /// [`RouteClass::Global`].
    #[must_use]
    pub fn classify(&self, method: &Method, matched_path: &str) -> RouteClass {
        let is_write = !method.is_safe();
        self.rules
            .iter()
            .find(|rule| {
                matched_path.starts_with(rule.prefix.as_str()) && (!rule.writes_only || is_write)
            })
            .map_or(RouteClass::Global, |rule| rule.class)
    }
}

/// The verdict for one request.
#[derive(Debug, Clone, Copy)]
pub struct RateLimitDecision {
    /// Whether the request may proceed.
    pub allowed: bool,
    /// Bucket capacity, reported as `X-RateLimit-Limit`.
    pub limit: u32,
    /// Requests still available, reported as `X-RateLimit-Remaining`.
    pub remaining: u32,
    /// How long until the next request would be allowed. Only meaningful when denied.
    pub retry_after: Duration,
}

impl RateLimitDecision {
    /// An allowing decision, for stores that cannot report exact remaining capacity.
    #[must_use]
    pub const fn allow(limit: u32, remaining: u32) -> Self {
        Self {
            allowed: true,
            limit,
            remaining,
            retry_after: Duration::ZERO,
        }
    }

    /// A denying decision.
    #[must_use]
    pub const fn deny(limit: u32, retry_after: Duration) -> Self {
        Self {
            allowed: false,
            limit,
            remaining: 0,
            retry_after,
        }
    }
}

/// Where rate-limit counters live.
#[async_trait]
pub trait RateLimitStore: Send + Sync + 'static {
    /// Charge one request against `key` in `class`.
    ///
    /// Implementations **must fail open**: a counter-store outage is not a reason to stop
    /// serving. A denied request is a deliberate decision, never a side effect of
    /// infrastructure trouble.
    async fn check(&self, class: RouteClass, key: &str) -> RateLimitDecision;
}

/// The mounted limiter: a store, a classifier, and the trust decision for proxy headers.
#[derive(Clone)]
pub struct RateLimiter {
    store: Arc<dyn RateLimitStore>,
    classifier: Arc<RouteClassifier>,
    trust_forwarded_for: bool,
}

impl RateLimiter {
    /// Bind `store` and `classifier` into a mountable limiter.
    #[must_use]
    pub fn new(
        store: Arc<dyn RateLimitStore>,
        classifier: RouteClassifier,
        cfg: &RateLimitConfig,
    ) -> Self {
        Self {
            store,
            classifier: Arc::new(classifier),
            trust_forwarded_for: cfg.trust_forwarded_for,
        }
    }

    /// Build the limiter described by `cfg`, returning `None` when limiting is disabled so
    /// the caller can skip mounting the layer entirely.
    ///
    /// `redis` is consulted only for [`RateLimitBackend::Redis`](tankovault_config::RateLimitBackend::Redis).
    /// When that backend is selected but no client could be built, this falls back to the
    /// in-memory store with a warning rather than starting with no limiting at all.
    #[must_use]
    pub fn from_config(
        cfg: &RateLimitConfig,
        classifier: RouteClassifier,
        #[cfg_attr(not(feature = "redis"), allow(unused_variables))] redis: Option<
            RedisStoreHandle,
        >,
    ) -> Option<Self> {
        if !cfg.enabled {
            tracing::info!("inbound rate limiting disabled by configuration");
            return None;
        }

        let store: Arc<dyn RateLimitStore> = match cfg.backend {
            tankovault_config::RateLimitBackend::Memory => {
                tracing::info!(
                    global = cfg.global.per_minute,
                    auth = cfg.auth.per_minute,
                    expensive = cfg.expensive.per_minute,
                    "rate limiting enabled (in-memory, per replica)"
                );
                Arc::new(memory::MemoryStore::new(cfg))
            }
            #[cfg(feature = "redis")]
            tankovault_config::RateLimitBackend::Redis => {
                if let Some(handle) = redis {
                    tracing::info!("rate limiting enabled (redis, shared across replicas)");
                    Arc::new(redis::RedisStore::new(handle.client, cfg))
                } else {
                    tracing::warn!(
                        "redis rate-limit backend selected but no client available; \
                         falling back to per-replica in-memory limiting"
                    );
                    Arc::new(memory::MemoryStore::new(cfg))
                }
            }
            #[cfg(not(feature = "redis"))]
            tankovault_config::RateLimitBackend::Redis => {
                tracing::warn!(
                    "redis rate-limit backend selected but this binary was built without \
                     the `redis` feature; falling back to per-replica in-memory limiting"
                );
                Arc::new(memory::MemoryStore::new(cfg))
            }
        };

        Some(Self::new(store, classifier, cfg))
    }

    /// Derive the bucket key for a request.
    ///
    /// Prefers a verified [`Principal`] extension, then the proxy-supplied client IP when
    /// the operator has said a trustworthy proxy is in front, then the peer address.
    /// Falls back to a single shared `unknown` bucket rather than to no limit at all: an
    /// unidentifiable caller is exactly the one that should not get a free pass.
    fn key(&self, req: &Request) -> String {
        if let Some(Principal(id)) = req.extensions().get::<Principal>() {
            return format!("u:{id}");
        }
        if self.trust_forwarded_for {
            if let Some(ip) = forwarded_client_ip(req.headers()) {
                return format!("ip:{}", bucket_ip(&ip));
            }
        }
        req.extensions()
            .get::<ConnectInfo<SocketAddr>>()
            .map_or_else(
                || "ip:unknown".to_owned(),
                |ConnectInfo(addr)| format!("ip:{}", canonicalise(addr.ip())),
            )
    }
}

/// The **right-most** entry of `X-Forwarded-For`, or `X-Real-IP`.
///
/// Only consulted when the operator has enabled `trust_forwarded_for`; otherwise these
/// headers are entirely client-controlled and would defeat the limiter.
///
/// Right-most, not left-most: the reverse proxy in front of us *appends* the peer it
/// actually accepted the connection from (`services/frontend`, mirroring nginx's
/// `$proxy_add_x_forwarded_for`), so every entry to the left of that one was supplied by
/// the client and is forgeable. Reading the left-most entry handed any caller a fresh
/// bucket per request, which removed the auth limiter entirely — unlimited online password
/// guessing and reset-mail flooding.
///
/// This assumes exactly one trusted hop in front of the service. That is what
/// `deploy/docker-compose.yml` deploys; if another proxy is added, it must also append, and
/// `trust_forwarded_for` must stay off wherever the service is reachable directly.
fn forwarded_client_ip(headers: &HeaderMap) -> Option<String> {
    if let Some(forwarded) = headers.get("x-forwarded-for").and_then(|v| v.to_str().ok()) {
        if let Some(last) = forwarded.rsplit(',').next() {
            let trimmed = last.trim();
            if !trimmed.is_empty() {
                return Some(trimmed.to_owned());
            }
        }
    }
    headers
        .get("x-real-ip")
        .and_then(|v| v.to_str().ok())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
}

/// Collapse an address to the unit an attacker cannot cheaply multiply.
///
/// IPv6 is masked to its /64 prefix: the standard residential and VPS allocation is a routed
/// /64, so bucketing on the full address gave one attacker 2^64 distinct budgets — and, with
/// the Redis store, 2^64 distinct keys to grow the counter store with. IPv4 keeps its full
/// address, where an extra address costs real money.
fn canonicalise(ip: IpAddr) -> String {
    match ip {
        IpAddr::V4(v4) => v4.to_string(),
        IpAddr::V6(v6) => {
            let s = v6.segments();
            format!("{:x}:{:x}:{:x}:{:x}::/64", s[0], s[1], s[2], s[3])
        }
    }
}

/// [`canonicalise`] for a header-sourced string, falling back to the raw value (bounded, so
/// a junk header cannot be used to mint unlimited keys) when it does not parse as an address.
fn bucket_ip(raw: &str) -> String {
    raw.parse::<IpAddr>().map_or_else(
        |_| {
            let mut trimmed = raw.to_owned();
            trimmed.truncate(45); // max textual IPv6 length; longer is not an address
            trimmed
        },
        canonicalise,
    )
}

/// Opaque carrier for a Redis client, so callers need not name `fred` types and the
/// signature of [`RateLimiter::from_config`] does not change with the feature set.
#[cfg(feature = "redis")]
pub struct RedisStoreHandle {
    pub(crate) client: fred::clients::Client,
}

#[cfg(feature = "redis")]
impl RedisStoreHandle {
    /// Wrap an initialised client.
    #[must_use]
    pub fn new(client: fred::clients::Client) -> Self {
        Self { client }
    }
}

/// Placeholder so `from_config` keeps one signature across feature sets.
#[cfg(not(feature = "redis"))]
pub struct RedisStoreHandle(());

/// The axum middleware. Mount with `axum::middleware::from_fn_with_state(limiter, enforce)`.
///
/// On denial, answers `429` with an RFC 9457 `problem+json` body matching the shape the
/// API's own error type produces, so a client parses one error format everywhere.
pub async fn enforce(
    axum::extract::State(limiter): axum::extract::State<RateLimiter>,
    req: Request,
    next: Next,
) -> Response {
    let method = req.method().clone();
    let class = req
        .extensions()
        .get::<MatchedPath>()
        .map_or(RouteClass::Global, |p| {
            limiter.classifier.classify(&method, p.as_str())
        });
    let key = limiter.key(&req);

    let decision = limiter.store.check(class, &key).await;
    if !decision.allowed {
        metrics::counter!("http_rate_limited_total", "class" => class.as_str()).increment(1);
        tracing::warn!(
            class = class.as_str(),
            retry_after_secs = decision.retry_after.as_secs(),
            "request rate limited"
        );
        return too_many_requests(&decision);
    }

    let mut response = next.run(req).await;
    let headers = response.headers_mut();
    insert_limit_headers(headers, &decision);
    response
}

/// `429` with `Retry-After` and the standard problem body.
fn too_many_requests(decision: &RateLimitDecision) -> Response {
    // Round up: a `Retry-After: 0` invites an immediate retry that is certain to fail.
    let retry_secs = decision.retry_after.as_secs().max(1);

    let body = axum::Json(serde_json::json!({
        "type": "about:blank#rate_limited",
        "title": "rate_limited",
        "status": StatusCode::TOO_MANY_REQUESTS.as_u16(),
        "detail": "too many requests; slow down and retry shortly",
    }));

    let mut response = (StatusCode::TOO_MANY_REQUESTS, body).into_response();
    let headers = response.headers_mut();
    if let Ok(value) = retry_secs.to_string().parse() {
        headers.insert(header::RETRY_AFTER, value);
    }
    insert_limit_headers(headers, decision);
    response
}

/// Advertise the budget so a well-behaved client can pace itself instead of discovering
/// the limit by being refused.
fn insert_limit_headers(headers: &mut HeaderMap, decision: &RateLimitDecision) {
    if let Ok(value) = decision.limit.to_string().parse() {
        headers.insert("x-ratelimit-limit", value);
    }
    if let Ok(value) = decision.remaining.to_string().parse() {
        headers.insert("x-ratelimit-remaining", value);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unclassified_routes_fall_back_to_global() {
        let classifier = RouteClassifier::new().auth("/v1/auth");
        assert_eq!(
            classifier.classify(&Method::GET, "/v1/series"),
            RouteClass::Global
        );
        assert_eq!(
            classifier.classify(&Method::POST, "/v1/auth/login"),
            RouteClass::Auth
        );
    }

    #[test]
    fn the_longest_matching_prefix_wins() {
        // Registration order must not matter; the more specific rule has to win or a
        // broad rule added later would silently downgrade a tight one.
        let classifier = RouteClassifier::new()
            .expensive("/v1/me/export")
            .auth("/v1/me");
        assert_eq!(
            classifier.classify(&Method::GET, "/v1/me/export"),
            RouteClass::Expensive,
            "the specific export rule must beat the broader /v1/me rule"
        );
        assert_eq!(
            classifier.classify(&Method::GET, "/v1/me/watchlist"),
            RouteClass::Auth
        );
    }

    #[test]
    fn writes_only_rules_spare_reads_on_a_shared_path() {
        // The operator console paints itself with `GET /v1/admin/scans`; only the `POST`
        // that triggers a run is genuinely expensive. A method-blind rule would throttle
        // the console's reads alongside the trigger.
        let classifier = RouteClassifier::new().expensive_write("/v1/admin/scans");
        assert_eq!(
            classifier.classify(&Method::GET, "/v1/admin/scans"),
            RouteClass::Global,
            "reads on the shared path keep the ordinary budget"
        );
        assert_eq!(
            classifier.classify(&Method::GET, "/v1/admin/scans/stream"),
            RouteClass::Global,
            "the live console stream is a read and must not be throttled as expensive"
        );
        assert_eq!(
            classifier.classify(&Method::POST, "/v1/admin/scans"),
            RouteClass::Expensive,
            "triggering a run is the expensive action the budget exists for"
        );
    }

    /// Regression: reading the left-most entry let any caller mint a fresh bucket per
    /// request by sending their own `X-Forwarded-For`, which removed the auth limiter.
    #[test]
    fn forwarded_for_takes_the_rightmost_entry() {
        let mut headers = HeaderMap::new();
        headers.insert(
            "x-forwarded-for",
            "10.0.0.1, 192.168.0.5, 203.0.113.7".parse().unwrap(),
        );
        assert_eq!(
            forwarded_client_ip(&headers).as_deref(),
            Some("203.0.113.7"),
            "only the entry our own proxy appended is trustworthy"
        );
    }

    #[test]
    fn a_client_supplied_forwarded_for_cannot_mint_a_new_bucket() {
        let appended_by_our_proxy = "203.0.113.7";
        let mut first = HeaderMap::new();
        first.insert(
            "x-forwarded-for",
            format!("1.2.3.4, {appended_by_our_proxy}").parse().unwrap(),
        );
        let mut second = HeaderMap::new();
        second.insert(
            "x-forwarded-for",
            format!("5.6.7.8, {appended_by_our_proxy}").parse().unwrap(),
        );
        assert_eq!(
            forwarded_client_ip(&first),
            forwarded_client_ip(&second),
            "a forged prefix must not change the bucket"
        );
    }

    /// A routed /64 is the standard residential and VPS allocation, so bucketing on the full
    /// v6 address gave one attacker 2^64 budgets and 2^64 Redis keys.
    #[test]
    fn ipv6_buckets_collapse_to_the_64_prefix() {
        let a = bucket_ip("2001:db8:1:2:3:4:5:6");
        let b = bucket_ip("2001:db8:1:2:ffff:ffff:ffff:ffff");
        assert_eq!(a, b);
        assert_eq!(a, "2001:db8:1:2::/64");
        assert_ne!(a, bucket_ip("2001:db8:1:3::1"));
    }

    #[test]
    fn ipv4_buckets_keep_the_full_address() {
        assert_eq!(bucket_ip("203.0.113.7"), "203.0.113.7");
        assert_ne!(bucket_ip("203.0.113.7"), bucket_ip("203.0.113.8"));
    }

    /// A junk header must not become an unbounded key-space either.
    #[test]
    fn unparseable_forwarded_values_are_truncated() {
        assert_eq!(bucket_ip("not-an-ip").len(), "not-an-ip".len());
        assert_eq!(bucket_ip(&"x".repeat(10_000)).len(), 45);
    }

    #[test]
    fn real_ip_is_the_fallback_and_blanks_are_ignored() {
        let mut headers = HeaderMap::new();
        headers.insert("x-forwarded-for", "   ".parse().unwrap());
        headers.insert("x-real-ip", "198.51.100.4".parse().unwrap());
        assert_eq!(
            forwarded_client_ip(&headers).as_deref(),
            Some("198.51.100.4")
        );

        assert_eq!(forwarded_client_ip(&HeaderMap::new()), None);
    }

    #[test]
    fn class_indices_are_dense_and_unique() {
        // `MemoryStore` indexes an array with these; a collision would silently share a
        // limiter between two classes.
        let mut seen = [false; RouteClass::COUNT];
        for class in RouteClass::ALL {
            assert!(!seen[class.index()], "duplicate index for {class:?}");
            seen[class.index()] = true;
        }
        assert!(seen.iter().all(|s| *s));
    }
}
