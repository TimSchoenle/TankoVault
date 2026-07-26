//! Typed API error → RFC 9457 problem+json. Internal errors never leak details.

use axum::Json;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde::Serialize;
use tankovault_auth::AuthError;
use tankovault_db::DbError;
use utoipa::ToSchema;

/// RFC 9457 `application/problem+json` error body shape produced by [`ApiError`]. Declared
/// purely for `OpenAPI` documentation — [`ApiError::into_response`] builds the JSON by hand so
/// runtime callers never construct this type.
#[derive(Serialize, ToSchema)]
#[schema(example = json!({
    "type": "about:blank#not_found",
    "title": "not_found",
    "status": 404,
    "detail": "resource not found",
}))]
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
    /// Login attempted before the account's email address was confirmed. Distinct from
    /// [`Self::Forbidden`] so the frontend can recognise it and offer to resend the link.
    #[error("email not verified")]
    EmailNotVerified,
    /// The account has been suspended by an administrator. Distinct from [`Self::Forbidden`],
    /// which means "you may not do *this*": suspension means the account may not act at all,
    /// and telling the user "insufficient privileges" would send them looking for a permission
    /// they can never be granted while suspended.
    #[error("account suspended")]
    Suspended,
    /// The requested capability exists in the code but is switched off for this deployment.
    ///
    /// `404`, matching `tankovault_service::flags::enforce`: the resource genuinely is not part
    /// of this deployment's API. Handlers use this for the cases the route-level middleware
    /// cannot express — a request body that asks for a disabled *mode* rather than a disabled
    /// path, such as a full scan when only fast scans are enabled.
    #[error("feature disabled")]
    FeatureDisabled(tankovault_domain::Feature),
    #[error("{0}")]
    BadRequest(String),
    #[error("service unavailable")]
    Unavailable,
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
            Self::Internal => (
                StatusCode::INTERNAL_SERVER_ERROR,
                "internal",
                "internal server error".into(),
            ),
        }
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let (status, kind, detail) = self.parts();
        let body = Json(ProblemDetails {
            r#type: format!("about:blank#{kind}"),
            title: kind.to_string(),
            status: status.as_u16(),
            detail,
        });
        (status, body).into_response()
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
