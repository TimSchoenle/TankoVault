//! The authenticated-tier access-control matrix — the other half of
//! [`admin_access_matrix`](../admin_access_matrix.rs): every `/v1/me` route driven anonymous
//! (401), suspended (403), and as an ordinary account (neither), reconciled against the
//! published `OpenAPI` document. Gated behind the `integration` feature; requires Docker.
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
    /// A single-use `ticket` query parameter, since `EventSource` cannot set headers. Minted
    /// through the store, not `POST /v1/me/stream-ticket`, whose own `AuthUser` gate would mask
    /// the check under test on the stream itself.
    StreamTicket,
}

/// One authenticated endpoint, as the matrix drives it.
struct Gate {
    method: &'static str,
    /// The `OpenAPI` path template, used to reconcile against the published document.
    template: &'static str,
    /// Concrete path with parameters filled in; mandatory query parameters must be present or
    /// extraction would 400 before the authorization check runs.
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
/// One long table by design, as in the admin matrix: split per module and a whole module could
/// fall out of the matrix unnoticed.
#[expect(
    clippy::too_many_lines,
    reason = "the matrix is one endpoint per line; splitting it would hide what it covers"
)]
fn me_gates() -> Vec<Gate> {
    vec![
        // --- account ---
        Gate {
            // Deliberately wrong, so the admitted leg reaches a `400` rather than deleting the caller.
            body: || Some(json!({ "confirm_username": "not-the-caller" })),
            ..gate("DELETE", "/v1/me", "/v1/me")
        },
        get("/v1/me/capabilities", "/v1/me/capabilities"),
        get("/v1/me/export", "/v1/me/export"),
        Gate {
            // Every field is optional, so an empty patch cannot disturb the account later legs reuse.
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
        // --- passkeys (the management half; the sign-in half is unauthenticated by design) ---
        get("/v1/me/passkeys", "/v1/me/passkeys"),
        Gate {
            body: || {
                Some(json!({
                    "current_password": "not-the-seeded-hash",
                    "label": "matrix",
                }))
            },
            admitted_leg_skipped: Some(
                "answers 401 for a wrong `current_password`, which is the same status as \
                 no session; the authenticated path is covered by passkeys.rs",
            ),
            ..gate(
                "POST",
                "/v1/me/passkeys/register/start",
                "/v1/me/passkeys/register/start",
            )
        },
        Gate {
            // An unparseable credential is refused (400) before the ceremony is looked up, so the
            // admitted leg neither needs a live ceremony nor hits the 401 an absent one answers.
            body: || {
                Some(json!({
                    "ceremony_id": "00000000-0000-7000-8000-00000000000a",
                    "credential": {},
                }))
            },
            ..gate(
                "POST",
                "/v1/me/passkeys/register/finish",
                "/v1/me/passkeys/register/finish",
            )
        },
        Gate {
            body: || Some(json!({ "label": "matrix" })),
            ..gate(
                "PATCH",
                "/v1/me/passkeys/{id}",
                "/v1/me/passkeys/00000000-0000-7000-8000-00000000000a",
            )
        },
        gate(
            "DELETE",
            "/v1/me/passkeys/{id}",
            "/v1/me/passkeys/00000000-0000-7000-8000-00000000000a",
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
            // Free-form by design (`request_body = serde_json::Value`); an empty object is valid.
            body: || Some(json!({})),
            ..gate(
                "PUT",
                "/v1/me/notification-prefs",
                "/v1/me/notification-prefs",
            )
        },
        Gate {
            credential: Credential::StreamTicket,
            ..get("/v1/me/stream", "/v1/me/stream")
        },
        gate("POST", "/v1/me/stream-ticket", "/v1/me/stream-ticket"),
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
        Gate {
            body: || Some(json!({ "series_ids": ["00000000-0000-7000-8000-00000000000a"] })),
            ..gate(
                "POST",
                "/v1/me/progress/bulk-read",
                "/v1/me/progress/bulk-read",
            )
        },
        // --- watchlist ---
        get("/v1/me/watchlist", "/v1/me/watchlist"),
        get("/v1/me/watchlist/summary", "/v1/me/watchlist/summary"),
        get(
            "/v1/me/watchlist/{series_id}",
            "/v1/me/watchlist/00000000-0000-7000-8000-00000000000a",
        ),
        // The bulk pair is a static segment under the same prefix as `{series_id}`; driving both
        // proves the router resolves `bulk` to the bulk handler, not the parameterised one.
        Gate {
            body: || {
                Some(json!({
                    "series_ids": ["00000000-0000-7000-8000-00000000000a"],
                    "status": "dropped"
                }))
            },
            ..gate("POST", "/v1/me/watchlist/bulk", "/v1/me/watchlist/bulk")
        },
        Gate {
            body: || Some(json!({ "series_ids": ["00000000-0000-7000-8000-00000000000a"] })),
            ..gate("DELETE", "/v1/me/watchlist/bulk", "/v1/me/watchlist/bulk")
        },
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

/// Reachable with no session at all, by design: the catalogue tier, and the legal documents.
///
/// Asserted because the failure is silent otherwise — an `AuthUser` on a browse route breaks
/// the front page for everyone, but a developer testing while signed in wouldn't notice.
///
/// The legal routes are here for a sharper reason than symmetry: **registering is the act of
/// accepting the Terms**, so the register form has to link them to a caller who by definition
/// has no account yet. Putting them behind the same auth layer as the rest of `/v1` is a
/// one-line mistake that every other test in this file would pass, because everywhere else a
/// `401` is the correct answer.
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
        (
            "/v1/series/{id}/similar",
            "/v1/series/00000000-0000-7000-8000-00000000000a/similar",
        ),
        ("/v1/tags", "/v1/tags"),
        ("/v1/legal", "/v1/legal"),
        // The harness publishes no documents, so this answers `404` — which is the point: a
        // `404` is not a `401`, so the assertion still distinguishes "no such document" from
        // "you must sign in to read the privacy policy".
        ("/v1/legal/{slug}", "/v1/legal/terms"),
    ]
}

/// Endpoints deliberately outside this file, each with the reason and where they *are* covered.
///
/// The reconciliation test consults this list, so "not in the matrix" is always a decision
/// somebody wrote down rather than an omission.
fn covered_elsewhere() -> Vec<(&'static str, &'static str)> {
    // A status-only leg can't distinguish "no session" from "wrong password" here (both 401),
    // so these are driven with real credentials elsewhere instead.
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
    // Sign-in with a passkey is credential-free on purpose — there is no session yet, and the
    // challenge is deliberately identifier-free — so neither leg of this matrix applies.
    let passkey_login = [
        "POST /v1/auth/passkey/login/start",
        "POST /v1/auth/passkey/login/finish",
    ];
    auth.into_iter()
        .map(|op| {
            (
                op,
                "auth_flows.rs / auth_lifecycle.rs, with real credentials",
            )
        })
        .chain(
            passkey_login
                .into_iter()
                .map(|op| (op, "passkeys.rs, with real ceremonies")),
        )
        .collect()
}

/// A seeded account, in the two forms the matrix has to present it in.
struct Caller {
    bearer: String,
    user: UserId,
}

/// Build the URI and `Authorization` header for one leg of one gate.
///
/// `caller` is `None` for the anonymous leg. A `StreamTicket` route still needs *a* ticket in the
/// URL there, since a missing parameter would 400 before reaching the check under test — so the
/// anonymous leg presents a value that cannot redeem.
async fn credential_for(
    app: &TestApp,
    gate: &Gate,
    caller: Option<&Caller>,
) -> (String, Option<String>) {
    match (gate.credential, caller) {
        (Credential::StreamTicket, Some(caller)) => {
            let ticket = app.stream_ticket(caller.user).await;
            (with_ticket(gate.path, &ticket), None)
        }
        (Credential::StreamTicket, None) => (with_ticket(gate.path, "not-a-ticket"), None),
        (Credential::Header, caller) => (gate.path.to_owned(), caller.map(|c| c.bearer.clone())),
    }
}

/// Append the query credential the `Credential::StreamTicket` routes read.
fn with_ticket(path: &str, ticket: &str) -> String {
    let separator = if path.contains('?') { '&' } else { '?' };
    format!("{path}{separator}ticket={ticket}")
}

/// Assemble the request for one leg, given the URI and header `credential_for` decided.
fn build(gate: &Gate, uri: String, bearer: Option<String>) -> Request<Body> {
    let mut builder = Request::builder().method(gate.method).uri(uri);
    if let Some(value) = bearer {
        builder = builder.header(header::AUTHORIZATION, value);
    }
    match (gate.body)() {
        Some(json) => builder
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(serde_json::to_vec(&json).expect("serialize")))
            .expect("build request"),
        None => builder.body(Body::empty()).expect("build request"),
    }
}

/// Drive one request and return its status **without draining the body** — `/v1/me/stream` is an
/// SSE stream that never ends, and draining it would hang the suite.
async fn status_of(app: &TestApp, gate: &Gate, caller: Option<&Caller>) -> StatusCode {
    let (uri, bearer) = credential_for(app, gate, caller).await;
    app.request(build(gate, uri, bearer)).await.status()
}

/// Read a response body as JSON, for the legs that assert on the problem document.
async fn problem_of(app: &TestApp, gate: &Gate, caller: &Caller) -> (StatusCode, Value) {
    let (uri, bearer) = credential_for(app, gate, Some(caller)).await;
    let response = app.request(build(gate, uri, bearer)).await;
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("read body");
    let json = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    (status, json)
}

async fn a_user(app: &TestApp, username: &str, status: AccountStatus) -> Caller {
    let user = app.seed_user(username, &[], status).await;
    Caller {
        bearer: app.bearer(user),
        user,
    }
}

// ---------------------------------------------------------------------------
// The three legs
// ---------------------------------------------------------------------------

/// No `/v1/me` endpoint answers without a session.
///
/// A route added without the `AuthUser` extractor serves one person's private data to anybody
/// who guesses the path, while every other (authenticated) test in the suite passes.
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
/// Suspension is enforced inside `AuthUser`, so this holds for free until a handler resolves
/// claims some other way, as `/v1/me/stream` once did. The body is asserted too, since `403`
/// alone can't distinguish "suspended" from "insufficient privileges".
#[tokio::test]
async fn every_me_endpoint_refuses_a_suspended_account() {
    let app = TestApp::spawn_with(TestConfig::new().without_rate_limiting()).await;
    let banned = a_user(&app, "banned", AccountStatus::Suspended).await;

    for gate in me_gates() {
        let (status, body) = problem_of(&app, &gate, &banned).await;
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
/// The inverse failure: a route that requires a capability is dead for every real user, and
/// nobody testing with an admin token would notice. Not `is_success()` — these requests name
/// absent rows and an unreachable sync service on purpose, so `404`/`400`/`502` are correct.
#[tokio::test]
async fn every_me_endpoint_admits_an_ordinary_account() {
    let app = TestApp::spawn_with(TestConfig::new().without_rate_limiting()).await;
    let plain = a_user(&app, "plain", AccountStatus::Active).await;

    for gate in me_gates() {
        if gate.admitted_leg_skipped.is_some() {
            continue;
        }
        let status = status_of(&app, &gate, Some(&plain)).await;
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
/// Reconciled against the published document rather than a hand-maintained list, so adding a
/// route without classifying it turns the build red on the pull request that adds it.
/// `/v1/admin` is `admin_access_matrix.rs`'s half, reconciled there against the same document.
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

/// A stream ticket opens the stream exactly once.
///
/// The credential travels in the query string, so single-use is what makes a recorded log line
/// harmless. `503` is the success signal: this harness wires no NATS, so an accepted ticket
/// lands there while a rejected one is `401`.
#[tokio::test]
async fn a_stream_ticket_cannot_open_the_stream_twice() {
    let app = TestApp::spawn_with(TestConfig::new().without_rate_limiting()).await;
    let reader = a_user(&app, "streamer", AccountStatus::Active).await;
    let ticket = app.stream_ticket(reader.user).await;

    let first = app
        .request(
            Request::builder()
                .method("GET")
                .uri(with_ticket("/v1/me/stream", &ticket))
                .body(Body::empty())
                .expect("build request"),
        )
        .await;
    assert_eq!(
        first.status(),
        StatusCode::SERVICE_UNAVAILABLE,
        "a fresh ticket must be accepted; 503 is this harness's no-NATS answer"
    );

    let replay = app
        .request(
            Request::builder()
                .method("GET")
                .uri(with_ticket("/v1/me/stream", &ticket))
                .body(Body::empty())
                .expect("build request"),
        )
        .await;
    assert_eq!(
        replay.status(),
        StatusCode::UNAUTHORIZED,
        "a spent ticket must not open a second stream — this is what makes a leaked log line \
         worthless"
    );
}

/// The mint endpoint hands out a ticket the stream accepts.
///
/// # The bug this pins
///
/// The two halves are wired through separate state, so they can drift silently: a mint storing
/// under a different key, or a stream reading a different query parameter, leaves the endpoint
/// answering `200` and the stream `401` forever — the shape of a bug once found on the frontend
/// (`?token=` against `?access_token=`), where nothing connected producer to reader.
#[tokio::test]
async fn the_minted_ticket_is_the_one_the_stream_accepts() {
    let app = TestApp::spawn_with(TestConfig::new().without_rate_limiting()).await;
    let reader = a_user(&app, "minter", AccountStatus::Active).await;

    let (status, body) = app
        .call("POST", "/v1/me/stream-ticket", Some(&reader.bearer), None)
        .await;
    assert_eq!(status, StatusCode::OK);
    let ticket = body["ticket"].as_str().expect("a ticket value").to_owned();
    assert!(!ticket.is_empty());
    assert!(
        body["expires_in"]
            .as_u64()
            .is_some_and(|s| s > 0 && s <= 60),
        "a stream ticket must be short-lived, got {body}"
    );

    let response = app
        .request(
            Request::builder()
                .method("GET")
                .uri(with_ticket("/v1/me/stream", &ticket))
                .body(Body::empty())
                .expect("build request"),
        )
        .await;
    assert_eq!(
        response.status(),
        StatusCode::SERVICE_UNAVAILABLE,
        "the minted ticket must be accepted by the stream, not rejected as unknown"
    );
}
