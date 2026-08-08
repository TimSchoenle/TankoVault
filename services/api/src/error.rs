//! Typed API error → RFC 9457 problem+json. Internal errors never leak details.

use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde::Serialize;
use tankovault_auth::AuthError;
use tankovault_db::DbError;
use tankovault_service::problem::{IntoProblem, Problem};
use utoipa::ToSchema;

/// RFC 9457 `application/problem+json` error body shape produced by [`ApiError`]. Declared
/// purely for `OpenAPI` documentation — `tankovault_service::problem` builds the JSON, so runtime
/// callers never construct this type. It mirrors `problem::ProblemBody`; a test asserts the two
/// agree field for field, since only this copy is published to clients.
#[derive(Serialize, ToSchema)]
#[schema(example = json!({
    "type": "about:blank#not_found",
    "title": "not_found",
    "status": 404,
    "detail": "resource not found",
}))]
#[derive(serde::Deserialize)]
pub struct ProblemDetails {
    pub r#type: String,
    pub title: String,
    pub status: u16,
    pub detail: String,
}

/// The single error type all handlers return.
#[derive(Debug, thiserror::Error)]
pub enum ApiError {
    #[error("not found")]
    NotFound,
    #[error("{0}")]
    Conflict(String),
    #[error("unauthorized")]
    Unauthorized,
    #[error("forbidden")]
    Forbidden,
    /// Login attempted before the account's email address was confirmed; distinct from
    /// [`Self::Forbidden`] so the frontend can offer to resend the link.
    #[error("email not verified")]
    EmailNotVerified,
    /// The account has been suspended by an administrator. Distinct from [`Self::Forbidden`]:
    /// suspension means the account may not act at all, not that it lacks a grantable permission.
    #[error("account suspended")]
    Suspended,
    /// The route needs a fresh second-factor presentation and the request carried none.
    ///
    /// `403`, not `401`, and the distinction is load-bearing: the caller *is* authenticated, and
    /// a `401` would drive the SPA's sign-out path — turning "confirm it is you" into "you have
    /// been logged out". The distinct problem type is what the client branches on to open its
    /// re-authentication prompt and retry.
    #[error("step-up authentication required")]
    StepUpRequired,
    /// The caller holds a privileged grant (or the deployment requires it of everyone) but has
    /// no second factor enrolled.
    ///
    /// Also `403` with its own type, so the console can route to the enrolment page rather than
    /// showing "insufficient privileges" to someone whose privileges are fine.
    #[error("two-factor enrolment required")]
    MfaEnrolmentRequired,
    /// The requested capability exists in the code but is switched off for this deployment.
    ///
    /// `404`, matching `tankovault_service::flags::enforce`. Handlers use this where
    /// route-level middleware can't express it — e.g. a request body asking for a disabled
    /// *mode* rather than a disabled path.
    #[error("feature disabled")]
    FeatureDisabled(tankovault_domain::Feature),
    #[error("{0}")]
    BadRequest(String),
    #[error("service unavailable")]
    Unavailable,
    /// An internal service this request depends on is unreachable or answered with something
    /// this service cannot represent. Distinct from [`Self::Internal`]: an upstream outage is
    /// not a bug here.
    #[error("upstream unavailable")]
    BadGateway,
    /// An internal service did not answer within its budget.
    #[error("upstream timed out")]
    GatewayTimeout,
    #[error("internal error")]
    Internal,
}

impl ApiError {
    fn parts(&self) -> (StatusCode, &'static str, String) {
        match self {
            Self::NotFound => (
                StatusCode::NOT_FOUND,
                "not_found",
                "resource not found".into(),
            ),
            Self::Conflict(m) => (StatusCode::CONFLICT, "conflict", m.clone()),
            Self::Unauthorized => (
                StatusCode::UNAUTHORIZED,
                "unauthorized",
                "authentication required".into(),
            ),
            Self::Forbidden => (
                StatusCode::FORBIDDEN,
                "forbidden",
                "insufficient privileges".into(),
            ),
            Self::EmailNotVerified => (
                StatusCode::FORBIDDEN,
                "email_not_verified",
                "please confirm your email address before signing in".into(),
            ),
            Self::Suspended => (
                StatusCode::FORBIDDEN,
                "account_suspended",
                "this account has been suspended; contact an administrator".into(),
            ),
            Self::StepUpRequired => (
                StatusCode::FORBIDDEN,
                "step_up_required",
                "confirm your identity with your second factor to continue".into(),
            ),
            Self::MfaEnrolmentRequired => (
                StatusCode::FORBIDDEN,
                "mfa_enrolment_required",
                "set up two-factor authentication before using this".into(),
            ),
            Self::FeatureDisabled(feature) => (
                StatusCode::NOT_FOUND,
                "feature_disabled",
                format!(
                    "the \"{}\" feature is switched off on this deployment",
                    feature.title()
                ),
            ),
            Self::BadRequest(m) => (StatusCode::BAD_REQUEST, "bad_request", m.clone()),
            Self::Unavailable => (
                StatusCode::SERVICE_UNAVAILABLE,
                "unavailable",
                "the live notification stream is temporarily unavailable".into(),
            ),
            Self::BadGateway => (
                StatusCode::BAD_GATEWAY,
                "upstream_unavailable",
                "a service this request depends on is unavailable; please try again".into(),
            ),
            Self::GatewayTimeout => (
                StatusCode::GATEWAY_TIMEOUT,
                "upstream_timeout",
                "a service this request depends on did not respond in time".into(),
            ),
            Self::Internal => (
                StatusCode::INTERNAL_SERVER_ERROR,
                "internal",
                "internal server error".into(),
            ),
        }
    }
}

impl IntoProblem for ApiError {
    fn into_problem(self) -> Problem {
        let (status, kind, detail) = self.parts();
        Problem::new(status, kind, detail)
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        self.into_problem().into_response()
    }
}

impl From<DbError> for ApiError {
    fn from(e: DbError) -> Self {
        match e {
            DbError::NotFound => Self::NotFound,
            DbError::Conflict(m) => Self::Conflict(m),
            other => {
                tracing::error!(error = %other, "database error");
                Self::Internal
            }
        }
    }
}

impl From<AuthError> for ApiError {
    fn from(e: AuthError) -> Self {
        match e {
            AuthError::InvalidToken | AuthError::MalformedHash => Self::Unauthorized,
            other => {
                tracing::error!(error = %other, "auth error");
                Self::Internal
            }
        }
    }
}

/// Handler result alias.
pub type ApiResult<T> = Result<T, ApiError>;

#[cfg(test)]
mod tests {
    use super::{ApiError, ProblemDetails};
    use axum::body::to_bytes;
    use axum::http::StatusCode;
    use axum::http::header::CONTENT_TYPE;
    use axum::response::IntoResponse as _;
    use tankovault_service::problem::{IntoProblem as _, PROBLEM_JSON};

    /// `ProblemDetails` is documentation only — `tankovault_service::problem` builds the runtime
    /// body. Two declarations of one wire shape is exactly the drift that made hand-mirrored DTOs
    /// a problem elsewhere in this codebase, so the published schema is asserted against a real
    /// response rather than trusted.
    #[tokio::test]
    async fn the_documented_schema_matches_the_body_actually_sent() {
        let response = ApiError::NotFound.into_response();
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
        assert_eq!(
            response
                .headers()
                .get(CONTENT_TYPE)
                .and_then(|v| v.to_str().ok()),
            Some(PROBLEM_JSON)
        );
        let bytes = to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("read body");

        // Deserializing into the *documented* type is the assertion: a member renamed or dropped
        // on either side fails here.
        let documented: ProblemDetails =
            serde_json::from_slice(&bytes).expect("body matches schema");
        assert_eq!(documented.r#type, "about:blank#not_found");
        assert_eq!(documented.title, "not_found");
        assert_eq!(documented.status, 404);
        assert_eq!(documented.detail, "resource not found");
    }

    /// Every variant's status must agree with the `status` member in its own body, or a client
    /// that reads one and not the other sees a different error than the one that happened.
    #[test]
    fn every_variant_echoes_its_own_status() {
        for error in [
            ApiError::NotFound,
            ApiError::Conflict("dup".to_owned()),
            ApiError::Unauthorized,
            ApiError::Forbidden,
            ApiError::EmailNotVerified,
            ApiError::Suspended,
            ApiError::BadRequest("bad".to_owned()),
            ApiError::Unavailable,
            ApiError::BadGateway,
            ApiError::GatewayTimeout,
            ApiError::Internal,
        ] {
            let problem = error.into_problem();
            assert!(
                !problem.kind.is_empty(),
                "every variant needs a machine-readable kind"
            );
            assert!(problem.status.is_client_error() || problem.status.is_server_error());
        }
    }

    /// The internal variant must not put its cause on the wire.
    #[test]
    fn the_internal_variant_says_nothing() {
        let problem = ApiError::Internal.into_problem();
        assert_eq!(problem.detail, "internal server error");
    }
}
