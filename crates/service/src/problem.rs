//! One RFC 9457 `application/problem+json` error shape for every service (ARCH-12).
//!
//! # Why this is shared
//!
//! The eight services used to encode failures four different ways: the API produced
//! problem+json, `sync` a plain-text `Display` of an `anyhow::Error` routed by substring match,
//! `control-plane` bare `(StatusCode, String)` tuples, and `render` / `challenge-solver` inline
//! `format!` strings. `services/api` proxies three of those, so its upstream client had to parse
//! four encodings — which is to say there was no way to write one correct upstream error mapper.
//!
//! Each service still owns its own `thiserror` enum, because which failures carry HTTP meaning is
//! a per-service contract. What is shared is the *wire shape*: a service maps its enum to
//! [`Problem`] via [`IntoProblem`], and the single [`IntoResponse`] implementation below decides
//! how that reaches the network.
//!
//! # What callers can rely on
//!
//! - `Content-Type: application/problem+json`.
//! - `type` is `about:blank#<kind>`, `title` is `<kind>`, `status` matches the HTTP status line,
//!   and `detail` is a human-readable sentence.
//! - [`Problem::kind`] is `&'static str` deliberately: it is a machine-readable discriminator a
//!   client may branch on, so it must come from a fixed vocabulary in the code rather than from a
//!   formatted message that a reworded log line could change.
//!
//! Detail strings are for humans and may be reworded. Do not put anything a caller must parse
//! anywhere but `kind` and `status`.

use axum::Json;
use axum::http::StatusCode;
use axum::http::header::CONTENT_TYPE;
use axum::response::{IntoResponse, Response};
use serde::Serialize;

/// The `application/problem+json` media type. Named here so no service spells it by hand.
pub const PROBLEM_JSON: &str = "application/problem+json";

/// The serialized RFC 9457 body. Built by [`Problem::into_response`]; services do not construct
/// it directly.
///
/// `services/api` re-declares this shape with `utoipa::ToSchema` for its `OpenAPI` document —
/// this crate deliberately does not depend on `utoipa`, since only the documented service needs
/// it. If the two ever disagree, the API's schema is the one that is wrong.
#[derive(Debug, Serialize)]
pub struct ProblemBody {
    pub r#type: String,
    pub title: String,
    pub status: u16,
    pub detail: String,
}

/// A failure, in the form the wire wants it.
#[derive(Debug, Clone)]
pub struct Problem {
    /// The HTTP status. Also echoed in the body's `status` member, per RFC 9457.
    pub status: StatusCode,
    /// Stable machine-readable discriminator, e.g. `"not_found"`. Becomes `title` and the
    /// fragment of `type`.
    pub kind: &'static str,
    /// Human-readable sentence. Never include internal detail here — see [`Problem::internal`].
    pub detail: String,
}

impl Problem {
    /// A problem with an explicit status, kind and detail.
    #[must_use]
    pub fn new(status: StatusCode, kind: &'static str, detail: impl Into<String>) -> Self {
        Self {
            status,
            kind,
            detail: detail.into(),
        }
    }

    /// A `500` that says nothing.
    ///
    /// The caller is expected to have logged the cause already. Internal failures must not put
    /// their `Display` on the wire: it routinely carries connection strings, SQL and file paths,
    /// and a caller can do nothing useful with any of it.
    #[must_use]
    pub fn internal() -> Self {
        Self::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            "internal",
            "internal server error",
        )
    }

    /// A `502` for a dependency that failed.
    #[must_use]
    pub fn bad_gateway() -> Self {
        Self::new(
            StatusCode::BAD_GATEWAY,
            "upstream_unavailable",
            "a service this request depends on is unavailable; please try again",
        )
    }
}

impl IntoResponse for Problem {
    fn into_response(self) -> Response {
        let body = Json(ProblemBody {
            r#type: format!("about:blank#{}", self.kind),
            title: self.kind.to_owned(),
            status: self.status.as_u16(),
            detail: self.detail,
        });
        // `Json` sets `application/json`; RFC 9457 wants `application/problem+json`, and the
        // override has to come after so it wins.
        let mut response = (self.status, body).into_response();
        response.headers_mut().insert(
            CONTENT_TYPE,
            axum::http::HeaderValue::from_static(PROBLEM_JSON),
        );
        response
    }
}

/// A service error that knows its place in the HTTP contract.
///
/// Implement this on the service's own `thiserror` enum rather than implementing `IntoResponse`
/// directly: that keeps the status/kind decision next to the variants (exhaustive by
/// construction, so adding a variant without deciding its status is a compile error) while the
/// wire encoding stays in one place.
pub trait IntoProblem {
    /// The status, kind and human-readable detail this failure presents as.
    fn into_problem(self) -> Problem;
}

impl IntoProblem for Problem {
    fn into_problem(self) -> Problem {
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::to_bytes;

    async fn body_of(problem: Problem) -> (StatusCode, String, serde_json::Value) {
        let response = problem.into_response();
        let status = response.status();
        let content_type = response
            .headers()
            .get(CONTENT_TYPE)
            .expect("content type is set")
            .to_str()
            .expect("content type is ascii")
            .to_owned();
        let bytes = to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("read body");
        (
            status,
            content_type,
            serde_json::from_slice(&bytes).expect("body is json"),
        )
    }

    /// The four members RFC 9457 defines, plus the media type. A client branching on `title` or
    /// `status` is within contract, so this is what must not drift.
    #[tokio::test]
    async fn the_wire_shape_is_rfc_9457() {
        let (status, content_type, json) = body_of(Problem::new(
            StatusCode::NOT_FOUND,
            "not_found",
            "no such series",
        ))
        .await;

        assert_eq!(status, StatusCode::NOT_FOUND);
        assert_eq!(content_type, PROBLEM_JSON);
        assert_eq!(json["type"], "about:blank#not_found");
        assert_eq!(json["title"], "not_found");
        assert_eq!(json["status"], 404);
        assert_eq!(json["detail"], "no such series");
    }

    /// `Json` sets `application/json`, so the override has to be applied *after* it. Getting the
    /// order wrong is invisible in the body and only shows up in the header.
    #[tokio::test]
    async fn the_json_content_type_does_not_win() {
        let (_, content_type, _) = body_of(Problem::internal()).await;
        assert_eq!(content_type, PROBLEM_JSON);
    }

    /// An internal failure says nothing. The cause belongs in the log, not in the response:
    /// `Display` on a database or IO error routinely carries connection strings and paths.
    #[tokio::test]
    async fn an_internal_problem_leaks_nothing() {
        let (status, _, json) = body_of(Problem::internal()).await;
        assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
        assert_eq!(json["detail"], "internal server error");
    }
}
