//! Typed API error → RFC 9457 problem+json. Internal errors never leak details.

use axum::Json;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use tankovault_auth::AuthError;
use tankovault_db::DbError;
use serde_json::json;

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
        let body = Json(json!({
            "type": format!("about:blank#{kind}"),
            "title": kind,
            "status": status.as_u16(),
            "detail": detail,
        }));
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
