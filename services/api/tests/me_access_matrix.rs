//! The authenticated-tier access-control matrix — the other half of
//! [`admin_access_matrix`](../admin_access_matrix.rs).
//!
//! That file proves every `/v1/admin` route is gated by the *right capability*. Nothing
//! proved the far larger `/v1/me` surface is gated at all. The properties are different:
//! `/v1/me` routes carry no capability requirement — they are scoped to the caller — so what
//! has to hold for every one of them is:
//!
//! 1. **anonymous → `401`**. A `/v1/me` route added without the `AuthUser` extractor is
//!    world-readable personal data. This is the leg that matters, and until now the only thing
//!    standing behind it was that every author remembered.
//! 2. **suspended → `403 account_suspended`**. Suspension is enforced inside the extractor, so
//!    a route reaching for the claims another way — which `GET /v1/me/stream` genuinely does,
//!    and which SEC-8 found unenforced there — silently keeps a banned account working until
//!    its access token expires.
//! 3. **an ordinary active account → neither `401` nor `403`**. The inverse error: a `/v1/me`
//!    route that accidentally requires a capability is dead for every real user, and no
//!    happy-path test written by someone with an admin token would notice.
//!
//! [`every_published_endpoint_is_covered_by_one_of_these_matrices`] reconciles all three legs
//! against the published `OpenAPI` document, so a new route must be classified by a human
//! rather than quietly shipping unverified.
//!
//! Opt-in: gated behind the `integration` feature because it requires Docker.
#![cfg(feature = "integration")]

use std::collections::BTreeSet;

use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use serde_json::{Value, json};
use tankovault_api_test_support::{TestApp, TestConfig};
use tankovault_domain::{AccountStatus, UserId};

/// How a route carries its credential.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Credential {
    /// The `Authorization: Bearer …` header, like every other route.
    Header,
    /// An `access_token` query parameter. `EventSource` cannot set headers, so `/v1/me/stream`
    /// takes its token in the URL (SEC-8). It is still a full access token and all three legs
    /// still apply — they just have to be driven through the query string.
    Query,
}

/// One authenticated endpoint, as the matrix drives it.
struct Gate {
    method: &'static str,
    /// The `OpenAPI` path template, used to reconcile against the published document.
    template: &'static str,
    /// The concrete path requested, with path parameters and any *mandatory* query parameters
    /// filled in — extraction runs before the handler, so a missing one would answer `400` and
    /// mask the authorization result.
    path: &'static str,
    credential: Credential,
    /// A body that deserializes, for the same reason.
    body: fn() -> Option<Value>,
    /// Set when the admitted leg cannot be asserted, with the reason. Only for handlers that
    /// answer `401` for a *second*, non-session reason, where the status cannot distinguish
    /// "no session" from "wrong secret".
    admitted_leg_skipped: Option<&'static str>,
}

fn empty() -> Option<Value> {
    None
}

/// A `Gate` with the common shape: header credential, no body, admitted leg asserted.
const fn get(template: &'static str, path: &'static str) -> Gate {
    Gate {
        method: "GET",
        template,
        path,
        credential: Credential::Header,
        body: empty,
        admitted_leg_skipped: None,
    }
}

const fn gate(method: &'static str, template: &'static str, path: &'static str) -> Gate {
    Gate {
        method,
        template,
        path,
        credential: Credential::Header,
        body: empty,
        admitted_leg_skipped: None,
    }
}

/// Every endpoint under `/v1/me`.
///
/// One long table by design, exactly as in the admin matrix: split per module and a whole
/// module can fall out of the matrix without anything noticing.
#[allow(clippy::too_many_lines)]
fn me_gates() -> Vec<Gate> {
    vec![
        // --- account ---
        Gate {
            // A deliberately wrong confirmation, so the admitted leg reaches a `400` rather
            // than deleting the caller mid-matrix.
            body: || Some(json!({ "confirm_username": "not-the-caller" })),
            ..gate("DELETE", "/v1/me", "/v1/me")
        },
        get("/v1/me/capabilities", "/v1/me/capabilities"),
        get("/v1/me/export", "/v1/me/export"),
        Gate {
            // Every field is optional and an empty patch changes nothing, so this is the one
            // shape that cannot disturb the account the later legs reuse.
            body: || Some(json!({})),
            ..gate("PATCH", "/v1/me/profile", "/v1/me/profile")
        },
        Gate {
            body: || {
                Some(json!({
                    "current_password": "not-the-seeded-hash",
                    "new_password": "correct horse battery staple",
                }))
            },
            admitted_leg_skipped: Some(
                "answers 401 for a wrong `current_password`, which is the same status as \
                 no session; the authenticated path is covered by auth_lifecycle.rs",
            ),
            ..gate("POST", "/v1/me/password", "/v1/me/password")
        },
        get("/v1/me/sessions", "/v1/me/sessions"),
        gate(
            "DELETE",
            "/v1/me/sessions/{id}",
            "/v1/me/sessions/00000000-0000-7000-8000-00000000000a",
        ),
        // --- notifications ---
        get("/v1/me/notifications", "/v1/me/notifications"),
        Gate {
            body: || Some(json!({ "ids": [] })),
            ..gate(
                "POST",
                "/v1/me/notifications/read",
                "/v1/me/notifications/read",
            )
        },
        get("/v1/me/notification-prefs", "/v1/me/notification-prefs"),
        Gate {
            // Free-form by design (`request_body = serde_json::Value`), so an empty object is
            // a valid document rather than a body the handler will reject.
            body: || Some(json!({})),
            ..gate(
                "PUT",
                "/v1/me/notification-prefs",
                "/v1/me/notification-prefs",
            )
        },
        Gate {
            credential: Credential::Query,
            ..get("/v1/me/stream", "/v1/me/stream")
        },
        // --- dashboard ---
        get("/v1/me/continue", "/v1/me/continue"),
        get("/v1/me/feed", "/v1/me/feed"),
        get("/v1/me/recommendations", "/v1/me/recommendations"),
        get("/v1/me/stats", "/v1/me/stats"),
        // --- privacy (GDPR self-service) ---
        get("/v1/me/privacy/requests", "/v1/me/privacy/requests"),
        Gate {
            body: || Some(json!({ "kind": "access" })),
            ..gate("POST", "/v1/me/privacy/requests", "/v1/me/privacy/requests")
        },
        gate(
            "DELETE",
            "/v1/me/privacy/requests/{id}",
            "/v1/me/privacy/requests/00000000-0000-7000-8000-00000000000a",
        ),
        // --- reading progress ---
        get(
            "/v1/me/progress/{series_id}",
            "/v1/me/progress/00000000-0000-7000-8000-00000000000a",
        ),
        Gate {
            body: || Some(json!({ "last_read_whole_number": 1.0 })),
            ..gate(
                "PUT",
                "/v1/me/progress/{series_id}",
                "/v1/me/progress/00000000-0000-7000-8000-00000000000a",
            )
        },
        Gate {
            body: || Some(json!({ "read": true })),
            ..gate(
                "PUT",
                "/v1/me/progress/{series_id}/chapters/{number}",
                "/v1/me/progress/00000000-0000-7000-8000-00000000000a/chapters/1",
            )
        },
        Gate {
            body: || Some(json!({ "number": 1.0 })),
            ..gate(
                "POST",
                "/v1/me/progress/{series_id}/mark-read-to",
                "/v1/me/progress/00000000-0000-7000-8000-00000000000a/mark-read-to",
            )
        },
        // --- watchlist ---
        get("/v1/me/watchlist", "/v1/me/watchlist"),
        Gate {
            body: || Some(json!({ "status": "reading", "notify": true })),
            ..gate(
                "PUT",
                "/v1/me/watchlist/{series_id}",
                "/v1/me/watchlist/00000000-0000-7000-8000-00000000000a",
            )
        },
        gate(
            "DELETE",
            "/v1/me/watchlist/{series_id}",
            "/v1/me/watchlist/00000000-0000-7000-8000-00000000000a",
        ),
        Gate {
            body: || Some(json!({ "excluded": true })),
            ..gate(
                "PUT",
                "/v1/me/watchlist/{series_id}/sync",
                "/v1/me/watchlist/00000000-0000-7000-8000-00000000000a/sync",
            )
        },
        Gate {
            body: || Some(json!({ "excluded": true })),
            ..gate(
                "PUT",
                "/v1/me/watchlist/{series_id}/sync/{provider}",
                "/v1/me/watchlist/00000000-0000-7000-8000-00000000000a/sync/anilist",
            )
        },
        // --- external sync (proxied to `services/sync`, which is not running here; an
        //     admitted call lands on a gateway error, which is still an admission) ---
        get("/v1/me/sync/providers", "/v1/me/sync/providers"),
        get("/v1/me/sync/conflicts", "/v1/me/sync/conflicts"),
        Gate {
            body: || Some(json!({ "resolution": "local" })),
            ..gate(
                "POST",
                "/v1/me/sync/conflicts/{id}/resolve",
                "/v1/me/sync/conflicts/00000000-0000-7000-8000-00000000000a/resolve",
            )
        },
        get("/v1/me/sync/history", "/v1/me/sync/history"),
        gate("DELETE", "/v1/me/sync/{provider}", "/v1/me/sync/anilist"),
        get(
            "/v1/me/sync/{provider}/authorize",
            "/v1/me/sync/anilist/authorize",
        ),
        get(
            "/v1/me/sync/{provider}/callback",
            "/v1/me/sync/anilist/callback?code=matrix",
        ),
        gate(
            "POST",
            "/v1/me/sync/{provider}/pull",
            "/v1/me/sync/anilist/pull",
        ),
        gate(
            "POST",
            "/v1/me/sync/{provider}/push",
            "/v1/me/sync/anilist/push",
        ),
        get(
            "/v1/me/sync/{provider}/settings",
            "/v1/me/sync/anilist/settings",
        ),
        Gate {
            body: || Some(json!({})),
            ..gate(
                "PATCH",
                "/v1/me/sync/{provider}/settings",
                "/v1/me/sync/anilist/settings",
            )
        },
        get(
            "/v1/me/sync/{provider}/status",
            "/v1/me/sync/anilist/status",
        ),
    ]
}

/// The catalogue tier: reachable with no session at all, by design.
///
/// Asserted because the failure is silent in the other direction — putting `AuthUser` on a
/// browse route makes the product's front page require an account, and every developer who is
/// signed in while testing sees it working.
fn public_gates() -> Vec<(&'static str, &'static str)> {
    vec![
        ("/v1/providers", "/v1/providers"),
        ("/v1/series", "/v1/series"),
        (
            "/v1/series/{id}",
            "/v1/series/00000000-0000-7000-8000-00000000000a",
        ),
        (
            "/v1/series/{id}/chapters",
            "/v1/series/00000000-0000-7000-8000-00000000000a/chapters",
        ),
        ("/v1/tags", "/v1/tags"),
    ]
}

/// Endpoints deliberately outside this file, each with the reason and where they *are* covered.
///
/// The reconciliation test consults this list, so "not in the matrix" is always a decision
/// somebody wrote down rather than an omission.
fn covered_elsewhere() -> Vec<(&'static str, &'static str)> {
    // The credential tier. A status-only leg cannot distinguish "no session" from "wrong
    // password" here — both are 401 by design — so these are driven with real credentials in
    // `auth_flows.rs` and `auth_lifecycle.rs` instead.
    let auth = [
        "POST /v1/auth/login",
        "POST /v1/auth/logout",
        "POST /v1/auth/refresh",
        "POST /v1/auth/register",
        "POST /v1/auth/password/forgot",
        "POST /v1/auth/password/reset",
        "POST /v1/auth/verify-email",
        "POST /v1/auth/verify-email/resend",
    ];
    auth.into_iter()
        .map(|op| {
            (
                op,
                "auth_flows.rs / auth_lifecycle.rs, with real credentials",
            )
        })
        .collect()
}

/// Drive one request and return its status **without draining the body**.
///
/// `/v1/me/stream` answers with a Server-Sent-Events stream that does not end; reading it to
/// completion would hang the suite. The matrix asserts on the status line only.
async fn status_of(app: &TestApp, gate: &Gate, credential: Option<&str>) -> StatusCode {
    let (uri, header_value) = match (gate.credential, credential) {
        (Credential::Query, Some(bearer)) => {
            // The query form carries the raw JWT, not the `Bearer …` header value.
            let raw = bearer.strip_prefix("Bearer ").unwrap_or(bearer);
            (replace_access_token(gate.path, raw), None)
        }
        (Credential::Query, None) => (
            // No credential at all would fail `Query` extraction with a `400` and never reach
            // the check, so the anonymous leg presents a token that cannot verify.
            replace_access_token(gate.path, "not-a-token"),
            None,
        ),
        (Credential::Header, cred) => (gate.path.to_owned(), cred),
    };

    let mut builder = Request::builder().method(gate.method).uri(uri);
    if let Some(value) = header_value {
        builder = builder.header(header::AUTHORIZATION, value);
    }
    let request = match (gate.body)() {
        Some(json) => builder
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(serde_json::to_vec(&json).expect("serialize")))
            .expect("build request"),
        None => builder.body(Body::empty()).expect("build request"),
    };
    app.request(request).await.status()
}

/// Append the query credential the `Credential::Query` routes read.
fn replace_access_token(path: &str, token: &str) -> String {
    let separator = if path.contains('?') { '&' } else { '?' };
    format!("{path}{separator}access_token={token}")
}

/// Read a response body as JSON, for the legs that assert on the problem document.
async fn problem_of(app: &TestApp, gate: &Gate, credential: &str) -> (StatusCode, Value) {
    let raw = credential.strip_prefix("Bearer ").unwrap_or(credential);
    let (uri, header_value) = match gate.credential {
        Credential::Query => (replace_access_token(gate.path, raw), None),
        Credential::Header => (gate.path.to_owned(), Some(credential)),
    };
    let mut builder = Request::builder().method(gate.method).uri(uri);
    if let Some(value) = header_value {
        builder = builder.header(header::AUTHORIZATION, value);
    }
    let request = match (gate.body)() {
        Some(json) => builder
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(serde_json::to_vec(&json).expect("serialize")))
            .expect("build request"),
        None => builder.body(Body::empty()).expect("build request"),
    };
    let response = app.request(request).await;
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("read body");
    let json = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    (status, json)
}

async fn a_user(app: &TestApp, username: &str, status: AccountStatus) -> UserId {
    app.seed_user(username, &[], status).await
}

// ---------------------------------------------------------------------------
// The three legs
// ---------------------------------------------------------------------------

/// No `/v1/me` endpoint answers without a session.
///
/// The whole point of the file. A route added without the `AuthUser` extractor serves one
/// person's reading history, sessions or GDPR export to anybody who guesses the path, and
/// every other test in the suite — all of which authenticate — passes.
#[tokio::test]
async fn no_me_endpoint_answers_without_a_session() {
    let app = TestApp::spawn_with(TestConfig::new().without_rate_limiting()).await;

    for gate in me_gates() {
        let status = status_of(&app, &gate, None).await;
        assert_eq!(
            status,
            StatusCode::UNAUTHORIZED,
            "{} {} without a session must be 401",
            gate.method,
            gate.template
        );
    }
}

/// A suspended account is refused everywhere, and told why.
///
/// Suspension is enforced inside the `AuthUser` extractor, so this holds for free — right up
/// until a handler resolves the claims some other way. `/v1/me/stream` did exactly that
/// (SEC-8): it verified the token and never asked whether the account was still allowed to
/// authenticate, so a banned user kept receiving live notifications until the token expired.
/// Asserting the *body* as well as the status is deliberate — `403` alone cannot distinguish
/// "suspended" from "insufficient privileges", and only the former should ever occur here.
#[tokio::test]
async fn every_me_endpoint_refuses_a_suspended_account() {
    let app = TestApp::spawn_with(TestConfig::new().without_rate_limiting()).await;
    let banned = a_user(&app, "banned", AccountStatus::Suspended).await;
    let bearer = app.bearer(banned);

    for gate in me_gates() {
        let (status, body) = problem_of(&app, &gate, &bearer).await;
        assert_eq!(
            status,
            StatusCode::FORBIDDEN,
            "{} {} must refuse a suspended account",
            gate.method,
            gate.template
        );
        assert_eq!(
            body["title"], "account_suspended",
            "{} {} must say the account is suspended, got {body}",
            gate.method, gate.template
        );
    }
}

/// An ordinary active account with no capabilities reaches every `/v1/me` endpoint.
///
/// The inverse failure: a `/v1/me` route that requires a capability is dead for every real
/// user, and nobody testing with an admin token would ever see it. Not `is_success()` — these
/// requests name absent rows and an unreachable sync service on purpose, so `404`/`400`/`502`
/// are correct. What must not happen is the authorization layer refusing the caller.
#[tokio::test]
async fn every_me_endpoint_admits_an_ordinary_account() {
    let app = TestApp::spawn_with(TestConfig::new().without_rate_limiting()).await;
    let plain = a_user(&app, "plain", AccountStatus::Active).await;
    let bearer = app.bearer(plain);

    for gate in me_gates() {
        if gate.admitted_leg_skipped.is_some() {
            continue;
        }
        let status = status_of(&app, &gate, Some(&bearer)).await;
        assert!(
            status != StatusCode::UNAUTHORIZED && status != StatusCode::FORBIDDEN,
            "{} {} must admit an ordinary account, got {status}",
            gate.method,
            gate.template
        );
    }
}

/// The catalogue tier stays open to anonymous callers.
#[tokio::test]
async fn the_public_catalogue_is_reachable_without_a_session() {
    let app = TestApp::spawn_with(TestConfig::new().without_rate_limiting()).await;

    for (template, path) in public_gates() {
        let (status, _) = app.call("GET", path, None, None).await;
        assert!(
            status != StatusCode::UNAUTHORIZED && status != StatusCode::FORBIDDEN,
            "GET {template} must not require a session, got {status}"
        );
    }
}

// ---------------------------------------------------------------------------
// Anti-rot
// ---------------------------------------------------------------------------

/// Every published endpoint is covered by one of the access-control matrices.
///
/// The published document is the authority on what the service exposes, so reconciling
/// against it — rather than against a hand-maintained list — is what stops these files
/// rotting. Adding a route without classifying it turns the build red on the pull request
/// that adds it, which is the only moment anyone can cheaply decide what its access rule
/// should be.
///
/// `/v1/admin` is `admin_access_matrix.rs`'s half and is reconciled there against the same
/// document; splitting the two files does not leave a gap between them.
#[tokio::test]
async fn every_published_endpoint_is_covered_by_one_of_these_matrices() {
    let spec = serde_json::to_value(tankovault_api::full_openapi()).expect("serialize openapi");
    let paths = spec["paths"].as_object().expect("openapi has paths");

    let mut published: BTreeSet<String> = BTreeSet::new();
    for (path, item) in paths {
        if path.starts_with("/v1/admin") {
            continue; // admin_access_matrix.rs reconciles this half.
        }
        for method in item.as_object().expect("path item is an object").keys() {
            let upper = method.to_uppercase();
            if matches!(
                upper.as_str(),
                "GET" | "POST" | "PUT" | "PATCH" | "DELETE" | "HEAD" | "OPTIONS"
            ) {
                published.insert(format!("{upper} {path}"));
            }
        }
    }

    let mut covered: BTreeSet<String> = me_gates()
        .iter()
        .map(|g| format!("{} {}", g.method, g.template))
        .collect();
    covered.extend(public_gates().iter().map(|(t, _)| format!("GET {t}")));
    covered.extend(covered_elsewhere().iter().map(|(op, _)| (*op).to_owned()));

    let unclassified: Vec<&String> = published.difference(&covered).collect();
    assert!(
        unclassified.is_empty(),
        "these endpoints are published but no access-control matrix classifies them — add a \
         row to me_gates()/public_gates(), or to covered_elsewhere() with the reason: \
         {unclassified:?}"
    );

    let stale: Vec<&String> = covered.difference(&published).collect();
    assert!(
        stale.is_empty(),
        "these matrix rows name endpoints the service no longer publishes: {stale:?}"
    );
}
