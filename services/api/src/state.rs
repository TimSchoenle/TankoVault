//! Shared application state and the authenticated-principal extractor.
//!
//! # Authorization model
//!
//! Every privileged handler asks for a named capability —
//! `user.require(Permission::ProvidersDelete).await?` — and the capability set is resolved
//! from the database on each request rather than read out of the access token. See
//! [`tankovault_domain::permissions`] for why the previous ordered `user < operator < admin`
//! role was removed rather than extended, and
//! [`tankovault_db::repo::permissions`] for why resolution is per-request.
//!
//! The extractor also refuses a **suspended** account outright, before any capability is
//! consulted. Suspension is not the absence of permissions: an account with no grants can
//! still read its own watchlist, which is precisely what a suspended one must not do.

use crate::error::ApiError;
use axum::extract::{ConnectInfo, FromRequestParts};
use axum::http::header::{AUTHORIZATION, USER_AGENT};
use axum::http::request::Parts;
use std::net::SocketAddr;
use std::sync::Arc;
use tankovault_auth::verify_access_token;
use tankovault_db::PgPool;
use tankovault_domain::{Permission, PermissionSet, UserId};
use tankovault_service::{AuditEvent, AuditSink, FeatureGate};

/// Application state shared across handlers.
#[derive(Clone)]
pub struct AppState {
    pub pool: PgPool,
    pub jwt_secret: Arc<Vec<u8>>,
    /// Server-side password pepper: a secret mixed into every argon2id hash as its keyed
    /// input, held here rather than in the database so a database leak alone cannot be
    /// brute-forced offline. Empty when the operator configured none, which reproduces
    /// un-peppered hashing for backward compatibility.
    pub password_pepper: Arc<Vec<u8>>,
    pub access_ttl: time::Duration,
    pub refresh_ttl: time::Duration,
    /// The control-plane, for proxying "Scan now".
    pub control_plane: crate::upstream::Upstream,
    /// The external-sync service, for proxying `/v1/me/sync/*` and the admin sync console.
    pub sync: crate::upstream::Upstream,
    /// Endpoint of the challenge-solver service, used by the "Test adapter" dry-run. A bare
    /// URL rather than an [`crate::upstream::Upstream`]: it is handed to the adapter fetch
    /// stack, which builds its own client.
    pub challenge_solver_url: String,
    /// Presented to the challenge-solver by the adapter fetch stack, which cannot go through
    /// [`crate::upstream::Upstream`].
    pub internal_token: Option<tankovault_service::InternalToken>,
    /// Core-NATS bus for relaying live per-user notifications over SSE. `None` when NATS
    /// was unreachable at boot: the live-stream endpoint degrades to `503` while every
    /// other route (including the durable notifications list) keeps working.
    pub bus: Option<tankovault_bus::Bus>,
    /// Single-use, 30-second tickets for opening `GET /v1/me/stream` — the credential that
    /// replaced the access token in that route's query string (SEC-8). Redis-backed where Redis
    /// is available, per-process otherwise; see [`crate::stream_tickets`].
    pub stream_tickets: Arc<dyn crate::stream_tickets::StreamTicketStore>,

    /// Where audit records go. A [`tankovault_service::NoopAuditSink`] when the operator
    /// disabled auditing, so no handler ever branches on the toggle.
    pub audit: Arc<dyn AuditSink>,
    /// Which features are currently switched on. Held here — not just in the middleware layer
    /// — because the flag-write handler has to refresh it, and `/v1/me/capabilities` has to
    /// report it.
    pub features: FeatureGate,
    /// Whether refresh cookies are marked `Secure` (true in production/TLS).
    pub cookie_secure: bool,
    /// Transactional email back-end (welcome, password reset). A no-op mailer when email
    /// is unconfigured, so these flows degrade gracefully rather than failing.
    pub mailer: Arc<dyn tankovault_email::EmailService>,
    /// Public base URL of the web app, used to build absolute links inside emails
    /// (e.g. the password-reset link). No trailing slash.
    pub email_base_url: String,
}

/// Where a request came from, for the audit trail.
///
/// Captured for every authenticated request but persisted only when the operator enabled
/// the corresponding privacy toggle — the filtering happens in the sink, so a handler
/// cannot accidentally retain an IP by constructing an event differently.
#[derive(Debug, Clone, Default)]
pub struct ClientContext {
    pub ip: Option<String>,
    pub user_agent: Option<String>,
}

/// Extract [`ClientContext`] on an unauthenticated route.
///
/// Infallible — a missing peer address or `User-Agent` yields `None` rather than an
/// error, because failing a login request over a missing header would be absurd. Exists so
/// the credential endpoints, which have no `AuthUser` to carry the context, can still
/// produce audit records that name where the attempt came from.
impl<S: Send + Sync> FromRequestParts<S> for ClientContext {
    type Rejection = std::convert::Infallible;

    async fn from_request_parts(parts: &mut Parts, _state: &S) -> Result<Self, Self::Rejection> {
        Ok(Self::from_parts(parts))
    }
}

impl ClientContext {
    /// Read the peer address and `User-Agent` from the request.
    ///
    /// Uses the connection's peer address rather than `X-Forwarded-For`: an audit record
    /// naming a client-supplied address is worse than one naming none, because it reads
    /// as authoritative. A deployment behind a proxy should record the real client at the
    /// proxy, where the value can be trusted.
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
    /// Request origin, attached to any audit record this principal produces.
    pub client: ClientContext,
    /// Carried so [`Self::require`] can record a refused privileged action without every
    /// handler having to thread `AppState` into its authorization check.
    audit: Arc<dyn AuditSink>,
}

impl AuthUser {
    /// Enforce a single capability, returning `Forbidden` otherwise.
    ///
    /// A refusal is audited. That is the whole reason this is `async`: an attempt to use a
    /// privileged endpoint without the capability — the single most interesting thing an audit
    /// trail can tell you — would otherwise return `403` and leave no trace.
    ///
    /// # Errors
    /// [`ApiError::Forbidden`] if the principal does not hold `required`.
    pub async fn require(&self, required: Permission) -> Result<(), ApiError> {
        self.require_all(&[required]).await
    }

    /// Enforce several capabilities at once.
    ///
    /// Exists because permissions deliberately do not imply one another: a handler that both
    /// reads and writes has to ask for both, and asking in one call keeps the audit record for
    /// a refusal naming *everything* that was missing rather than only the first check to fail.
    ///
    /// # Errors
    /// [`ApiError::Forbidden`] if any of `required` is missing.
    pub async fn require_all(&self, required: &[Permission]) -> Result<(), ApiError> {
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

        Ok(Self {
            user_id,
            permissions: principal.permissions,
            client: ClientContext::from_parts(parts),
            audit: Arc::clone(&state.audit),
        })
    }
}
