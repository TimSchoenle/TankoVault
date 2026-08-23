//! One typed client for every internal service the API fronts.
//!
//! Centralizes the internal-token header, timeouts and upstream-status mapping that call
//! sites used to duplicate, each free to get the error mapping subtly different.

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
    /// `material` is present under `internal.identity = "mtls"`, and then the client both
    /// presents this service's certificate and verifies its peers against the configured
    /// bundle — `.tls_built_in_root_certs(false)` because a peer signed by a *public* authority
    /// is not a peer, and accepting one would make the whole point of pinning to an internal CA
    /// moot.
    ///
    /// # Errors
    /// When the TLS backend cannot be initialised, or the supplied material is not valid PEM.
    pub fn client(
        material: Option<&tankovault_service::ClientMaterial>,
    ) -> Result<reqwest::Client, reqwest::Error> {
        let builder = reqwest::Client::builder()
            .connect_timeout(CONNECT_TIMEOUT)
            .timeout(REQUEST_TIMEOUT);

        let Some(material) = material else {
            return builder.build();
        };

        // reqwest wants the chain and the key in one PEM blob.
        let mut identity = material.cert.clone();
        identity.extend_from_slice(&material.key);

        builder
            .identity(reqwest::Identity::from_pem(&identity)?)
            // `tls_certs_only`, not `add_root_certificate`: it also drops the built-in roots, and
            // a peer signed by a *public* authority is not a peer. Merely adding the internal CA
            // alongside the public set would leave every WebPKI-signed host acceptable, which is
            // most of the value of pinning to an internal CA gone.
            .tls_certs_only([reqwest::Certificate::from_pem(&material.ca)?])
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
    /// goes through [`Self::prepare`], which is the point.
    pub fn request(&self, method: reqwest::Method, path: &str) -> reqwest::RequestBuilder {
        self.prepare(self.http.request(method, self.url(path)))
    }

    /// The peer's name, for the caller's own log lines.
    #[must_use]
    pub fn name(&self) -> &'static str {
        self.name
    }

    /// `path` must never be a raw client-supplied string: a `/`, `..`, `?` or `#` in it moves
    /// the request to a different endpoint on this internal peer, one that still gets
    /// `X-Internal-Token`. Path segments from a request are validated by their type before
    /// they reach here; see [`crate::slug`].
    fn url(&self, path: &str) -> String {
        format!("{}/{}", self.base, path.trim_start_matches('/'))
    }

    /// Attach the internal token, if one is configured, and the trace this call belongs to.
    ///
    /// Every outbound path goes through here — [`Self::send`] and [`Self::request`] both — so
    /// it is the one place either can be forgotten.
    fn prepare(&self, req: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
        let mut req = match &self.token {
            Some(token) => req.header(INTERNAL_TOKEN_HEADER, token.expose_secret()),
            None => req,
        };
        // Continues this request's trace on the peer, so one user action reads as one trace
        // across the tier instead of four unrelated ones. Empty unless Sentry is configured.
        for (name, value) in tankovault_service::trace_headers() {
            req = req.header(name, value);
        }
        req
    }

    /// Send, then translate the outcome into this service's error vocabulary.
    ///
    /// # Errors
    /// [`ApiError::BadGateway`] or [`ApiError::GatewayTimeout`] on transport failure; [`ApiError::NotFound`] or [`ApiError::Conflict`] when the peer said so deliberately.
    async fn send<T: DeserializeOwned>(&self, req: reqwest::RequestBuilder) -> ApiResult<Json<T>> {
        let resp = self.prepare(req).send().await.map_err(|e| {
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
    /// `404`/`409` are forwarded since internal services emit them deliberately and the
    /// public contract documents both. `401` is not forwarded — it means this service failed
    /// to authenticate to its own peer, an operator misconfiguration, not the client's fault.
    fn map_status(&self, status: reqwest::StatusCode, body: &str) -> ApiError {
        match status.as_u16() {
            404 => ApiError::NotFound,
            409 => ApiError::Conflict("Account not linked".to_owned()),
            // `401` and `403` mean different misconfigurations now that callers are named:
            // the peer either did not recognise this service, or recognised it and refuses it
            // this route. Neither is the client's fault, so both surface as `502`.
            401 => {
                tracing::error!(
                    upstream = self.name,
                    %status,
                    "internal call was not recognised: does this deployment's \
                     internal.caller.token match the peer's internal.peers.api.token (or, under \
                     mtls, does its certificate SAN match internal.peers.api.san)?"
                );
                ApiError::BadGateway
            }
            403 => {
                tracing::error!(
                    upstream = self.name,
                    %status,
                    "internal call was recognised but refused this route: `api` is missing from \
                     the peer's route table for it"
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

    /// The endpoint's declared body is enforced, not merely documented.
    ///
    /// Proxy handlers used to return `Json<serde_json::Value>` while their `#[utoipa::path]`
    /// named a concrete body; nothing connected the two. A peer whose shape has drifted must
    /// now fail at this boundary instead of reaching a client compiled against the old one.
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

    /// A `204` from the peer is a success with nothing to forward, not an absent body the
    /// client should see, so the empty-body case must produce the canonical acknowledgement
    /// rather than `null`.
    ///
    /// The second half is what makes [`tankovault_contracts::sync::Ack`] honest. Two routes
    /// (`link`, `patch_settings`) answer `204` upstream and are published as `200 {"ok": true}`
    /// purely because of the synthesis below; before `Ack` existed, that coupling lived in a
    /// hand-written `json!` literal in the handler and a `serde_json::Value` return type, so
    /// changing the synthesis would have silently changed what the SPA received. If these two
    /// ever disagree, those routes break.
    #[test]
    fn an_empty_body_is_still_the_canonical_acknowledgement() {
        let up = upstream(None);
        let Json(value) = up
            .decode::<serde_json::Value>("")
            .expect("an empty body is not an error");
        assert_eq!(value, serde_json::json!({ "ok": true }));

        let Json(ack) = up
            .decode::<tankovault_contracts::sync::Ack>("")
            .expect("the synthesised body is an Ack");
        assert!(ack.ok, "Ack is what the synthesised acknowledgement means");
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
