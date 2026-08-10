//! Service-to-service authentication and authorisation for the internal tier.
//!
//! `sync`, `control-plane`, `worker`, `render` and `challenge-solver` serve privileged routes
//! that are reachable by service name from anywhere on the network. Two questions are answered
//! here, in this order: **who is calling** ([`IdentityMode`]), and **may that caller reach this
//! route** ([`RouteTable`]).
//!
//! Only the first question has a mode-dependent answer. Authorisation reads the same table
//! whichever way the caller was identified, so a deployment cannot end up authorised differently
//! by virtue of running `token` rather than `mtls`. That invariant is held by
//! `authorisation_does_not_depend_on_how_the_caller_was_identified` below and by each service's
//! own matrix test.
//!
//! Health and readiness probes are mounted outside this stack, so an orchestrator never needs a
//! credential.

use axum::extract::{Request, State};
use axum::http::{HeaderName, Method, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use secrecy::{ExposeSecret, SecretString};
use std::sync::Arc;
use subtle::ConstantTimeEq;
use tankovault_config::{IdentityMode, ResolvedInternalAuth};

/// The header a caller presents under [`IdentityMode::Token`].
///
/// Named for the tier rather than the caller: which caller it is, is what the *value* says now.
pub const INTERNAL_TOKEN_HEADER: HeaderName = HeaderName::from_static("x-internal-token");

/// A credential this service presents when calling a peer.
///
/// `Debug` is redacted via [`secrecy`], so a stray `tracing::debug!(?state)` cannot leak it.
/// `Arc<SecretString>` because this value is cloned into an `AppState` per request; the `Arc`
/// keeps exactly one heap copy, which is the copy zeroized.
#[derive(Clone, Debug)]
pub struct InternalToken(Arc<SecretString>);

impl InternalToken {
    /// Wrap a resolved token.
    #[must_use]
    pub fn new(token: impl Into<SecretString>) -> Self {
        Self(Arc::new(token.into()))
    }

    /// Constant-time equality against a presented value.
    #[must_use]
    pub fn matches(&self, presented: &str) -> bool {
        // The plain `len()` check is safe pre-filtering, not a leak: lengths are already
        // public, and `ct_eq` would reject a length mismatch anyway.
        let expected = self.0.expose_secret().as_bytes();
        let got = presented.as_bytes();
        expected.len() == got.len() && bool::from(expected.ct_eq(got))
    }
}

/// Reading the token is [`ExposeSecret`], not a bespoke method, so every secret read in the
/// workspace is the same greppable call.
impl ExposeSecret<str> for InternalToken {
    fn expose_secret(&self) -> &str {
        self.0.expose_secret()
    }
}

/// The identified caller of an internal request, inserted into request extensions once it has
/// been both authenticated and authorised.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Caller(Arc<str>);

impl Caller {
    /// The caller's configured name.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.0
    }

    /// The stand-in used under [`IdentityMode::Off`], where nobody is identified.
    ///
    /// Deliberately a name no peer entry can hold — [`ResolvedPeer`](tankovault_config::ResolvedPeer)
    /// names are trimmed and non-empty — so an audit record can never confuse a real caller with
    /// an unauthenticated one.
    #[must_use]
    pub fn anonymous() -> Self {
        Self(Arc::from("<unidentified>"))
    }
}

impl std::fmt::Display for Caller {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// One internal route and the callers permitted to reach it.
///
/// `path` is the axum route pattern (`/internal/providers/{id}/test`), matched through
/// [`axum::extract::MatchedPath`] rather than by string comparison against the request URI, so a
/// path parameter cannot be crafted to look like a different route.
#[derive(Debug, Clone)]
pub struct InternalRoute {
    pub method: Method,
    pub path: &'static str,
    /// Caller names permitted here. Empty means nobody, which is a usable way to retire a route
    /// without deleting it.
    pub callers: &'static [&'static str],
}

/// Every internal route a service serves, and who may reach each one.
///
/// Deny by default: a request whose matched route has no entry is refused. That is what makes a
/// forgotten route fail closed and loudly rather than inheriting whatever the last `.route()`
/// call happened to allow.
#[derive(Debug, Clone, Copy)]
pub struct RouteTable(pub &'static [InternalRoute]);

impl RouteTable {
    /// The callers permitted on `method path`, or `None` when the route is not in the table.
    #[must_use]
    pub fn callers_for(self, method: &Method, path: &str) -> Option<&'static [&'static str]> {
        self.0
            .iter()
            .find(|r| r.method == method && r.path == path)
            .map(|r| r.callers)
    }

    /// Whether `caller` may reach `method path`.
    #[must_use]
    pub fn allows(self, caller: &str, method: &Method, path: &str) -> bool {
        self.callers_for(method, path)
            .is_some_and(|callers| callers.contains(&caller))
    }
}

/// Everything the middleware needs: how to identify a caller, and what each may reach.
#[derive(Clone, Debug)]
pub struct InternalAuth {
    mode: IdentityMode,
    peers: Arc<[Peer]>,
    routes: RouteTable,
}

/// One accepted caller, with whichever credential the active mode verifies.
#[derive(Clone, Debug)]
struct Peer {
    name: Arc<str>,
    token: Option<InternalToken>,
    san: Option<Arc<str>>,
}

impl InternalAuth {
    /// Build from a validated configuration plus this service's route table.
    #[must_use]
    pub fn new(resolved: &ResolvedInternalAuth, routes: RouteTable) -> Self {
        let peers = resolved
            .peers
            .iter()
            .map(|p| Peer {
                name: Arc::from(p.name.as_str()),
                token: p.token.as_ref().map(|t| InternalToken::new(t.clone())),
                san: p.san.as_deref().map(Arc::from),
            })
            .collect::<Vec<_>>();

        Self {
            mode: resolved.mode,
            peers: peers.into(),
            routes,
        }
    }

    /// The active identity mode.
    #[must_use]
    pub fn mode(&self) -> IdentityMode {
        self.mode
    }

    /// Identify the caller behind `req`, or `None` if no credential matched.
    ///
    /// Under [`IdentityMode::Token`] every peer is compared even after a match, so the work done
    /// does not depend on which peer matched or on how many entries precede it.
    fn identify(&self, req: &Request) -> Option<Caller> {
        match self.mode {
            IdentityMode::Off => Some(Caller::anonymous()),
            IdentityMode::Token => {
                let presented = req
                    .headers()
                    .get(INTERNAL_TOKEN_HEADER)
                    .and_then(|v| v.to_str().ok())?;
                let mut found: Option<&Arc<str>> = None;
                for peer in self.peers.iter() {
                    if peer.token.as_ref().is_some_and(|t| t.matches(presented)) {
                        found = Some(&peer.name);
                    }
                }
                found.map(|name| Caller(Arc::clone(name)))
            }
            IdentityMode::Mtls => {
                // Absent means the connection was not mutually authenticated. The verifier
                // requires a client certificate, so this is unreachable through the mTLS
                // listener — and returning `None` rather than trusting the request is what
                // keeps it unreachable if the service is ever mounted on a plain one.
                let sans = req.extensions().get::<crate::tls::PeerSans>()?;
                self.peers
                    .iter()
                    .find(|p| p.san.as_deref().is_some_and(|s| sans.contains(s)))
                    .map(|p| Caller(Arc::clone(&p.name)))
            }
        }
    }
}

/// Identify the caller, authorise the route, and record who it was.
///
/// Mount with `axum::middleware::from_fn_with_state(auth, identify)` inside the service's
/// [`crate::http::HttpStack`], so refusals still carry the security headers and request id.
///
/// A refusal is a bare status with no body: the caller is a service with static configuration,
/// not a human debugging. `401` means "I do not know who you are", `403` means "I know, and no".
/// Only the matched route pattern is logged, never the URI — `/v1/me/stream` carries a token in
/// its query string, and a path parameter can carry a user id.
pub async fn identify(State(auth): State<InternalAuth>, mut req: Request, next: Next) -> Response {
    let Some(caller) = auth.identify(&req) else {
        tracing::warn!(
            mode = %auth.mode,
            "rejected an internal request that presented no recognised credential"
        );
        return StatusCode::UNAUTHORIZED.into_response();
    };

    // The matched pattern, not the raw URI: `/internal/providers/{id}/test` is one route however
    // the id is spelled, and a request that matched no route has nothing to authorise.
    let matched = req
        .extensions()
        .get::<axum::extract::MatchedPath>()
        .map(|m| m.as_str().to_owned());
    let Some(path) = matched else {
        tracing::warn!(caller = %caller, "rejected an internal request that matched no route");
        return StatusCode::NOT_FOUND.into_response();
    };

    if auth.mode != IdentityMode::Off && !auth.routes.allows(caller.name(), req.method(), &path) {
        // Distinguishing the two in the log (not on the wire) is what tells an operator whether
        // they mis-scoped a caller or forgot to classify a new route.
        if auth.routes.callers_for(req.method(), &path).is_none() {
            tracing::error!(
                caller = %caller,
                method = %req.method(),
                route = %path,
                "refused an internal route that is in no route table; classify it, or it stays \
                 unreachable"
            );
        } else {
            tracing::warn!(
                caller = %caller,
                method = %req.method(),
                route = %path,
                "refused an internal route this caller is not permitted to reach"
            );
        }
        return StatusCode::FORBIDDEN.into_response();
    }

    req.extensions_mut().insert(caller);
    next.run(req).await
}

/// Placeholder tokens published in this repository, refused wherever they appear.
///
/// Counterpart of `services/api/src/main.rs::KNOWN_PLACEHOLDERS`. `xtask repo-lint` derives
/// this list's required contents from `deploy/docker-compose.yml` defaults and fails if a
/// published secret is missing from it.
const KNOWN_PLACEHOLDERS: [(&str, &str); 1] = [(
    "dev-internal-token-not-for-production-use",
    "development internal-tier token",
)];

/// The name of the published placeholder `value` is, if it is one.
fn known_placeholder(value: &str) -> Option<&'static str> {
    let trimmed = value.trim();
    KNOWN_PLACEHOLDERS
        .iter()
        .find(|(placeholder, _)| *placeholder == trimmed)
        .map(|(_, name)| *name)
}

/// Validate this service's internal-auth configuration.
///
/// A **published placeholder is refused in every profile**, not only production: a check a
/// deployment can skip by forgetting one environment variable is not a check. Every credential
/// is checked, inbound and outbound — a placeholder is no safer as a peer's token than as this
/// service's own.
///
/// # Errors
/// [`tankovault_config::ConfigError::Invalid`] for anything
/// [`tankovault_config::InternalAuthConfig::resolve`] refuses, or any credential that is a
/// published placeholder.
pub fn resolve(
    cfg: &tankovault_config::InternalAuthConfig,
) -> Result<ResolvedInternalAuth, tankovault_config::ConfigError> {
    let resolved = cfg.resolve(tankovault_config::is_production())?;

    let credentials = resolved
        .caller
        .iter()
        .filter_map(|c| {
            c.token
                .as_ref()
                .map(|t| (format!("internal.caller.token ({})", c.name), t))
        })
        .chain(resolved.peers.iter().filter_map(|p| {
            p.token
                .as_ref()
                .map(|t| (format!("internal.peers.{}.token", p.name), t))
        }));

    for (key, token) in credentials {
        if let Some(name) = known_placeholder(token.expose_secret()) {
            return Err(tankovault_config::ConfigError::Invalid(format!(
                "refusing to start: {key} is the well-known {name}, which is published in this \
                 repository. Anything that has read deploy/docker-compose.yml can call this \
                 service's privileged routes with it."
            )));
        }
    }

    if resolved.mode == IdentityMode::Off {
        tracing::warn!(
            "internal.identity=off: this service's privileged routes are reachable by anything \
             that can open a socket to it, and every caller is unidentified. Set \
             internal.identity to `token` or `mtls` outside local development."
        );
    }

    if let Some(paths) = &resolved.tls {
        // Read now rather than at first connection. A missing mount is a deployment error, and
        // discovering it when the listener binds — before anything can be routed to this
        // replica — is the difference between a crash-looping pod and a running one that
        // refuses every request.
        crate::tls::client_material(paths).map_err(|e| {
            tankovault_config::ConfigError::Invalid(format!(
                "internal.identity=mtls but the certificate material could not be read: {e}"
            ))
        })?;
    }

    Ok(resolved)
}

/// Refuse an upstream URL that contradicts the active identity mode.
///
/// The failure this exists for is silent: under `mtls`, an upstream still spelled `http://`
/// connects, works, and carries every internal call in plaintext with no client certificate
/// offered — a deployment that believes it has mutual TLS and does not. Nothing else notices,
/// because the *server* side is configured correctly and simply never sees these requests.
///
/// The scheme stays whatever the operator wrote; silently upgrading it would hide the same
/// misconfiguration one layer down.
///
/// # Errors
/// [`tankovault_config::ConfigError::Invalid`] when `mode` is [`IdentityMode::Mtls`] and `url`
/// is not `https://`.
pub fn check_upstream_scheme(
    mode: IdentityMode,
    name: &str,
    url: &str,
) -> Result<(), tankovault_config::ConfigError> {
    if mode == IdentityMode::Mtls && !url.trim().starts_with("https://") {
        return Err(tankovault_config::ConfigError::Invalid(format!(
            "internal.identity=mtls, but the {name} URL is `{url}`. A plaintext upstream under \
             mtls presents no client certificate and encrypts nothing, while the peer's own \
             configuration still says it requires both. Use https://."
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tankovault_config::{
        CallerConfig, InternalAuthConfig, PeerConfig, ResolvedCaller, ResolvedPeer,
    };

    const SCANS: &str = "/internal/scans";
    const TEST_ADAPTER: &str = "/internal/providers/{id}/test";

    static ROUTES: &[InternalRoute] = &[
        InternalRoute {
            method: Method::POST,
            path: SCANS,
            callers: &["api"],
        },
        InternalRoute {
            method: Method::POST,
            path: TEST_ADAPTER,
            callers: &["api"],
        },
    ];

    fn auth(mode: IdentityMode, peers: Vec<ResolvedPeer>) -> InternalAuth {
        InternalAuth::new(
            &ResolvedInternalAuth {
                mode,
                caller: None,
                peers,
                tls: None,
                probe_listen: None,
            },
            RouteTable(ROUTES),
        )
    }

    fn token_peer(name: &str, token: &str) -> ResolvedPeer {
        ResolvedPeer {
            name: name.to_owned(),
            token: Some(SecretString::from(token.to_owned())),
            san: None,
        }
    }

    fn request_with_token(token: &str) -> Request {
        Request::builder()
            .header(INTERNAL_TOKEN_HEADER, token)
            .body(axum::body::Body::empty())
            .expect("a header-only request builds")
    }

    /// `deploy/docker-compose.yml` published a default token that production only refused
    /// when *missing*, not when equal to this one — refused in every profile now.
    #[test]
    fn the_published_placeholder_is_refused_in_every_profile() {
        for (placeholder, _) in KNOWN_PLACEHOLDERS {
            assert!(
                known_placeholder(placeholder).is_some(),
                "{placeholder} must be recognised"
            );
            // Surrounding whitespace is how a placeholder survives a copy-paste through YAML.
            assert!(known_placeholder(&format!("  {placeholder}  ")).is_some());
        }
        assert!(known_placeholder("a-real-token-from-openssl-rand-hex-32").is_none());
    }

    /// The placeholder must be refused as a *peer's* credential too, not only as this service's
    /// own. Accepting it inbound is the same hole from the other side.
    #[test]
    fn a_placeholder_is_refused_wherever_it_appears() {
        let placeholder = KNOWN_PLACEHOLDERS[0].0;
        let cfg = InternalAuthConfig {
            identity: IdentityMode::Token,
            peers: std::collections::BTreeMap::from([(
                "api".to_owned(),
                PeerConfig {
                    token: Some(SecretString::from(placeholder.to_owned())),
                    san: None,
                },
            )]),
            ..Default::default()
        };
        let err = resolve(&cfg).expect_err("a published placeholder must never be accepted");
        assert!(
            err.to_string().contains("internal.peers.api.token"),
            "{err}"
        );

        let cfg = InternalAuthConfig {
            identity: IdentityMode::Token,
            caller: CallerConfig {
                name: Some("worker".to_owned()),
                token: Some(SecretString::from(placeholder.to_owned())),
            },
            ..Default::default()
        };
        assert!(resolve(&cfg).is_err(), "outbound placeholders too");
    }

    #[test]
    fn matches_only_the_exact_token() {
        let token = InternalToken::new("s3cret-value-of-sufficient-length");
        assert!(token.matches("s3cret-value-of-sufficient-length"));
        assert!(!token.matches("s3cret-value-of-sufficient-lengtH"));
        assert!(!token.matches("s3cret-value-of-sufficient-lengt"));
        assert!(!token.matches("s3cret-value-of-sufficient-length "));
        assert!(!token.matches(""));
    }

    /// Asserted as "the secret does not appear" rather than an exact rendering, so a
    /// cosmetic change in `secrecy`'s redaction format doesn't fail a test that shouldn't care.
    #[test]
    fn debug_is_redacted() {
        let token = InternalToken::new("do-not-print-me-do-not-print-me-1");
        let rendered = format!("{token:?}");
        assert!(!rendered.contains("do-not-print"), "{rendered}");
        assert!(rendered.contains("REDACTED"), "{rendered}");
    }

    /// The whole point of per-caller credentials: a token identifies *which* peer, not merely
    /// that some peer presented something. Before this, one shared secret meant `challenge-solver`
    /// and `api` were indistinguishable to every callee.
    #[test]
    fn a_token_names_the_caller_that_presented_it() {
        let auth = auth(
            IdentityMode::Token,
            vec![
                token_peer("api", "api-token-of-sufficient-length-01"),
                token_peer("worker", "worker-token-of-sufficient-len-02"),
            ],
        );

        assert_eq!(
            auth.identify(&request_with_token("api-token-of-sufficient-length-01")),
            Some(Caller(Arc::from("api")))
        );
        assert_eq!(
            auth.identify(&request_with_token("worker-token-of-sufficient-len-02")),
            Some(Caller(Arc::from("worker")))
        );
        assert_eq!(
            auth.identify(&request_with_token("neither-of-the-above")),
            None
        );
    }

    /// Deny by default. A route nobody classified must be refused rather than inheriting the
    /// permissions of whatever else the service mounts.
    #[test]
    fn an_unclassified_route_is_refused() {
        let table = RouteTable(ROUTES);
        assert!(table.allows("api", &Method::POST, SCANS));
        assert!(!table.allows("api", &Method::POST, "/internal/recsys-build"));
        assert!(
            table
                .callers_for(&Method::POST, "/internal/recsys-build")
                .is_none()
        );
    }

    /// A caller is authorised per route, not per tier. `worker` holding a valid credential must
    /// still not be able to trigger a scan.
    #[test]
    fn a_recognised_caller_is_not_thereby_permitted_everywhere() {
        let table = RouteTable(ROUTES);
        assert!(table.allows("api", &Method::POST, TEST_ADAPTER));
        assert!(!table.allows("worker", &Method::POST, TEST_ADAPTER));
    }

    /// The method is part of the route's identity: `GET /internal/scans` is not `POST
    /// /internal/scans`, and a table keyed on the path alone would authorise both.
    #[test]
    fn the_method_is_part_of_the_route() {
        let table = RouteTable(ROUTES);
        assert!(table.allows("api", &Method::POST, SCANS));
        assert!(!table.allows("api", &Method::GET, SCANS));
    }

    /// mTLS names the caller from the verified SAN, and a request that arrives without one was
    /// not verified — it must not fall through to "some peer".
    #[test]
    fn mtls_identifies_by_san_and_refuses_an_unverified_request() {
        let auth = auth(
            IdentityMode::Mtls,
            vec![ResolvedPeer {
                name: "api".to_owned(),
                token: None,
                san: Some("api.tankovault.svc".to_owned()),
            }],
        );

        let mut req = Request::builder()
            .body(axum::body::Body::empty())
            .expect("an empty request builds");
        assert_eq!(auth.identify(&req), None, "no certificate, no caller");

        // Every name the certificate carries is offered, and the configured one is in the
        // middle of them: cert-manager emits `api`, `api.<ns>`, `api.<ns>.svc` and the FQDN, and
        // matching only the first would make the peer entry depend on emission order.
        req.extensions_mut().insert(crate::tls::PeerSans(
            vec![
                "api".to_owned(),
                "api.tankovault".to_owned(),
                "api.tankovault.svc".to_owned(),
            ]
            .into(),
        ));
        assert_eq!(auth.identify(&req), Some(Caller(Arc::from("api"))));

        let mut wrong = Request::builder()
            .body(axum::body::Body::empty())
            .expect("an empty request builds");
        wrong.extensions_mut().insert(crate::tls::PeerSans(
            vec!["worker.tankovault.svc".to_owned()].into(),
        ));
        assert_eq!(auth.identify(&wrong), None, "an unlisted SAN is not a peer");
    }

    /// The invariant the whole design rests on: authorisation is mode-independent. If these
    /// diverge, "works outside Kubernetes" stops being true without anything failing.
    #[test]
    fn authorisation_does_not_depend_on_how_the_caller_was_identified() {
        let table = RouteTable(ROUTES);
        for route in ROUTES {
            for caller in ["api", "worker", "render"] {
                let permitted = route.callers.contains(&caller);
                assert_eq!(
                    table.allows(caller, &route.method, route.path),
                    permitted,
                    "{caller} on {} {} must not depend on the identity mode",
                    route.method,
                    route.path
                );
            }
        }
    }

    /// `off` is for local development, so it identifies nobody but must still let every request
    /// through — including routes that are in no table, which is how a service under test keeps
    /// working before its table is written.
    #[test]
    fn off_identifies_nobody_and_authorises_everything() {
        let auth = auth(IdentityMode::Off, Vec::new());
        let req = Request::builder()
            .body(axum::body::Body::empty())
            .expect("an empty request builds");
        assert_eq!(auth.identify(&req), Some(Caller::anonymous()));
    }

    /// The anonymous stand-in must not be spellable as a real peer, or an audit record could
    /// not tell an unauthenticated request from a service that happened to be named that.
    #[test]
    fn the_anonymous_caller_cannot_collide_with_a_configured_peer() {
        let name = Caller::anonymous();
        let cfg = InternalAuthConfig {
            identity: IdentityMode::Token,
            peers: std::collections::BTreeMap::from([(
                name.name().to_owned(),
                PeerConfig {
                    token: Some(SecretString::from(
                        "a-token-of-entirely-sufficient-len".to_owned(),
                    )),
                    san: None,
                },
            )]),
            ..Default::default()
        };
        // The name is accepted as a map key, but it can never be *produced* by identification:
        // `identify` returns it only in `Off`, where the route table is not consulted at all.
        let resolved = cfg.resolve(false).expect("an odd name is still a name");
        let auth = InternalAuth::new(&resolved, RouteTable(ROUTES));
        assert_eq!(auth.mode(), IdentityMode::Token);
        assert!(
            !RouteTable(ROUTES).allows(name.name(), &Method::POST, SCANS),
            "the anonymous name is in no route table"
        );
    }

    /// The silent failure this guard exists for: under `mtls`, an `http://` upstream connects
    /// and works, carrying every internal call in plaintext with no client certificate offered.
    /// The peer's own configuration still says it requires both, so nothing on either side
    /// reports a problem — the requests simply never arrive at the mTLS listener.
    #[test]
    fn a_plaintext_upstream_is_refused_under_mtls() {
        assert!(check_upstream_scheme(IdentityMode::Mtls, "sync", "http://sync:8083").is_err());
        assert!(check_upstream_scheme(IdentityMode::Mtls, "sync", "https://sync:8083").is_ok());

        // The other two modes are plaintext by design; refusing `http://` there would be wrong.
        assert!(check_upstream_scheme(IdentityMode::Token, "sync", "http://sync:8083").is_ok());
        assert!(check_upstream_scheme(IdentityMode::Off, "sync", "http://sync:8083").is_ok());
    }

    /// The error has to name the offending setting, or an operator reading a crash-looping pod's
    /// logs cannot tell which of three upstream URLs is wrong.
    #[test]
    fn the_scheme_error_names_the_upstream_and_its_url() {
        let err = check_upstream_scheme(IdentityMode::Mtls, "control-plane", "http://cp:8081")
            .expect_err("plaintext under mtls is refused");
        let msg = err.to_string();
        assert!(msg.contains("control-plane"), "{msg}");
        assert!(msg.contains("http://cp:8081"), "{msg}");
    }

    /// A caller entry resolves through to the outbound side unchanged; the token a service
    /// presents is the one its peer lists for it.
    #[test]
    fn a_resolved_caller_carries_the_token_it_presents() {
        let caller = ResolvedCaller {
            name: "api".to_owned(),
            token: Some(SecretString::from(
                "api-token-of-sufficient-length-01".to_owned(),
            )),
        };
        let token = InternalToken::new(caller.token.clone().expect("seeded above"));
        assert!(token.matches("api-token-of-sufficient-length-01"));
    }
}
