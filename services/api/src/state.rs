//! Shared application state and the authenticated-user extractor (RBAC).

use crate::error::ApiError;
use axum::extract::FromRequestParts;
use axum::http::header::AUTHORIZATION;
use axum::http::request::Parts;
use std::sync::Arc;
use tankovault_auth::verify_access_token;
use tankovault_db::PgPool;
use tankovault_domain::{UserId, UserRole};
use tankovault_observability::PrometheusHandle;

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
    pub metrics: PrometheusHandle,
    /// Whether refresh cookies are marked `Secure` (true in production/TLS).
    pub cookie_secure: bool,
    /// Transactional email back-end (welcome, password reset). A no-op mailer when email
    /// is unconfigured, so these flows degrade gracefully rather than failing.
    pub mailer: Arc<dyn tankovault_email::EmailService>,
    /// Public base URL of the web app, used to build absolute links inside emails
    /// (e.g. the password-reset link). No trailing slash.
    pub email_base_url: String,
}

/// An authenticated principal, extracted from a `Bearer` access token.
pub struct AuthUser {
    pub user_id: UserId,
    pub role: UserRole,
}

impl AuthUser {
    /// Enforce a minimum RBAC role, returning `Forbidden` otherwise.
    ///
    /// # Errors
    /// [`ApiError::Forbidden`] if the principal's role is below `required`.
    pub fn require(&self, required: UserRole) -> Result<(), ApiError> {
        if self.role.at_least(required) {
            Ok(())
        } else {
            Err(ApiError::Forbidden)
        }
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
        })
    }
}
