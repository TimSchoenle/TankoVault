//! The router: the shell, hashed assets, the licence notices, health, and the `/v1/*` proxy.

use std::convert::Infallible;
use std::future::{Ready, ready};
use std::path::Path;
use std::task::{Context, Poll};

use axum::Router;
use axum::body::Bytes;
use axum::extract::Request;
use axum::http::{HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{any, get};
use tankovault_config::BrandingConfig;
use tankovault_frontend::config::FrontendConfig;
use tower::{Service, ServiceBuilder};
use tower_http::services::{ServeDir, ServeFile};
use tower_http::set_header::SetResponseHeaderLayer;

use super::{
    AppState,
    csp::{CspHeader, content_security_policy},
    proxy::proxy,
    shell::{brand_shell, read_shell, shell_path},
};

/// The subdirectory `dx` writes every content-hashed file into.
///
/// Content-hashed means a changed file is a changed URL, which is what makes a one-year
/// `immutable` lifetime safe here and nowhere else in the bundle.
const HASHED_ASSETS: &str = "/assets";

/// A response held in memory, carrying its own `Cache-Control`.
///
/// The bundle's blanket `Cache-Control` layer is `if_not_present`, so stating the value here is
/// what keeps that default off these responses.
#[derive(Clone)]
struct FixedResponse {
    status: StatusCode,
    content_type: &'static str,
    cache_control: &'static str,
    body: Bytes,
}

impl FixedResponse {
    /// The app shell, from the bytes [`read_shell`] took at startup.
    ///
    /// **`no-store` rather than `no-cache`, and no `Last-Modified` or `ETag`.** All three are one
    /// requirement: this document must never be reconstructed from a stored copy. It carries a
    /// per-response CSP nonce and the inline-script hashes of exactly one build, so a body paired
    /// with any other response's header is a document that response's policy refuses.
    ///
    /// `no-cache` does not prevent that — it means *revalidate*, and a successful revalidation
    /// re-uses the stored body under the new response's headers. Served from disk it did exactly
    /// that: image timestamps are clamped to `SOURCE_DATE_EPOCH`, which this repository pins to a
    /// constant `0` on purpose (see `the_build_epoch_is_one_constant` in `xtask`), so every build
    /// answered `Last-Modified: Thu, 01 Jan 1970 00:00:00 GMT` and every conditional request was
    /// a `304` forever. Serving from memory with no validator is what makes the `304` impossible
    /// rather than merely unlikely.
    fn shell(body: Bytes) -> Self {
        Self {
            status: StatusCode::OK,
            content_type: "text/html; charset=utf-8",
            cache_control: "no-store",
            body,
        }
    }

    /// The shell could not be read at startup, so there is no SPA to serve.
    ///
    /// A `503` rather than the `404` a missing file used to produce: the bundle is broken, which
    /// is an operator's problem and not a wrong URL.
    fn unavailable() -> Self {
        Self {
            status: StatusCode::SERVICE_UNAVAILABLE,
            content_type: "text/plain; charset=utf-8",
            cache_control: "no-store",
            body: Bytes::from_static(b"app shell unavailable\n"),
        }
    }

    /// A bundle file that is not there.
    ///
    /// A `404`, never the shell. Answering a `<script type="module">` request with HTML fails in
    /// the browser on the MIME type instead, which reads as a bundler or CSP fault rather than
    /// the missing file it is — the same reasoning that makes the notices a route of their own.
    ///
    /// `no-store` because a rolling deploy legitimately serves this: a client that already has
    /// the new shell asks a replica that has not rolled yet for a file only the new image has.
    /// Caching that answer would outlive the rollout by a year.
    fn missing() -> Self {
        Self {
            status: StatusCode::NOT_FOUND,
            content_type: "text/plain; charset=utf-8",
            cache_control: "no-store",
            body: Bytes::from_static(b"not found\n"),
        }
    }
}

impl Service<Request> for FixedResponse {
    type Response = Response;
    type Error = Infallible;
    type Future = Ready<Result<Response, Infallible>>;

    fn poll_ready(&mut self, _cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Poll::Ready(Ok(()))
    }

    fn call(&mut self, _request: Request) -> Self::Future {
        ready(Ok((
            self.status,
            [
                (
                    header::CONTENT_TYPE,
                    HeaderValue::from_static(self.content_type),
                ),
                (
                    header::CACHE_CONTROL,
                    HeaderValue::from_static(self.cache_control),
                ),
            ],
            self.body.clone(),
        )
            .into_response()))
    }
}

/// Where the SPA links its third-party licence notices, and the only URL they are published at.
///
/// `web/frontend/src/components/nav.rs` repeats this literal — it is a separate workspace, so
/// there is no compile-time relationship between the two — and `xtask repo-lint` is what holds
/// them equal.
pub(super) const NOTICES_ROUTE: &str = "/third-party-notices";

/// Where the same notices are published as the structured inventory the SPA's `/licenses` screen
/// fetches.
///
/// `web/frontend/src/views/licenses.rs` repeats this literal for the same reason as
/// [`NOTICES_ROUTE`], and `xtask repo-lint` holds this pair equal too.
pub(super) const NOTICES_JSON_ROUTE: &str = "/third-party-notices.json";

/// Assemble the router: the health probe, the licence notices, the `/v1/*` proxy, and the
/// static bundle (with SPA fallback and hardening headers) catching everything else.
pub(super) fn build_router(
    frontend: &FrontendConfig,
    branding: &BrandingConfig,
    state: AppState,
) -> Router {
    let static_dir = frontend.static_dir.as_str();
    let shell_path = shell_path(static_dir);
    let source = read_shell(&shell_path).map(|source| brand_shell(&source, branding));

    // Assembled once from the shell read above, since the hashes cover a file that can't change
    // without a redeploy; only the optional nonce is per response.
    let csp = CspHeader::new(content_security_policy(
        source.as_deref(),
        &shell_path,
        &frontend.cloudflare,
    ));

    // The same bytes the policy was derived from. Serving the file instead let a stored copy of
    // one build meet the header of another; see [`FixedResponse::shell`].
    let shell = source.map_or_else(FixedResponse::unavailable, |source| {
        FixedResponse::shell(Bytes::from(source.into_bytes()))
    });

    // `/assets/…` is content-hashed, so a changed file is a changed URL and a year is safe. A
    // miss is a `404` rather than the shell — see [`FixedResponse::missing`]. `if_not_present`
    // rather than `overriding`, so that `404` keeps its own `no-store`.
    let assets = ServiceBuilder::new()
        .layer(SetResponseHeaderLayer::if_not_present(
            header::CACHE_CONTROL,
            HeaderValue::from_static("public, max-age=31536000, immutable"),
        ))
        .service(
            ServeDir::new(Path::new(static_dir).join(HASHED_ASSETS.trim_start_matches('/')))
                .fallback(FixedResponse::missing()),
        );

    // `/` and `/index.html` are routed explicitly so neither ever reaches `ServeDir` and the file
    // on disk: the shell has exactly one representation, and it is the in-memory one. Everything
    // else is a real file if the bundle has one, and a client-side route (`/series/…`,
    // `/account/…`) otherwise, which is what makes a hard refresh or a deep link work.
    let bundle = Router::new()
        .route_service("/", shell.clone())
        .route_service("/index.html", shell.clone())
        .nest_service(HASHED_ASSETS, assets)
        .fallback_service(ServeDir::new(static_dir).fallback(shell));

    // Baseline hardening on the app shell only: `if_not_present` never clobbers a value a
    // served file already carries, and `/v1/*` responses keep the API's own headers untouched.
    let static_service = ServiceBuilder::new()
        .layer(SetResponseHeaderLayer::if_not_present(
            header::X_CONTENT_TYPE_OPTIONS,
            HeaderValue::from_static("nosniff"),
        ))
        .layer(SetResponseHeaderLayer::if_not_present(
            header::REFERRER_POLICY,
            HeaderValue::from_static("no-referrer"),
        ))
        .layer(SetResponseHeaderLayer::if_not_present(
            header::X_FRAME_OPTIONS,
            HeaderValue::from_static("DENY"),
        ))
        .layer(SetResponseHeaderLayer::if_not_present(
            header::CONTENT_SECURITY_POLICY,
            csp,
        ))
        // The default for anything in the bundle that states nothing of its own: revalidate.
        // The two that do state their own are the ones that matter — the shell is `no-store`
        // ([`FixedResponse::shell`]) and `/assets/…` is immutable — and `if_not_present` is what
        // leaves both alone. This layer previously read `no-cache` and applied it to *everything*
        // under a comment claiming hashed assets were cached immutably; nothing set that, and
        // every asset paid a revalidation round-trip per load.
        .layer(SetResponseHeaderLayer::if_not_present(
            header::CACHE_CONTROL,
            HeaderValue::from_static("no-cache"),
        ))
        .service(bundle);
    // Compression isn't layered here separately: `HttpStack`'s single `CompressionLayer`
    // already covers the WASM bundle and proxied JSON, and skips a response already carrying
    // `Content-Encoding` or `text/event-stream` — which is what keeps SSE flushing frame by
    // frame.

    // A route of its own rather than a file dropped into the bundle, for two reasons that both
    // end in a reader not seeing the notices: an extensionless file is served
    // `application/octet-stream`, which downloads rather than displays under `nosniff`; and any
    // path the bundle does not contain resolves to the app shell, so a typo'd or renamed file
    // would answer 200 with the SPA and nothing would look wrong.
    let notices = ServiceBuilder::new()
        .layer(SetResponseHeaderLayer::overriding(
            header::CONTENT_TYPE,
            HeaderValue::from_static("text/plain; charset=utf-8"),
        ))
        .layer(SetResponseHeaderLayer::if_not_present(
            header::X_CONTENT_TYPE_OPTIONS,
            HeaderValue::from_static("nosniff"),
        ))
        .service(ServeFile::new(&frontend.notices_path));

    // The same document, structured, for the screen that renders it. A route of its own for the
    // second of the two reasons above: `.json` would sniff correctly, but an absent file inside
    // the bundle would still resolve to the app shell, and the screen would then be parsing HTML
    // and reporting a corrupt inventory instead of a missing one. `ServeFile` answers 404, which
    // is what the screen's unavailable state is keyed on.
    //
    // No `Cache-Control` of its own: the inventory changes only when the image does, but it is
    // served from the same origin as a shell that is `no-store`, and a stale inventory behind a
    // fresh bundle is exactly the drift the document exists to rule out.
    let notices_json = ServiceBuilder::new()
        .layer(SetResponseHeaderLayer::overriding(
            header::CONTENT_TYPE,
            HeaderValue::from_static("application/json; charset=utf-8"),
        ))
        .layer(SetResponseHeaderLayer::if_not_present(
            header::X_CONTENT_TYPE_OPTIONS,
            HeaderValue::from_static("nosniff"),
        ))
        .service(ServeFile::new(&frontend.notices_json_path));

    Router::new()
        .route("/healthz", get(healthz))
        .route_service(NOTICES_ROUTE, notices)
        .route_service(NOTICES_JSON_ROUTE, notices_json)
        .route("/v1/{*rest}", any(proxy))
        .fallback_service(static_service)
        .with_state(state)
}

/// Legacy liveness alias.
///
/// The real probe is `GET /health` from [`tankovault_service::ops_router`]. Kept only because
/// the retired nginx image published this path and an external monitor may still poll it.
pub(super) async fn healthz() -> impl IntoResponse {
    ([(header::CONTENT_TYPE, "text/plain")], "ok\n")
}
