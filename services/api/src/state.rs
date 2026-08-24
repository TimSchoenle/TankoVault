//! Shared application state and the authenticated-principal extractor.
//!
//! Capabilities are resolved from the database per request, not read from the access token,
//! and a suspended account is refused before any capability is consulted.

use crate::error::ApiError;
use axum::extract::{ConnectInfo, FromRequestParts, OptionalFromRequestParts};
use axum::http::header::{AUTHORIZATION, USER_AGENT};
use axum::http::request::Parts;
use secrecy::SecretSlice;
use std::net::SocketAddr;
use std::sync::Arc;
use tankovault_auth::verify_access_token;
use tankovault_db::PgPool;
use tankovault_domain::{Permission, PermissionSet, UserId};
use tankovault_service::{AuditEvent, AuditSink, FeatureGate, TunableSet};

/// Application state shared across handlers.
#[derive(Clone)]
pub struct AppState {
    /// The catalogue. Cloning it clones a handle, not the pool.
    pub pool: PgPool,
    /// HS256 signing key for access tokens.
    ///
    /// `Arc<SecretSlice<u8>>`: axum clones state per request, and `SecretSlice`'s `Clone`
    /// copies the heap allocation, so a bare wrapper would scatter fresh copies of the key
    /// across the heap. The `Arc` keeps one copy, zeroized at shutdown.
    pub jwt_secret: Arc<SecretSlice<u8>>,
    /// Server-side password pepper: mixed into every argon2id hash, held here rather than the
    /// database so a leak alone cannot be brute-forced offline. Empty reproduces un-peppered
    /// hashing for backward compatibility.
    ///
    /// `Arc` for the same reason as [`Self::jwt_secret`].
    pub password_pepper: Arc<SecretSlice<u8>>,
    /// How long a minted access token stays valid, from `auth.access_ttl_minutes`.
    pub access_ttl: time::Duration,
    /// How long a refresh family stays renewable, from `auth.refresh_ttl_days`.
    pub refresh_ttl: time::Duration,
    /// The control-plane, for proxying "Scan now".
    pub control_plane: crate::upstream::Upstream,
    /// The external-sync service, for proxying `/v1/me/sync/*` and the admin sync console.
    pub sync: crate::upstream::Upstream,
    /// The scan worker, for proxying the "Test adapter" dry-run.
    pub worker: crate::upstream::Upstream,
    /// Core-NATS bus for relaying live per-user notifications over SSE; `None` degrades
    /// `/v1/me/stream` to `503` while every other route keeps working.
    pub bus: Option<tankovault_bus::Bus>,
    /// Single-use, 30-second tickets for opening `GET /v1/me/stream` — replaces the access
    /// token in that route's query string. Redis-backed where available, per-process otherwise.
    pub stream_tickets: Arc<dyn crate::stream_tickets::StreamTicketStore>,

    /// Where audit records go. A [`tankovault_service::NoopAuditSink`] when the operator
    /// disabled auditing, so no handler ever branches on the toggle.
    pub audit: Arc<dyn AuditSink>,
    /// Which features are currently switched on. Held here — not just in the middleware layer
    /// — because the flag-write handler has to refresh it, and `/v1/me/capabilities` has to
    /// report it.
    pub features: FeatureGate,
    /// The recommender's tuning, resolved from the compiled registry plus stored overrides.
    /// Held here for the same two reasons [`Self::features`] is: the tuning-write handler has
    /// to refresh it, and the shelf reads it on every request.
    pub tunables: TunableSet,
    /// Whether refresh cookies are marked `Secure` (true in production/TLS).
    pub cookie_secure: bool,
    /// The `WebAuthn` relying party, or `None` when this deployment configured no origin for
    /// it. `None` is a working state, not a broken one — passkeys are simply unavailable.
    pub webauthn: Option<crate::passkey::SharedRelyingParty>,
    /// Seals TOTP secrets at rest, or `None` when `auth.mfa_encryption_key` is unset.
    ///
    /// `None` is a working state, like [`Self::webauthn`]: TOTP enrolment answers `503` naming
    /// the setting, while security keys and recovery codes keep working. Storing the secret
    /// unsealed instead is not an option — it is symmetric, so the database row would be a
    /// working second factor for whoever reads it.
    pub mfa_sealer: Option<tankovault_auth::Sealer>,
    /// The issuer an authenticator app files this deployment's entry under.
    pub totp_issuer: String,
    /// How long a step-up elevation survives *without being used*. Every elevated request slides
    /// it forward, so a session spent working inside one console panel is asked once.
    pub step_up_ttl: time::Duration,
    /// The ceiling on that sliding: no elevation is honoured longer than this after it was
    /// earned, however continuously it is used.
    pub step_up_max_ttl: time::Duration,
    /// How long a half-finished sign-in may sit before it has to be restarted.
    pub mfa_challenge_ttl: time::Duration,
    /// Transactional email back-end (welcome, password reset). A no-op mailer when email
    /// is unconfigured, so these flows degrade gracefully rather than failing.
    pub mailer: Arc<dyn tankovault_email::EmailService>,
    /// Public base URL of the web app, used to build absolute links inside emails
    /// (e.g. the password-reset link). No trailing slash.
    pub email_base_url: String,
    /// The operator's legal documents, read through an mtime check. Empty is a working state:
    /// the footer simply publishes no Legal column.
    pub legal: crate::legal::LegalDocs,
    /// What this deployment calls itself: the name stamped into email and the authenticator
    /// prompt, and the identity `GET /v1/branding` publishes to the client.
    pub branding: crate::branding::Branding,
    /// Which repository the native client updates from and which client versions this
    /// deployment supports, as `GET /v1/client` publishes them.
    pub client_channel: crate::client::ClientChannel,
    /// The console's system rollup, cached: it is a `count(*)` over every large table.
    pub system_stats: Arc<crate::cache::Cached<tankovault_db::repo::stats::SystemStats>>,
    /// The console's per-provider table, cached: it aggregates every chapter row by provider,
    /// and two console tabs request it.
    pub provider_stats: Arc<crate::cache::Cached<Vec<tankovault_db::repo::stats::ProviderStat>>>,
    /// The genre terms that classify a series as adult, as intake applies them.
    ///
    /// The same set the worker ingests with, so the public tag facet withholds exactly the terms
    /// that put a series behind the gate — a second, API-local list would drift from the one that
    /// did the classifying and start naming genres the reader can, or cannot, actually reach.
    ///
    /// `Arc` because axum clones state per request, and this is a hash set.
    pub adult_tags: Arc<tankovault_domain::AdultTagSet>,
}

/// Where a request came from, for the audit trail.
///
/// Persisted only when the operator enabled the privacy toggle — filtering happens in the
/// sink, so a handler can't accidentally retain an IP.
#[derive(Debug, Clone, Default)]
pub struct ClientContext {
    pub ip: Option<String>,
    pub user_agent: Option<String>,
}

/// Extract [`ClientContext`] on an unauthenticated route.
///
/// Infallible — a missing peer address or `User-Agent` yields `None`, not an error, since
/// credential endpoints need audit context without an `AuthUser` to carry it.
impl<S: Send + Sync> FromRequestParts<S> for ClientContext {
    type Rejection = std::convert::Infallible;

    async fn from_request_parts(parts: &mut Parts, _state: &S) -> Result<Self, Self::Rejection> {
        Ok(Self::from_parts(parts))
    }
}

impl ClientContext {
    /// Read the peer address and `User-Agent` from the request.
    ///
    /// Uses the connection's peer address, not `X-Forwarded-For`: a client-supplied address
    /// that reads as authoritative is worse than none. A proxy deployment should record the
    /// real client at the proxy, where the value can be trusted.
    pub(crate) fn from_parts(parts: &Parts) -> Self {
        Self {
            ip: parts
                .extensions
                .get::<ConnectInfo<SocketAddr>>()
                .map(|ConnectInfo(addr)| addr.ip().to_string()),
            user_agent: parts
                .headers
                .get(USER_AGENT)
                .and_then(|v| v.to_str().ok())
                .map(str::to_owned),
        }
    }
}

/// An authenticated principal: a verified identity plus the capabilities it currently holds.
pub struct AuthUser {
    pub user_id: UserId,
    /// The capabilities resolved for this request. Freshly read, so a grant revoked a second
    /// ago is already gone.
    pub permissions: PermissionSet,
    /// Whether this account may see adult-gated series, as far as the *account* decides it.
    ///
    /// Half the answer. The deployment flag is the other half and both must hold, which is why
    /// no handler reads this directly — [`crate::content_gate::AdultVisibility`] combines them.
    pub adult_opt_in: bool,
    /// Request origin, attached to any audit record this principal produces.
    pub client: ClientContext,
    /// Whether this account holds a second factor — a confirmed authenticator-app enrolment or
    /// a security key.
    ///
    /// Read on every authenticated request, alongside the permission set, so a factor removed a
    /// second ago is already gone. Consulted by [`Self::require_all`] and by the passkey gate.
    pub mfa_enrolled: bool,
    /// Whether this request carried a live step-up grant in `X-Step-Up`.
    ///
    /// Resolved here rather than by a second extractor so that [`Self::require_all`] — the
    /// funnel every privileged handler already passes through — can demand it for a mutating
    /// capability without each of forty admin handlers being edited to ask. `crate::step_up`
    /// reads the same field for the `/v1/me` routes, which do not go through `require_all`.
    ///
    /// A grant earned by password is **not** counted once a factor is enrolled: see
    /// `crate::step_up` for why.
    pub elevated: bool,
    /// Carried so [`Self::require`] can record a refused privileged action without every
    /// handler having to thread `AppState` into its authorization check.
    audit: Arc<dyn AuditSink>,
}

impl AuthUser {
    /// Enforce a single capability, returning `Forbidden` otherwise.
    ///
    /// A refusal is audited — the whole reason this is `async`: an unauthorized attempt is
    /// the single most interesting thing an audit trail can tell you.
    ///
    /// # Errors
    /// [`ApiError::Forbidden`] if the principal does not hold `required`.
    pub async fn require(&self, required: Permission) -> Result<(), ApiError> {
        self.require_all(&[required]).await
    }

    /// Enforce several capabilities at once.
    ///
    /// Permissions deliberately do not imply one another, so a dual-purpose handler asks for
    /// both at once — keeping the audit record for a refusal naming everything missing.
    ///
    /// # Two gates before the grants
    ///
    /// Both are checked here, and here rather than anywhere else, because this is the one
    /// funnel every privileged handler already passes through — every call site of this method
    /// lives under `services/api/src/admin/`. A middleware keyed on path prefix would let a
    /// privileged route added outside `/v1/admin` escape silently; this way a handler cannot
    /// ask for a capability without also asking for what guards it.
    ///
    /// 1. **The account must hold a second factor at all.** An administrator without one is a
    ///    password away from being someone else.
    /// 2. **A mutating capability additionally needs a fresh step-up.** Reads are exempt:
    ///    prompting to load a dashboard would keep a standing elevation open all day, which is
    ///    worse than not prompting. [`Permission::is_mutating`] draws the line, exhaustively.
    ///
    /// Both are ordered before the grant check on purpose: an unenrolled administrator is told
    /// to enrol rather than told their permissions are insufficient, which is the difference
    /// between an actionable message and a support ticket.
    ///
    /// # Errors
    /// [`ApiError::MfaEnrolmentRequired`] if the caller has no second factor;
    /// [`ApiError::StepUpRequired`] if a mutating capability was asked for without one;
    /// [`ApiError::Forbidden`] if any of `required` is missing.
    pub async fn require_all(&self, required: &[Permission]) -> Result<(), ApiError> {
        if !self.mfa_enrolled {
            self.audit
                .record(
                    AuditEvent::new("authz.denied")
                        .actor(self.user_id)
                        .detail(serde_json::json!({
                            "reason": "mfa_enrolment_required",
                            "required": required.iter().map(|p| p.as_str()).collect::<Vec<_>>(),
                        }))
                        .denied()
                        .client(self.client.ip.clone(), self.client.user_agent.clone()),
                )
                .await;
            return Err(ApiError::MfaEnrolmentRequired);
        }

        if !self.elevated && required.iter().copied().any(Permission::is_mutating) {
            self.audit
                .record(
                    AuditEvent::new("authz.denied")
                        .actor(self.user_id)
                        .detail(serde_json::json!({
                            "reason": "step_up_required",
                            "required": required.iter().map(|p| p.as_str()).collect::<Vec<_>>(),
                        }))
                        .denied()
                        .client(self.client.ip.clone(), self.client.user_agent.clone()),
                )
                .await;
            return Err(ApiError::StepUpRequired);
        }

        let missing: Vec<&'static str> = required
            .iter()
            .filter(|p| !self.permissions.has(**p))
            .map(|p| p.as_str())
            .collect();
        if missing.is_empty() {
            return Ok(());
        }

        self.audit
            .record(
                AuditEvent::new("authz.denied")
                    .actor(self.user_id)
                    .detail(serde_json::json!({
                        "required": required.iter().map(|p| p.as_str()).collect::<Vec<_>>(),
                        "missing": missing,
                    }))
                    .denied()
                    .client(self.client.ip.clone(), self.client.user_agent.clone()),
            )
            .await;
        Err(ApiError::Forbidden)
    }

    /// Build an audit event already attributed to this principal and its request origin.
    #[must_use]
    pub fn event(&self, action: &'static str) -> AuditEvent {
        AuditEvent::new(action)
            .actor(self.user_id)
            .client(self.client.ip.clone(), self.client.user_agent.clone())
    }
}

impl FromRequestParts<AppState> for AuthUser {
    type Rejection = ApiError;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &AppState,
    ) -> Result<Self, Self::Rejection> {
        let header = parts
            .headers
            .get(AUTHORIZATION)
            .and_then(|v| v.to_str().ok())
            .ok_or(ApiError::Unauthorized)?;
        let token = header
            .strip_prefix("Bearer ")
            .ok_or(ApiError::Unauthorized)?;

        let claims = verify_access_token(&state.jwt_secret, token)?;
        let user_id = claims.user_id().ok_or(ApiError::Unauthorized)?;

        // A valid signature proves the token was ours; it does not prove the account still
        // exists, is still permitted to act, or still holds what it held when the token was
        // minted. All three are settled here.
        let principal = tankovault_db::repo::permissions::resolve(&state.pool, user_id)
            .await?
            .ok_or(ApiError::Unauthorized)?;

        if !principal.status.may_authenticate() {
            return Err(ApiError::Suspended);
        }

        // The deployment-wide requirement, distinct from the privileged-account one in
        // `require_all`. Enforced here because it applies to *every* authenticated route, and
        // exempting the enrolment surface is what stops it bricking the deployment the moment
        // an operator switches it on.
        if !principal.mfa_enrolled
            && state
                .features
                .is_enabled(tankovault_domain::Feature::AccountsMfaRequired)
            && !exempt_from_mandatory_mfa(parts.uri.path())
        {
            return Err(ApiError::MfaEnrolmentRequired);
        }

        Ok(Self {
            user_id,
            permissions: principal.permissions,
            adult_opt_in: principal.adult_opt_in,
            client: ClientContext::from_parts(parts),
            mfa_enrolled: principal.mfa_enrolled,
            elevated: crate::step_up::resolve(
                state,
                user_id,
                principal.mfa_enrolled,
                &parts.headers,
            )
            .await?,
            audit: Arc::clone(&state.audit),
        })
    }
}

/// Routes an unenrolled account may still reach while `accounts.mfa_required` is on.
///
/// Exactly the surface needed to *become* enrolled, plus the probe the client uses to discover
/// that it must. Without this the flag is a deployment-wide lockout: every account without a
/// factor is refused everywhere, including the page that would give them one, and the only
/// recovery is a database edit.
///
/// Prefix-matched rather than exact, because `/v1/me/mfa` fans out into enrolment,
/// confirmation, security keys and recovery codes, and a list of exact paths is a list that
/// falls behind the router. Nothing outside `/v1/me/mfa` and `/v1/me/step-up` is exempt, and
/// neither prefix carries a route that does anything but enrol or elevate.
fn exempt_from_mandatory_mfa(path: &str) -> bool {
    path.starts_with("/v1/me/mfa")
        || path.starts_with("/v1/me/step-up")
        || path == "/v1/me/capabilities"
}

/// Extract an [`AuthUser`] on a route that also serves anonymous callers.
///
/// `Ok(None)` for *every* reason a principal cannot be established — no header, a malformed or
/// expired token, an erased account — because on these routes all of them mean the same thing:
/// nobody is signed in, serve the public view. A suspended account is the one case worth
/// distinguishing, and it is not: it also gets the public view, which is strictly less than it
/// would get by authenticating.
///
/// Never use this where a route's *authorization* depends on the result. Anything that grants
/// access must extract [`AuthUser`] itself and take the 401.
impl OptionalFromRequestParts<AppState> for AuthUser {
    type Rejection = std::convert::Infallible;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &AppState,
    ) -> Result<Option<Self>, Self::Rejection> {
        Ok(
            <Self as FromRequestParts<AppState>>::from_request_parts(parts, state)
                .await
                .ok(),
        )
    }
}
