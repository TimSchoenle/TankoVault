//! Tests the frontend service over its real router.

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;

use axum::Router;
use axum::extract::Path;
use axum::http::{HeaderMap, HeaderName, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use tankovault_config::BrandingConfig;
use tankovault_frontend::config::{CloudflareConfig, FrontendConfig};
use tankovault_service::MetricsRegistry;
use tokio::net::TcpListener;

use super::csp::{content_security_policy, header_value};
use super::proxy::{set_forwarded_headers, strip_hop_by_hop};
use super::routes::{NOTICES_JSON_ROUTE, NOTICES_ROUTE};
use super::shell::{brand_shell, read_shell};
use super::{AppState, build_app, upstream_health};

/// A stub `api` upstream: `/v1/echo` reflects the forwarded client IP for assertions,
/// `/v1/status/{code}` returns an arbitrary status, `/health` backs the readiness probe.
async fn spawn_stub_upstream() -> SocketAddr {
    async fn echo(headers: HeaderMap) -> Response {
        let xff = headers
            .get("x-forwarded-for")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_owned();
        ([("x-echo-xff", xff.clone())], format!("echo:{xff}")).into_response()
    }
    async fn status(Path(code): Path<u16>) -> Response {
        StatusCode::from_u16(code).unwrap().into_response()
    }

    let app = Router::new()
        .route("/v1/echo", get(echo))
        .route("/v1/status/{code}", get(status))
        .route("/health", get(async || "ok"));
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    addr
}

/// Writes a minimal SPA bundle (the shell, one hashed asset and a stand-in notices file) to a
/// unique temp directory.
fn write_bundle() -> PathBuf {
    static COUNTER: AtomicU32 = AtomicU32::new(0);
    let unique = format!(
        "{}-{}",
        std::process::id(),
        COUNTER.fetch_add(1, Ordering::Relaxed)
    );
    let dir = std::env::temp_dir().join(format!("tankovault-frontend-test-{unique}"));
    std::fs::create_dir_all(dir.join("assets")).unwrap();
    std::fs::write(dir.join("index.html"), SHELL).unwrap();
    std::fs::write(dir.join("app.js"), "APPJS").unwrap();
    std::fs::write(dir.join("assets").join(HASHED_ASSET), "HASHEDJS").unwrap();
    // Beside the bundle here only because a test needs somewhere to put it; in the image it
    // sits at `/THIRD-PARTY-NOTICES`, outside the served directory.
    std::fs::write(dir.join(NOTICES_FILE), NOTICES).unwrap();
    std::fs::write(dir.join(NOTICES_JSON_FILE), NOTICES_JSON).unwrap();
    clamp_timestamps(&dir);
    dir
}

/// Date every file in the fixture `Thu, 01 Jan 1970`, as the image does.
///
/// Not decoration. Image layer timestamps are clamped from `SOURCE_DATE_EPOCH`, which this
/// repository pins to a constant `0`, so this is the shell's real mtime in production, and it
/// is the precondition for the `304` that
/// [`a_conditional_request_for_the_shell_is_never_answered_304`] exists to prevent. A fixture
/// dated *now* cannot reproduce the bug: any conditional request against it is legitimately
/// modified, and the test would pass against the code that shipped it.
fn clamp_timestamps(dir: &std::path::Path) {
    for name in ["index.html", "app.js"] {
        std::fs::File::options()
            .write(true)
            .open(dir.join(name))
            .unwrap()
            .set_modified(std::time::SystemTime::UNIX_EPOCH)
            .unwrap();
    }
}

/// A stand-in for what `dx` writes into `assets/`: a filename carrying its own content hash.
const HASHED_ASSET: &str = "app-dxh0123456789abcd.js";

/// The stand-in notices document and the name [`write_bundle`] writes it under.
const NOTICES: &str = "THIRD-PARTY NOTICES\nApache License 2.0\n";
const NOTICES_FILE: &str = "THIRD-PARTY-NOTICES";

/// The same, as the structured inventory: the shape `xtask notices --json` writes and the
/// SPA's `/licenses` screen parses, cut down to one notice.
const NOTICES_JSON: &str = r#"{"sections":[{"slug":"backend","title":"Part 1","ships_as":"the binaries","licences":[{"id":"Apache-2.0","name":"Apache License 2.0","crates":1,"notices":[{"text":"Apache License 2.0","crates":[{"name":"anyhow","version":"1.0.0"}]}]}]}]}"#;
const NOTICES_JSON_FILE: &str = "THIRD-PARTY-NOTICES.json";

/// The stand-in app shell: an inline boot script (which the CSP has to admit by hash) and
/// an external one (which `'self'` already covers), matching the real shell's shape.
const SHELL: &str = "<html><head><script>window.tv=1;</script>\
                     <script type=\"module\" src=\"/app.js\"></script></head>\
                     <body>INDEX</body></html>";

/// The service configuration these tests run against: the written bundle, its notices file
/// beside it, and a body cap small enough to be reached deliberately.
fn test_config(static_dir: &str) -> FrontendConfig {
    FrontendConfig {
        static_dir: static_dir.to_owned(),
        notices_path: format!("{static_dir}/{NOTICES_FILE}"),
        notices_json_path: format!("{static_dir}/{NOTICES_JSON_FILE}"),
        max_body_bytes: 1024 * 1024,
        ..FrontendConfig::default()
    }
}

/// A client for the tests that send conditional request headers, which `reqwest::get` cannot.
///
/// Built instead of `Client::new()`: `clippy.toml` disallows that constructor for having no
/// timeouts and no pool bound.
fn conditional_client() -> reqwest::Client {
    reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(5))
        .timeout(Duration::from_secs(30))
        .build()
        .unwrap()
}

/// The `Content-Security-Policy` the shell is actually served with.
async fn served_csp(front: SocketAddr) -> String {
    reqwest::get(format!("http://{front}/"))
        .await
        .unwrap()
        .headers()
        .get("content-security-policy")
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default()
        .to_owned()
}

/// The `'sha256-…'` source expression the policy has to carry for an inline `script`.
///
/// Goes through the scanner instead of restating a literal, so an assertion cannot pass
/// against a hash that agrees with the test and with nothing a browser computes.
fn inline_hash(script: &str) -> String {
    let scan = csp_shell::scan_shell(&format!("<script>{script}</script>"));
    scan.hashes
        .first()
        .unwrap_or_else(|| panic!("no inline script found in {script}"))
        .to_string()
}

/// The `script-src` directive of `csp`, without the `;` that closes it.
///
/// Split out because `style-src` legitimately carries `'unsafe-inline'`: an assertion over
/// the whole policy string cannot tell the two apart, and the one that matters is scripts.
fn script_src(csp: &str) -> &str {
    csp.split(';')
        .map(str::trim)
        .find(|directive| directive.starts_with("script-src "))
        .unwrap_or_else(|| panic!("no script-src directive in {csp}"))
}

/// Stands up the real frontend application, the shared stack included, against a stub
/// upstream on an ephemeral port.
///
/// Goes through [`build_app`], not [`build_router`]: the middleware a regression would drop
/// (request id, ops probes) lives in the stack, so testing only the inner router
/// would miss a regression.
async fn spawn_frontend(static_dir: &str, upstream: SocketAddr) -> SocketAddr {
    spawn_frontend_with(test_config(static_dir), upstream).await
}

/// As [`spawn_frontend`], for a caller that needs a configuration of its own.
async fn spawn_frontend_with(frontend: FrontendConfig, upstream: SocketAddr) -> SocketAddr {
    let state = AppState {
        client: reqwest::Client::builder()
            .gzip(false)
            .brotli(false)
            .build()
            .unwrap(),
        upstream: format!("http://{upstream}"),
        max_body_bytes: frontend.max_body_bytes,
    };
    let health = upstream_health(&state);
    let app = build_app(
        &frontend,
        &BrandingConfig::default(),
        state,
        // No recorder: installing the process-wide Prometheus recorder twice fails, and
        // these tests share a process.
        &MetricsRegistry::disabled(),
        health,
    );
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(
            listener,
            app.into_make_service_with_connect_info::<SocketAddr>(),
        )
        .await
        .unwrap();
    });
    addr
}

/// The contract every other service exposes, `/healthz` and `/readyz` both.
#[tokio::test]
async fn health_and_ready_match_the_shared_contract() {
    let upstream = spawn_stub_upstream().await;
    let dir = write_bundle();
    let front = spawn_frontend(dir.to_str().unwrap(), upstream).await;

    let health = reqwest::get(format!("http://{front}/health"))
        .await
        .unwrap();
    assert_eq!(health.status(), StatusCode::OK);

    // Readiness reports the api upstream, which the stub is serving.
    let ready = reqwest::get(format!("http://{front}/ready")).await.unwrap();
    assert_eq!(ready.status(), StatusCode::OK);
    assert!(ready.text().await.unwrap().contains("\"api\""));
}

/// With the upstream gone, readiness must fail while liveness must not: a restart can't
/// bring the API back.
///
/// The dead upstream holds its port and drops every connection. A port bound and released
/// instead could be taken by a parallel test's stub upstream, which would answer, so
/// readiness would pass and the test would fail at random.
#[tokio::test]
async fn readiness_fails_when_the_api_upstream_is_unreachable() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let dead = listener.local_addr().unwrap();
    tokio::spawn(async move {
        while let Ok((connection, _)) = listener.accept().await {
            drop(connection);
        }
    });
    let dir = write_bundle();
    let front = spawn_frontend(dir.to_str().unwrap(), dead).await;

    let ready = reqwest::get(format!("http://{front}/ready")).await.unwrap();
    assert_eq!(ready.status(), StatusCode::SERVICE_UNAVAILABLE);

    let health = reqwest::get(format!("http://{front}/health"))
        .await
        .unwrap();
    assert_eq!(health.status(), StatusCode::OK);
}

/// This tier originates every correlation chain: a bare `TraceLayer` minted no request id,
/// so a frontend-to-api hop couldn't be correlated in logs.
#[tokio::test]
async fn responses_carry_a_request_id() {
    let upstream = spawn_stub_upstream().await;
    let dir = write_bundle();
    let front = spawn_frontend(dir.to_str().unwrap(), upstream).await;

    for path in ["/app.js", "/v1/echo"] {
        let response = reqwest::get(format!("http://{front}{path}")).await.unwrap();
        assert!(
            response.headers().contains_key("x-request-id"),
            "{path} carried no x-request-id"
        );
    }
}

/// The shared stack's security middleware sends the API's `default-src 'none'` CSP, which
/// blocks the WASM bundle; [`stack_security`] turns it off so this tier's own policy
/// survives.
#[tokio::test]
async fn the_spa_keeps_its_own_content_security_policy() {
    let upstream = spawn_stub_upstream().await;
    let dir = write_bundle();
    let front = spawn_frontend(dir.to_str().unwrap(), upstream).await;

    let csp = served_csp(front).await;
    assert!(
        csp.contains("wasm-unsafe-eval"),
        "the API's CSP clobbered the SPA's: {csp}"
    );
    // Nothing here needs eval(); admitting it would hand an injected script the one
    // primitive this policy denies.
    assert!(
        !csp.contains("'unsafe-eval'"),
        "the SPA's CSP re-enabled eval(): {csp}"
    );
}

/// The shell's inline scripts run before the WASM bundle (theme pre-paint, search
/// shortcut); `script-src 'self'` doesn't cover them, so without their hashes the browser
/// refuses both.
#[tokio::test]
async fn the_shells_inline_scripts_are_admitted_by_hash() {
    let upstream = spawn_stub_upstream().await;
    let dir = write_bundle();
    let front = spawn_frontend(dir.to_str().unwrap(), upstream).await;

    let csp = served_csp(front).await;
    assert!(
        csp.contains(&inline_hash("window.tv=1;")),
        "the shell's inline boot script is not admitted: {csp}"
    );
}

/// **The bug this pins.** The shell was served off disk
/// by `ServeFile`. Image timestamps are clamped to `SOURCE_DATE_EPOCH`, which this repository
/// pins to a constant `0` on purpose, so every build answered `Last-Modified: Thu, 01 Jan 1970
/// 00:00:00 GMT`, and `Cache-Control: no-cache` means *revalidate*, not *do not store*. Every
/// conditional request therefore succeeded with a `304`, and the browser re-used the previous
/// build's shell under the current build's headers, indefinitely and across redeploys.
///
/// Both symptoms followed from that one pairing. The stored body's inline scripts were not in
/// the new build's hash list and the nonce Cloudflare had stamped into it belonged to an older
/// response, so every inline script was refused; and the stored body named a content-hashed
/// bundle file the new build does not have, which the SPA fallback answered with the shell,
/// failing in the browser on its MIME type.
#[tokio::test]
async fn a_conditional_request_for_the_shell_is_never_answered_304() {
    let upstream = spawn_stub_upstream().await;
    let dir = write_bundle();
    let front = spawn_frontend(dir.to_str().unwrap(), upstream).await;

    // Both shapes a browser can arrive in. Production sees the first: Cloudflare injects into
    // the HTML, so it strips the `ETag`, and the only validator the browser can store is the
    // `Last-Modified` the fixture reproduces in `clamp_timestamps`.
    let conditional = [
        vec![("if-modified-since", "Thu, 01 Jan 1970 00:00:00 GMT")],
        vec![("if-none-match", "*")],
    ];
    let client = conditional_client();
    for headers in conditional {
        let mut request = client.get(format!("http://{front}/"));
        for (name, value) in &headers {
            request = request.header(*name, *value);
        }
        let response = request.send().await.unwrap();

        assert_eq!(
            response.status(),
            StatusCode::OK,
            "the shell revalidated into a stored body: {headers:?}"
        );
        assert_eq!(response.text().await.unwrap(), SHELL);
    }
}

/// The other half of the same requirement: a document that cannot be stored cannot be
/// revalidated into. `no-store`, not `no-cache`, and no validator to revalidate with:
/// a `Last-Modified` or `ETag` here is what a conditional request would answer `304` against.
#[tokio::test]
async fn the_shell_is_never_stored() {
    let upstream = spawn_stub_upstream().await;
    let dir = write_bundle();
    let front = spawn_frontend(dir.to_str().unwrap(), upstream).await;

    for path in ["/", "/index.html", "/series/abc/deep"] {
        let response = reqwest::get(format!("http://{front}{path}")).await.unwrap();
        let headers = response.headers();
        assert_eq!(headers.get("cache-control").unwrap(), "no-store", "{path}");
        assert!(headers.get("last-modified").is_none(), "{path}");
        assert!(headers.get("etag").is_none(), "{path}");
    }
}

/// The invariant the whole policy rests on: the document the browser receives and the document
/// whose inline scripts the header admits are the same bytes. Two reads of the same path could
/// not guarantee it (the shell is read once and served from memory), so this asserts the
/// property, not the mechanism, by scanning what was actually served.
#[tokio::test]
async fn the_served_shell_is_the_document_the_policy_admits() {
    let upstream = spawn_stub_upstream().await;
    let dir = write_bundle();
    let front = spawn_frontend(dir.to_str().unwrap(), upstream).await;

    let csp = served_csp(front).await;
    let body = reqwest::get(format!("http://{front}/"))
        .await
        .unwrap()
        .text()
        .await
        .unwrap();

    let scan = csp_shell::scan_shell(&body);
    assert!(
        !scan.hashes.is_empty(),
        "no inline script in the served shell"
    );
    for hash in &scan.hashes {
        assert!(
            csp.contains(&hash.to_string()),
            "the served shell carries an inline script the header does not admit: {csp}"
        );
    }
}

/// A bundle file that is not there must be a `404`. Answered with the shell it is a `200` of
/// HTML where the browser expected a module, which fails on the MIME type and reads as a
/// bundler or CSP fault instead of the missing file it is, the same reasoning that makes
/// the licence notices a route of their own.
#[tokio::test]
async fn a_missing_hashed_asset_is_a_404_rather_than_the_app_shell() {
    let upstream = spawn_stub_upstream().await;
    let dir = write_bundle();
    let front = spawn_frontend(dir.to_str().unwrap(), upstream).await;

    let response = reqwest::get(format!("http://{front}/assets/gone-dxhdeadbeef.js"))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
    assert_ne!(response.text().await.unwrap(), SHELL);
}

/// `/assets/…` filenames carry their own content hash, so a changed file is a changed URL:
/// the one thing in the bundle that may be cached for a year. A blanket `no-cache` would cost a
/// revalidation round trip per asset per load.
#[tokio::test]
async fn hashed_assets_are_cached_immutably() {
    let upstream = spawn_stub_upstream().await;
    let dir = write_bundle();
    let front = spawn_frontend(dir.to_str().unwrap(), upstream).await;

    let response = reqwest::get(format!("http://{front}/assets/{HASHED_ASSET}"))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response.headers().get("cache-control").unwrap(),
        "public, max-age=31536000, immutable"
    );
    assert_eq!(response.text().await.unwrap(), "HASHEDJS");
}

/// Pinned against a hash computed outside this codebase: `sha256-bhHHL3z2…` is the value MDN
/// documents for `alert(1)`. The scan itself lives in `csp-shell`, but a browser
/// silently refusing a script it should run stays invisible server-side, so the value that
/// reaches this service's header is pinned here, not only upstream.
#[test]
fn inline_scripts_hash_the_way_a_browser_does() {
    assert_eq!(
        inline_hash("alert(1)"),
        "'sha256-bhHHL3z2vDgxUt0W3dWQOrprscmda2Y5pLsLg4GF+pI='"
    );
}

/// The tab is named before the WASM bundle boots, so the pre-boot name has to be the
/// operator's, and the rewrite must leave the inline scripts byte-identical, or the hashes
/// derived from the same source would stop matching what the browser computes.
#[test]
fn the_served_shell_carries_the_operators_name_and_no_other_change() {
    let source = std::fs::read_to_string("../../web/frontend/index.html")
        .expect("the shell ships in the repo");
    let branding = BrandingConfig {
        name: "MangaBox".to_owned(),
        tagline: Some("everything you read, in one place".to_owned()),
        ..BrandingConfig::default()
    };
    let branded = brand_shell(&source, &branding);

    assert!(
        branded.contains("<title>MangaBox — manga tracker</title>"),
        "{branded}"
    );
    assert!(
        branded.contains(
            r#"<meta name="description" content="MangaBox — everything you read, in one place" />"#
        ),
        "{branded}"
    );
    assert!(!branded.contains("TankoVault"), "{branded}");
    assert_eq!(
        csp_shell::scan_shell(&source).hashes,
        csp_shell::scan_shell(&branded).hashes,
        "branding the shell moved an inline-script hash"
    );
}

/// An operator's name reaches an attribute value; an unescaped quote there is a shell that
/// does not parse, which is a blank page instead of a rebrand.
#[test]
fn a_name_with_markup_characters_cannot_break_the_shell() {
    let source = std::fs::read_to_string("../../web/frontend/index.html")
        .expect("the shell ships in the repo");
    let branding = BrandingConfig {
        name: r#"Ink" & <Vault>"#.to_owned(),
        ..BrandingConfig::default()
    };
    let branded = brand_shell(&source, &branding);
    assert!(
        branded.contains("<title>Ink&quot; &amp; &lt;Vault&gt; — manga tracker</title>"),
        "{branded}"
    );
    assert!(!branded.contains(r#"content="Ink" &"#), "{branded}");
}

/// An unreadable shell must not take the whole tier down: the API proxy and every hashed
/// asset still work, only the inline scripts are refused.
#[test]
fn a_missing_shell_degrades_to_a_hashless_policy() {
    let missing = std::path::Path::new("./no-such-directory/index.html");
    assert!(read_shell(missing).is_none());
    let policy = content_security_policy(None, missing, &CloudflareConfig::default());
    let csp = header_value(&policy);
    let csp = csp.to_str().unwrap();
    assert!(
        csp.contains("script-src 'self' 'wasm-unsafe-eval';"),
        "{csp}"
    );
}

/// Cloudflare's bot products (Bot Fight Mode, JavaScript Detections, the challenge platform)
/// inject an inline `<script>` at the edge, long after this service hashed the shell. The
/// documented way to admit it is a nonce, which Cloudflare copies out of the response
/// header, so a *fresh* one has to reach every response. A constant nonce would pass a
/// naive "contains `nonce-`" assertion while being `'unsafe-inline'` under another name,
/// which is why this asserts the two differ.
#[tokio::test]
async fn the_cloudflare_nonce_is_fresh_on_every_response() {
    let upstream = spawn_stub_upstream().await;
    let dir = write_bundle();
    let front = spawn_frontend_with(
        FrontendConfig {
            cloudflare: CloudflareConfig {
                script_nonce: true,
                turnstile: false,
            },
            ..test_config(dir.to_str().unwrap())
        },
        upstream,
    )
    .await;

    let (first, second) = (served_csp(front).await, served_csp(front).await);
    for csp in [&first, &second] {
        assert!(
            script_src(csp).contains("'nonce-"),
            "no nonce in script-src: {csp}"
        );
        // The nonce is an addition, not a replacement: the shell's own inline scripts are
        // still admitted by hash, so a Cloudflare-less request path is unaffected.
        assert!(
            csp.contains(&inline_hash("window.tv=1;")),
            "the nonce displaced the shell's inline-script hashes: {csp}"
        );
        // Neither concession the nonce exists to avoid may ride along with it.
        assert!(!csp.contains("'unsafe-eval'"), "{csp}");
        assert!(!script_src(csp).contains("'unsafe-inline'"), "{csp}");
    }
    assert_ne!(first, second, "the same nonce was reused across responses");
}

/// `csp_shell::Headers::cache_control` is an obligation, not a suggestion: a nonce served
/// from cache is pinned across every reader for the lifetime of the entry, which is
/// `'unsafe-inline'` under another name. This tier discharges it with the shell's
/// unconditional `no-store` instead of by reading the field, so the two have to be pinned
/// together: weakening it would silently invalidate the whole nonce argument.
///
/// `no-store` and not `no-cache`: the latter permits a stored copy and only requires it to be
/// revalidated, and a `304` then pairs one response's nonce with another response's body.
/// That is not hypothetical; see [`a_conditional_request_for_the_shell_is_never_answered_304`].
#[tokio::test]
async fn a_nonced_shell_is_served_uncacheable() {
    let upstream = spawn_stub_upstream().await;
    let dir = write_bundle();
    let front = spawn_frontend_with(
        FrontendConfig {
            cloudflare: CloudflareConfig {
                script_nonce: true,
                turnstile: false,
            },
            ..test_config(dir.to_str().unwrap())
        },
        upstream,
    )
    .await;

    let response = reqwest::get(format!("http://{front}/")).await.unwrap();
    assert_eq!(response.headers().get("cache-control").unwrap(), "no-store");
}

/// Off by default, and off is the whole policy unchanged: a deployment that is not behind
/// Cloudflare must not carry either concession.
#[tokio::test]
async fn the_cloudflare_flags_are_off_by_default() {
    let upstream = spawn_stub_upstream().await;
    let dir = write_bundle();
    let front = spawn_frontend(dir.to_str().unwrap(), upstream).await;

    let csp = served_csp(front).await;
    assert!(!csp.contains("nonce-"), "{csp}");
    assert!(!csp.contains("challenges.cloudflare.com"), "{csp}");
    assert!(!csp.contains("frame-src"), "{csp}");
    // Two responses are byte-identical without the flag, so the header stays cacheable.
    assert_eq!(csp, served_csp(front).await);
}

/// Turnstile serves `api.js` and the widget's iframe from one origin `'self'` cannot cover,
/// and admitting the script without the frame renders an empty box, so the flag has to
/// reach both directives.
#[tokio::test]
async fn the_turnstile_flag_admits_the_widget_origin_in_both_directives() {
    let upstream = spawn_stub_upstream().await;
    let dir = write_bundle();
    let front = spawn_frontend_with(
        FrontendConfig {
            cloudflare: CloudflareConfig {
                script_nonce: false,
                turnstile: true,
            },
            ..test_config(dir.to_str().unwrap())
        },
        upstream,
    )
    .await;

    let csp = served_csp(front).await;
    assert!(
        script_src(&csp).contains("https://challenges.cloudflare.com"),
        "{csp}"
    );
    // Seeded from the `default-src 'self'` it replaces, so admitting the widget cannot
    // silently revoke same-origin frames.
    assert!(
        csp.contains("frame-src 'self' https://challenges.cloudflare.com"),
        "{csp}"
    );
    // `frame-ancestors` is what stops this app being framed; the widget being framed *by*
    // it is the opposite direction and must not have loosened it.
    assert!(csp.contains("frame-ancestors 'none'"), "{csp}");
}

#[tokio::test]
async fn healthz_returns_ok() {
    let upstream = spawn_stub_upstream().await;
    let dir = write_bundle();
    let front = spawn_frontend(dir.to_str().unwrap(), upstream).await;

    let response = reqwest::get(format!("http://{front}/healthz"))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.text().await.unwrap(), "ok\n");
}

#[tokio::test]
async fn static_assets_are_served_with_hardening_headers() {
    let upstream = spawn_stub_upstream().await;
    let dir = write_bundle();
    let front = spawn_frontend(dir.to_str().unwrap(), upstream).await;

    let response = reqwest::get(format!("http://{front}/app.js"))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response.headers().get("x-content-type-options").unwrap(),
        "nosniff"
    );
    assert_eq!(response.headers().get("x-frame-options").unwrap(), "DENY");
    assert_eq!(response.text().await.unwrap(), "APPJS");
}

/// The obligation the whole notices artefact exists for: a reader whose browser downloads
/// and runs the WASM bundle has received a binary distribution, and almost every licence in
/// it requires its text to travel along. Serving them only inside the image would satisfy
/// whoever pulls the image and nobody who actually runs the code.
///
/// `text/plain` matters as much as the 200: the file is extensionless, so left to content
/// sniffing it is `application/octet-stream`, which a browser downloads instead of showing.
#[tokio::test]
async fn the_licence_notices_are_served_to_the_reader_as_text() {
    let upstream = spawn_stub_upstream().await;
    let dir = write_bundle();
    let front = spawn_frontend(dir.to_str().unwrap(), upstream).await;

    let response = reqwest::get(format!("http://{front}{NOTICES_ROUTE}"))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response.headers().get("content-type").unwrap(),
        "text/plain; charset=utf-8"
    );
    assert_eq!(
        response.headers().get("x-content-type-options").unwrap(),
        "nosniff"
    );
    assert_eq!(response.text().await.unwrap(), NOTICES);
}

/// The inventory the `/licenses` screen fetches, and the `application/json` that lets it be
/// parsed instead of downloaded.
#[tokio::test]
async fn the_licence_inventory_is_served_to_the_reader_as_json() {
    let upstream = spawn_stub_upstream().await;
    let dir = write_bundle();
    let front = spawn_frontend(dir.to_str().unwrap(), upstream).await;

    let response = reqwest::get(format!("http://{front}{NOTICES_JSON_ROUTE}"))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response.headers().get("content-type").unwrap(),
        "application/json; charset=utf-8"
    );
    assert_eq!(response.text().await.unwrap(), NOTICES_JSON);
}

/// The same trap as [`missing_notices_are_a_404_rather_than_the_app_shell`], and worse for
/// being silent: an image built before the inventory existed has no file here, and the SPA
/// fallback would hand the screen the app shell. It would parse that as JSON, fail, and
/// report a corrupt inventory instead of an absent one.
#[tokio::test]
async fn a_missing_inventory_is_a_404_rather_than_the_app_shell() {
    let upstream = spawn_stub_upstream().await;
    let dir = write_bundle();
    let front = spawn_frontend_with(
        FrontendConfig {
            notices_json_path: "./no-such-inventory.json".to_owned(),
            ..test_config(dir.to_str().unwrap())
        },
        upstream,
    )
    .await;

    let response = reqwest::get(format!("http://{front}{NOTICES_JSON_ROUTE}"))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
}

/// Pins why the notices are a route and not a file in the served bundle: every unmatched
/// path resolves to the app shell, so a missing or renamed notices file would answer `200`
/// with a page of HTML claiming to be the licence texts, and no gate would see it.
#[tokio::test]
async fn missing_notices_are_a_404_rather_than_the_app_shell() {
    let upstream = spawn_stub_upstream().await;
    let dir = write_bundle();
    let front = spawn_frontend_with(
        FrontendConfig {
            notices_path: "./no-such-notices-file".to_owned(),
            ..test_config(dir.to_str().unwrap())
        },
        upstream,
    )
    .await;

    let response = reqwest::get(format!("http://{front}{NOTICES_ROUTE}"))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
    assert_ne!(response.text().await.unwrap(), SHELL);
}

#[tokio::test]
async fn unknown_paths_fall_back_to_the_app_shell() {
    let upstream = spawn_stub_upstream().await;
    let dir = write_bundle();
    let front = spawn_frontend(dir.to_str().unwrap(), upstream).await;

    // A client-side route with no matching file must resolve to index.html, not 404.
    let response = reqwest::get(format!("http://{front}/series/abc/deep"))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.text().await.unwrap(), SHELL);
}

#[tokio::test]
async fn v1_requests_are_proxied_with_the_client_ip_forwarded() {
    let upstream = spawn_stub_upstream().await;
    let dir = write_bundle();
    let front = spawn_frontend(dir.to_str().unwrap(), upstream).await;

    let response = reqwest::get(format!("http://{front}/v1/echo"))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    // The proxy must have injected the loopback peer as X-Forwarded-For for the API.
    let echoed = response
        .headers()
        .get("x-echo-xff")
        .unwrap()
        .to_str()
        .unwrap()
        .to_owned();
    assert!(echoed.contains("127.0.0.1"), "XFF not forwarded: {echoed}");
    assert_eq!(response.text().await.unwrap(), format!("echo:{echoed}"));
}

#[tokio::test]
async fn proxy_passes_upstream_status_through() {
    let upstream = spawn_stub_upstream().await;
    let dir = write_bundle();
    let front = spawn_frontend(dir.to_str().unwrap(), upstream).await;

    let response = reqwest::get(format!("http://{front}/v1/status/404"))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
}

#[test]
fn forwarded_for_appends_to_an_existing_chain() {
    let mut headers = HeaderMap::new();
    headers.insert(
        HeaderName::from_static("x-forwarded-for"),
        HeaderValue::from_static("203.0.113.7"),
    );
    set_forwarded_headers(&mut headers, "198.51.100.2");
    assert_eq!(
        headers.get("x-forwarded-for").unwrap(),
        "203.0.113.7, 198.51.100.2"
    );
    assert_eq!(headers.get("x-real-ip").unwrap(), "198.51.100.2");
}

#[test]
fn hop_by_hop_headers_are_stripped() {
    let mut headers = HeaderMap::new();
    headers.insert(header::CONNECTION, HeaderValue::from_static("keep-alive"));
    headers.insert(header::UPGRADE, HeaderValue::from_static("websocket"));
    headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/json"),
    );
    strip_hop_by_hop(&mut headers);
    assert!(headers.get(header::CONNECTION).is_none());
    assert!(headers.get(header::UPGRADE).is_none());
    // A non-hop-by-hop header is untouched.
    assert!(headers.get(header::CONTENT_TYPE).is_some());
}
