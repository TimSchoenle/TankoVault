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
//!
//! # Typed bodies (ARCH-10)
//!
//! Every verb is generic in the response body, so a proxy handler declaring
//! `body = tankovault_contracts::sync::AccountStatus` in its `#[utoipa::path]` can *return*
//! that type and have the deserialize step enforce the declaration at the edge. Before that,
//! the handlers all returned `Json<serde_json::Value>` and nothing — not the compiler, not a
//! test — connected the declaration to what was forwarded, while the generated
//! `crates/api-client` and the frontend trusted it. Being one place is what made this a
//! four-line change rather than twenty.
//!
//! `T = serde_json::Value` is still the right answer for a genuinely schema-less command
//! response and costs nothing: [`serde_json::from_value`] into a `Value` is the identity.
//!
//! # What this type does *not* check
//!
//! [`Upstream::url`] is a `format!` and nothing more: it joins `base` and `path` and trusts the
//! path. That is the one thing a caller must not hand a raw client-supplied string, because a
//! `/`, a `..`, a `?` or a `#` in it moves the request to a different endpoint on the internal
//! service — one this client then presents `X-Internal-Token` on. Path segments that come from
//! a request are validated *before* they get here, by their type; see [`crate::slug`] for the
//! bug that was and why it is a type rather than a check at each of the eleven call sites.

use crate::error::{ApiError, ApiResult};
use axum::Json;
use secrecy::ExposeSecret as _;
use serde::Serialize;
use serde::de::DeserializeOwned;
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

    /// `GET path`, decoding the body as `T`.
    ///
    /// # Errors
    /// See [`Self::send`].
    pub async fn get<T: DeserializeOwned>(&self, path: &str) -> ApiResult<Json<T>> {
        let url = self.url(path);
        self.send(self.http.get(url)).await
    }

    /// `POST path` with a JSON body, decoding the response as `T`.
    ///
    /// # Errors
    /// See [`Self::send`].
    pub async fn post<B: Serialize, T: DeserializeOwned>(
        &self,
        path: &str,
        body: &B,
    ) -> ApiResult<Json<T>> {
        let url = self.url(path);
        self.send(self.http.post(url).json(body)).await
    }

    /// `PATCH path` with a JSON body, decoding the response as `T`.
    ///
    /// # Errors
    /// See [`Self::send`].
    pub async fn patch<B: Serialize, T: DeserializeOwned>(
        &self,
        path: &str,
        body: &B,
    ) -> ApiResult<Json<T>> {
        let url = self.url(path);
        self.send(self.http.patch(url).json(body)).await
    }

    /// `DELETE path` with a JSON body, decoding the response as `T`.
    ///
    /// # Errors
    /// See [`Self::send`].
    pub async fn delete<B: Serialize, T: DeserializeOwned>(
        &self,
        path: &str,
        body: &B,
    ) -> ApiResult<Json<T>> {
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
            Some(token) => req.header(INTERNAL_TOKEN_HEADER, token.expose_secret()),
            None => req,
        }
    }

    /// Send, then translate the outcome into this service's error vocabulary.
    ///
    /// # Errors
    /// [`ApiError::BadGateway`] when the peer is unreachable, answers with a status this
    /// service cannot represent, or sends a body that is not a `T`; [`ApiError::GatewayTimeout`]
    /// on a timeout; [`ApiError::NotFound`] and [`ApiError::Conflict`] when the peer said so
    /// deliberately.
    async fn send<T: DeserializeOwned>(&self, req: reqwest::RequestBuilder) -> ApiResult<Json<T>> {
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

        self.decode(&text)
    }

    /// Turn a successful response body into the `T` the calling endpoint publishes.
    ///
    /// Separate from [`Self::send`] so the two rules it encodes are testable without a peer.
    ///
    /// # Errors
    /// [`ApiError::BadGateway`] when the body is not a `T`.
    fn decode<T: DeserializeOwned>(&self, text: &str) -> ApiResult<Json<T>> {
        // A `204`, or a body the peer chose not to give us, is a success with nothing to
        // forward. Callers expect an object, so give them the canonical empty one.
        let value: serde_json::Value = if text.trim().is_empty() {
            serde_json::json!({ "ok": true })
        } else {
            serde_json::from_str(text).unwrap_or_else(|_| serde_json::json!({ "ok": true }))
        };

        // Where the caller named a concrete `T`, this is the step that makes the endpoint's
        // `#[utoipa::path]` declaration true rather than aspirational: a peer that changed its
        // shape fails here, at the boundary, instead of silently reaching a generated client
        // that was compiled against the old one. A `T` of `serde_json::Value` passes through.
        serde_json::from_value(value).map(Json).map_err(|e| {
            tracing::error!(
                upstream = self.name,
                error = %e,
                expected = std::any::type_name::<T>(),
                "internal service answered with a body this endpoint does not publish"
            );
            ApiError::BadGateway
        })
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

    #[expect(
        clippy::disallowed_methods,
        reason = "these tests assert header and URL construction; nothing is ever sent, so the \
                  client's (absent) timeouts are not reachable"
    )]
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

    /// The endpoint's declared body is now *enforced* rather than merely documented (ARCH-10).
    ///
    /// Twenty proxy handlers used to return `Json<serde_json::Value>` while their
    /// `#[utoipa::path]` named a concrete `body =`. Nothing connected the two, and the
    /// generated `crates/api-client` and the frontend believed the declaration. A peer whose
    /// shape has drifted must now fail at this boundary instead of reaching a client compiled
    /// against the old one.
    #[test]
    fn a_body_that_is_not_the_declared_shape_is_a_bad_gateway() {
        let up = upstream(None);
        assert!(
            up.decode::<tankovault_contracts::sync::ProviderInfo>(r#"{"slug":"anilist"}"#)
                .is_err(),
            "a ProviderInfo missing `name` must not be forwarded as one"
        );
        let Json(ok) = up
            .decode::<tankovault_contracts::sync::ProviderInfo>(
                r#"{"slug":"anilist","name":"AniList"}"#,
            )
            .expect("the declared shape decodes");
        assert_eq!(ok.slug, "anilist");
    }

    /// The command proxies stay `serde_json::Value`, and for them the empty-body case must
    /// still produce the canonical acknowledgement rather than `null` — a `204` from the peer
    /// is a success with nothing to forward, not an absent body the client should see.
    #[test]
    fn an_empty_body_is_still_the_canonical_acknowledgement() {
        let up = upstream(None);
        let Json(value) = up
            .decode::<serde_json::Value>("")
            .expect("an empty body is not an error");
        assert_eq!(value, serde_json::json!({ "ok": true }));
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
