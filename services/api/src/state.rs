//! Shared application state and the authenticated-user extractor (RBAC).

use crate::error::ApiError;
use axum::extract::{ConnectInfo, FromRequestParts};
use axum::http::header::{AUTHORIZATION, USER_AGENT};
use axum::http::request::Parts;
use std::net::SocketAddr;
use std::sync::Arc;
use tankovault_auth::verify_access_token;
use tankovault_db::PgPool;
use tankovault_domain::{UserId, UserRole};
use tankovault_service::{AuditEvent, AuditSink};

/// Application state shared across handlers.
#[derive(Clone)]
pub struct AppState {
    pub pool: PgPool,
    pub jwt_secret: Arc<Vec<u8>>,
    pub access_ttl: time::Duration,
    pub refresh_ttl: time::Duration,
    /// Base URL of the control-plane, for proxying "Scan now".
    pub control_plane_url: String,
    /// Base URL of the `AniList` sync service, for proxying `/me/sync/anilist/*`.
    pub sync_url: String,
    /// Endpoint of the challenge-solver service, used by the "Test adapter" dry-run.
    pub challenge_solver_url: String,
    /// Core-NATS bus for relaying live per-user notifications over SSE. `None` when NATS
    /// was unreachable at boot: the live-stream endpoint degrades to `503` while every
    /// other route (including the durable notifications list) keeps working.
    pub bus: Option<tankovault_bus::Bus>,
    pub http: reqwest::Client,
    /// Where audit records go. A [`tankovault_service::NoopAuditSink`] when the operator
    /// disabled auditing, so no handler ever branches on the toggle.
    pub audit: Arc<dyn AuditSink>,
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

/// An authenticated principal, extracted from a `Bearer` access token.
pub struct AuthUser {
    pub user_id: UserId,
    pub role: UserRole,
    /// Request origin, attached to any audit record this principal produces.
    pub client: ClientContext,
    /// Carried so [`Self::require`] can record a refused privileged action without every
    /// handler having to thread `AppState` into its authorization check.
    audit: Arc<dyn AuditSink>,
}

impl AuthUser {
    /// Enforce a minimum RBAC role, returning `Forbidden` otherwise.
    ///
    /// A refusal is audited. That is the whole reason this is `async`: the previous
    /// version returned `403` and left no trace, so an attempt to use a privileged
    /// endpoint without the role — the single most interesting thing an audit trail can
    /// tell you — was invisible.
    ///
    /// # Errors
    /// [`ApiError::Forbidden`] if the principal's role is below `required`.
    pub async fn require(&self, required: UserRole) -> Result<(), ApiError> {
        if self.role.at_least(required) {
            return Ok(());
        }
        self.audit
            .record(
                AuditEvent::new("authz.denied")
                    .actor(self.user_id)
                    .detail(serde_json::json!({
                        "required_role": required.to_string(),
                        "actual_role": self.role.to_string(),
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
        Ok(Self {
            user_id: claims.user_id().ok_or(ApiError::Unauthorized)?,
            role: claims.role(),
            client: ClientContext::from_parts(parts),
            audit: Arc::clone(&state.audit),
        })
    }
}
