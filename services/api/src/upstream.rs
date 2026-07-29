//! One typed client for every internal service the API fronts.
//!
//! The API proxies to `sync`, `control-plane` and `challenge-solver`. That block used to be
//! open-coded at nine call sites, each repeating the same `format!` for the URL, the same
//! `map_err(|_| ApiError::Internal)`, and — crucially — each free to get the error mapping
//! subtly different. Three consequences of that, all fixed here:
//!
//! - **Upstream failures collapsed to `500`.** A `sync` outage was indistinguishable from a
//!   bug in this process. [`Upstream`] maps transport failure to `502` and keeps upstream
//!   `404`/`409` intact, so the `409` `OpenAPI` documents is now actually emittable.
//! - **The internal token had nowhere to live.** Every outbound call must present
//!   `X-Internal-Token`; a per-call-site convention would be forgotten exactly once, which is
//!   all it takes. Attaching it here makes it structural.
//! - **No timeouts.** The client was a bare `reqwest::Client::new()`, which has none, feeding
//!   an unbounded `tokio::spawn` in `spawn_targeted_push` — a hung `sync` leaked a task and a
//!   socket per marked chapter.

use crate::error::{ApiError, ApiResult};
use axum::Json;
use serde::Serialize;
use std::time::Duration;
use tankovault_service::{INTERNAL_TOKEN_HEADER, InternalToken};

/// Connect timeout for internal calls. These are same-network hops; a connect that has not
/// completed in this long is a dead peer, not a slow one.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);

/// Whole-request timeout. Above the slowest legitimate operation (a `sync` pull walks a
/// third-party list) and below the API's own 30 s request timeout, so the caller sees this
/// service's `504` rather than an ambiguous edge timeout.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(25);

/// A typed handle to one internal service.
///
/// Cheap to clone: the inner `reqwest::Client` is an `Arc` and shares the connection pool.
#[derive(Clone, Debug)]
pub struct Upstream {
    http: reqwest::Client,
    /// Base URL with no trailing slash, so path joining is a plain `format!`.
    base: String,
    /// Presented on every request. `None` when the deployment has not configured one, which
    /// `tankovault_config::InternalAuthConfig` permits outside the production profile.
    token: Option<InternalToken>,
    /// Names the peer in log lines and in the `502` reason. Not sent on the wire.
    name: &'static str,
}

impl Upstream {
    /// Build a handle to `base`, presenting `token` on every call.
    ///
    /// # Errors
    /// When the shared `reqwest::Client` cannot be constructed (a TLS backend failure).
    pub fn new(
        http: reqwest::Client,
        base: impl Into<String>,
        token: Option<InternalToken>,
        name: &'static str,
    ) -> Self {
        let base = base.into();
        Self {
            http,
            base: base.trim_end_matches('/').to_owned(),
            token,
            name,
        }
    }

    /// The shared client every [`Upstream`] should be built from.
    ///
    /// # Errors
    /// When the TLS backend cannot be initialised.
    pub fn client() -> Result<reqwest::Client, reqwest::Error> {
        reqwest::Client::builder()
            .connect_timeout(CONNECT_TIMEOUT)
            .timeout(REQUEST_TIMEOUT)
            .build()
    }

    /// `GET path`, decoding a JSON body.
    ///
    /// # Errors
    /// See [`Self::send`].
    pub async fn get(&self, path: &str) -> ApiResult<Json<serde_json::Value>> {
        let url = self.url(path);
        self.send(self.http.get(url)).await
    }

    /// `POST path` with a JSON body.
    ///
    /// # Errors
    /// See [`Self::send`].
    pub async fn post<B: Serialize>(
        &self,
        path: &str,
        body: &B,
    ) -> ApiResult<Json<serde_json::Value>> {
        let url = self.url(path);
        self.send(self.http.post(url).json(body)).await
    }

    /// `PATCH path` with a JSON body.
    ///
    /// # Errors
    /// See [`Self::send`].
    pub async fn patch<B: Serialize>(
        &self,
        path: &str,
        body: &B,
    ) -> ApiResult<Json<serde_json::Value>> {
        let url = self.url(path);
        self.send(self.http.patch(url).json(body)).await
    }

    /// `DELETE path` with a JSON body.
    ///
    /// # Errors
    /// See [`Self::send`].
    pub async fn delete<B: Serialize>(
        &self,
        path: &str,
        body: &B,
    ) -> ApiResult<Json<serde_json::Value>> {
        let url = self.url(path);
        self.send(self.http.delete(url).json(body)).await
    }

    /// Fire a request whose outcome the caller does not await — the targeted sync push.
    ///
    /// Returns the builder rather than sending, so the caller owns the `tokio::spawn`. Still
    /// goes through [`Self::authenticate`], which is the point.
    pub fn request(&self, method: reqwest::Method, path: &str) -> reqwest::RequestBuilder {
        self.authenticate(self.http.request(method, self.url(path)))
    }

    /// The peer's name, for the caller's own log lines.
    #[must_use]
    pub fn name(&self) -> &'static str {
        self.name
    }

    fn url(&self, path: &str) -> String {
        format!("{}/{}", self.base, path.trim_start_matches('/'))
    }

    /// Attach the internal token, if one is configured.
    fn authenticate(&self, req: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
        match &self.token {
            Some(token) => req.header(INTERNAL_TOKEN_HEADER, token.expose()),
            None => req,
        }
    }

    /// Send, then translate the outcome into this service's error vocabulary.
    ///
    /// # Errors
    /// [`ApiError::BadGateway`] when the peer is unreachable or answers with a status this
    /// service cannot represent, [`ApiError::GatewayTimeout`] on a timeout,
    /// [`ApiError::NotFound`] and [`ApiError::Conflict`] when the peer said so deliberately.
    async fn send(&self, req: reqwest::RequestBuilder) -> ApiResult<Json<serde_json::Value>> {
        let resp = self.authenticate(req).send().await.map_err(|e| {
            tracing::error!(upstream = self.name, error = %e, "internal service unreachable");
            if e.is_timeout() {
                ApiError::GatewayTimeout
            } else {
                ApiError::BadGateway
            }
        })?;

        let status = resp.status();
        let text = resp.text().await.unwrap_or_default();
        if !status.is_success() {
            return Err(self.map_status(status, &text));
        }

        // A `204`, or a body the peer chose not to give us, is a success with nothing to
        // forward. Callers expect an object, so give them the canonical empty one.
        let value = if text.trim().is_empty() {
            serde_json::json!({ "ok": true })
        } else {
            serde_json::from_str(&text).unwrap_or_else(|_| serde_json::json!({ "ok": true }))
        };
        Ok(Json(value))
    }

    /// Map an upstream status onto this service's error vocabulary.
    ///
    /// `404` and `409` are forwarded because the internal services emit them deliberately
    /// (unknown provider; account not linked) and the public contract documents both.
    /// A `401` is *not* forwarded — it means this service failed to authenticate to its own
    /// peer, which is an operator misconfiguration and must not be reported to the client as
    /// if their credentials were the problem.
    fn map_status(&self, status: reqwest::StatusCode, body: &str) -> ApiError {
        match status.as_u16() {
            404 => ApiError::NotFound,
            409 => ApiError::Conflict("Account not linked".to_owned()),
            401 | 403 => {
                tracing::error!(
                    upstream = self.name,
                    %status,
                    "internal call was refused: is TANKOVAULT_INTERNAL__TOKEN set and identical \
                     on both services?"
                );
                ApiError::BadGateway
            }
            _ => {
                tracing::warn!(upstream = self.name, %status, body = %body, "internal service returned an error");
                ApiError::BadGateway
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn upstream(token: Option<&str>) -> Upstream {
        Upstream::new(
            reqwest::Client::new(),
            "http://sync:8083/",
            token.map(InternalToken::new),
            "sync",
        )
    }

    #[test]
    fn joins_paths_without_doubling_slashes() {
        let up = upstream(None);
        assert_eq!(
            up.url("/v1/sync/providers"),
            "http://sync:8083/v1/sync/providers"
        );
        assert_eq!(
            up.url("v1/sync/providers"),
            "http://sync:8083/v1/sync/providers"
        );
    }

    /// The statuses the internal services emit on purpose must survive the hop; everything
    /// else must not masquerade as a fault in *this* service.
    #[test]
    fn deliberate_upstream_statuses_are_preserved() {
        let up = upstream(None);
        assert!(matches!(
            up.map_status(reqwest::StatusCode::NOT_FOUND, ""),
            ApiError::NotFound
        ));
        assert!(matches!(
            up.map_status(reqwest::StatusCode::CONFLICT, ""),
            ApiError::Conflict(_)
        ));
        assert!(matches!(
            up.map_status(reqwest::StatusCode::INTERNAL_SERVER_ERROR, ""),
            ApiError::BadGateway
        ));
    }

    /// A misconfigured internal token must not surface to the client as *their* 401.
    #[test]
    fn an_upstream_refusal_is_not_reported_as_a_client_auth_failure() {
        let up = upstream(None);
        assert!(matches!(
            up.map_status(reqwest::StatusCode::UNAUTHORIZED, ""),
            ApiError::BadGateway
        ));
        assert!(matches!(
            up.map_status(reqwest::StatusCode::FORBIDDEN, ""),
            ApiError::BadGateway
        ));
    }
}
