//! Typed API error → RFC 9457 problem+json. Internal errors never leak details.

use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde::Serialize;
use tankovault_auth::AuthError;
use tankovault_db::DbError;
use tankovault_service::problem::{IntoProblem, Problem};
use utoipa::ToSchema;

/// The closed vocabulary of `title` values an API response can carry.
///
/// Published as a schema enum so clients branch on a generated type rather than on string
/// literals. That makes the list a **contract**: a token missing from here fails to deserialise
/// in the generated client, which turns a clean error response into
/// `progenitor_client::Error::InvalidResponsePayload` — the server's message is lost and the
/// caller sees "unreadable response" instead of the refusal. So every problem body reachable on
/// an API route needs a variant, including the two the shared middleware emits without ever
/// building an [`ApiError`]: [`Self::FeatureDisabled`] and [`Self::RateLimited`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, serde::Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum ProblemKind {
    NotFound,
    Conflict,
    Unauthorized,
    Forbidden,
    EmailNotVerified,
    AccountSuspended,
    StepUpRequired,
    MfaEnrolmentRequired,
    /// Also emitted by `tankovault_service::flags::enforce`, which never builds an [`ApiError`].
    FeatureDisabled,
    /// Emitted only by `tankovault_service::ratelimit`, never by [`ApiError`].
    RateLimited,
    BadRequest,
    Unavailable,
    UpstreamUnavailable,
    UpstreamTimeout,
    Internal,
}

impl ProblemKind {
    /// Every published token, for the tests reconciling this list against what is emitted. An
    /// omission here fails those tests; the enum itself is what `openapi.json` is built from.
    #[cfg(test)]
    pub const ALL: [Self; 15] = [
        Self::NotFound,
        Self::Conflict,
        Self::Unauthorized,
        Self::Forbidden,
        Self::EmailNotVerified,
        Self::AccountSuspended,
        Self::StepUpRequired,
        Self::MfaEnrolmentRequired,
        Self::FeatureDisabled,
        Self::RateLimited,
        Self::BadRequest,
        Self::Unavailable,
        Self::UpstreamUnavailable,
        Self::UpstreamTimeout,
        Self::Internal,
    ];

    /// The wire token, as it appears in `title` and in the fragment of `type`.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::NotFound => "not_found",
            Self::Conflict => "conflict",
            Self::Unauthorized => "unauthorized",
            Self::Forbidden => "forbidden",
            Self::EmailNotVerified => "email_not_verified",
            Self::AccountSuspended => "account_suspended",
            Self::StepUpRequired => "step_up_required",
            Self::MfaEnrolmentRequired => "mfa_enrolment_required",
            Self::FeatureDisabled => "feature_disabled",
            Self::RateLimited => "rate_limited",
            Self::BadRequest => "bad_request",
            Self::Unavailable => "unavailable",
            Self::UpstreamUnavailable => "upstream_unavailable",
            Self::UpstreamTimeout => "upstream_timeout",
            Self::Internal => "internal",
        }
    }
}

/// RFC 9457 `application/problem+json` error body shape produced by [`ApiError`]. Declared
/// purely for `OpenAPI` documentation — `tankovault_service::problem` builds the JSON, so runtime
/// callers never construct this type. It mirrors `problem::ProblemBody`; a test asserts the two
/// agree field for field, since only this copy is published to clients.
#[derive(Serialize, serde::Deserialize, ToSchema)]
#[schema(example = json!({
    "type": "about:blank#not_found",
    "title": "not_found",
    "status": 404,
    "detail": "resource not found",
}))]
pub struct ProblemDetails {
    pub r#type: String,
    pub title: ProblemKind,
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
    fn parts(&self) -> (StatusCode, ProblemKind, String) {
        match self {
            Self::NotFound => (
                StatusCode::NOT_FOUND,
                ProblemKind::NotFound,
                "resource not found".into(),
            ),
            Self::Conflict(m) => (StatusCode::CONFLICT, ProblemKind::Conflict, m.clone()),
            Self::Unauthorized => (
                StatusCode::UNAUTHORIZED,
                ProblemKind::Unauthorized,
                "authentication required".into(),
            ),
            Self::Forbidden => (
                StatusCode::FORBIDDEN,
                ProblemKind::Forbidden,
                "insufficient privileges".into(),
            ),
            Self::EmailNotVerified => (
                StatusCode::FORBIDDEN,
                ProblemKind::EmailNotVerified,
                "please confirm your email address before signing in".into(),
            ),
            Self::Suspended => (
                StatusCode::FORBIDDEN,
                ProblemKind::AccountSuspended,
                "this account has been suspended; contact an administrator".into(),
            ),
            Self::StepUpRequired => (
                StatusCode::FORBIDDEN,
                ProblemKind::StepUpRequired,
                "confirm your identity with your second factor to continue".into(),
            ),
            Self::MfaEnrolmentRequired => (
                StatusCode::FORBIDDEN,
                ProblemKind::MfaEnrolmentRequired,
                "set up two-factor authentication before using this".into(),
            ),
            Self::FeatureDisabled(feature) => (
                StatusCode::NOT_FOUND,
                ProblemKind::FeatureDisabled,
                format!(
                    "the \"{}\" feature is switched off on this deployment",
                    feature.title()
                ),
            ),
            Self::BadRequest(m) => (StatusCode::BAD_REQUEST, ProblemKind::BadRequest, m.clone()),
            Self::Unavailable => (
                StatusCode::SERVICE_UNAVAILABLE,
                ProblemKind::Unavailable,
                "the live notification stream is temporarily unavailable".into(),
            ),
            Self::BadGateway => (
                StatusCode::BAD_GATEWAY,
                ProblemKind::UpstreamUnavailable,
                "a service this request depends on is unavailable; please try again".into(),
            ),
            Self::GatewayTimeout => (
                StatusCode::GATEWAY_TIMEOUT,
                ProblemKind::UpstreamTimeout,
                "a service this request depends on did not respond in time".into(),
            ),
            Self::Internal => (
                StatusCode::INTERNAL_SERVER_ERROR,
                ProblemKind::Internal,
                "internal server error".into(),
            ),
        }
    }
}

impl IntoProblem for ApiError {
    fn into_problem(self) -> Problem {
        let (status, kind, detail) = self.parts();
        Problem::new(status, kind.as_str(), detail)
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
    use super::{ApiError, ProblemDetails, ProblemKind};
    use axum::body::to_bytes;
    use axum::http::StatusCode;
    use axum::http::header::CONTENT_TYPE;
    use axum::response::{IntoResponse as _, Response};
    use tankovault_domain::Feature;
    use tankovault_service::problem::{IntoProblem as _, PROBLEM_JSON};

    /// Every `ApiError` variant, so the loops below cannot quietly stop covering one.
    ///
    /// The exhaustive match below the list is what makes that hold: a new variant stops this
    /// file compiling, so it cannot reach the wire without someone standing here deciding what
    /// its status and token are. The previous version of these tests was a hand-picked list and
    /// silently omitted the three `403`s the console branches on.
    fn every_variant() -> Vec<ApiError> {
        let all = vec![
            ApiError::NotFound,
            ApiError::Conflict("dup".to_owned()),
            ApiError::Unauthorized,
            ApiError::Forbidden,
            ApiError::EmailNotVerified,
            ApiError::Suspended,
            ApiError::StepUpRequired,
            ApiError::MfaEnrolmentRequired,
            ApiError::FeatureDisabled(Feature::AccountsMfa),
            ApiError::BadRequest("bad".to_owned()),
            ApiError::Unavailable,
            ApiError::BadGateway,
            ApiError::GatewayTimeout,
            ApiError::Internal,
        ];
        for error in &all {
            match error {
                ApiError::NotFound
                | ApiError::Conflict(_)
                | ApiError::Unauthorized
                | ApiError::Forbidden
                | ApiError::EmailNotVerified
                | ApiError::Suspended
                | ApiError::StepUpRequired
                | ApiError::MfaEnrolmentRequired
                | ApiError::FeatureDisabled(_)
                | ApiError::BadRequest(_)
                | ApiError::Unavailable
                | ApiError::BadGateway
                | ApiError::GatewayTimeout
                | ApiError::Internal => {}
            }
        }
        all
    }

    async fn documented_body(response: Response) -> ProblemDetails {
        let bytes = to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("read body");
        // Deserializing into the *documented* type is the assertion: a member renamed or dropped
        // on either side fails here, and now so does a `title` outside the published vocabulary.
        serde_json::from_slice(&bytes).expect("body matches the published schema")
    }

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

        let documented = documented_body(response).await;
        assert_eq!(documented.r#type, "about:blank#not_found");
        assert_eq!(documented.title, ProblemKind::NotFound);
        assert_eq!(documented.status, 404);
        assert_eq!(documented.detail, "resource not found");
    }

    /// Every variant's status must agree with the `status` member in its own body, or a client
    /// that reads one and not the other sees a different error than the one that happened. Every
    /// variant, not a hand-picked list: the previous version of this test omitted the three
    /// `403`s the console branches on, which are the ones a mistake here would hurt most.
    #[tokio::test]
    async fn every_variant_echoes_its_own_status() {
        for error in every_variant() {
            let expected = error.parts().0;
            let response = error.into_response();
            assert_eq!(response.status(), expected);

            let documented = documented_body(response).await;
            assert_eq!(documented.status, expected.as_u16());
            assert_eq!(
                documented.r#type,
                format!("about:blank#{}", documented.title.as_str())
            );
            assert!(expected.is_client_error() || expected.is_server_error());
        }
    }

    /// The vocabulary published in `openapi.json` is closed, so a token emitted on an API route
    /// but missing from it deserialises as nothing at all in the generated client: the caller
    /// gets `InvalidResponsePayload` and the server's message is lost. This is what proves the
    /// list may be closed — it has to name *every* producer, and two of them never build an
    /// `ApiError`.
    #[test]
    fn every_problem_body_an_api_route_can_emit_is_in_the_vocabulary() {
        let published: Vec<&str> = ProblemKind::ALL.iter().map(|k| k.as_str()).collect();

        for error in every_variant() {
            let kind = error.parts().1;
            assert!(
                published.contains(&kind.as_str()),
                "`{}` is emitted but not published",
                kind.as_str()
            );
        }

        // The shared middleware writes its problem body by hand rather than through `ApiError`,
        // so neither of these is reachable from the loop above.
        for kind in [
            tankovault_service::ratelimit::RATE_LIMITED_KIND,
            tankovault_service::flags::FEATURE_DISABLED_KIND,
        ] {
            assert!(
                published.contains(&kind),
                "the shared middleware emits `{kind}`, which the API does not publish"
            );
        }
    }

    /// The token is the contract; `ProblemKind`'s serde encoding and its `as_str` are two ways of
    /// spelling it, and only one of them reaches the wire.
    #[test]
    fn the_serde_encoding_and_the_wire_token_agree() {
        for kind in ProblemKind::ALL {
            assert_eq!(
                serde_json::to_value(kind).expect("kind serializes"),
                serde_json::Value::String(kind.as_str().to_owned())
            );
        }
    }

    /// The internal variant must not put its cause on the wire.
    #[test]
    fn the_internal_variant_says_nothing() {
        let problem = ApiError::Internal.into_problem();
        assert_eq!(problem.detail, "internal server error");
    }
}
