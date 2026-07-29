//! The complete admin access-control matrix.
//!
//! Every permission-gated endpoint under `/v1/admin` is driven through the *real* router —
//! the `AuthUser` extractor, the feature-flag middleware and the handler's own `require` — in
//! three legs:
//!
//! 1. **anonymous** → `401`, so no admin surface is ever reachable without a session;
//! 2. **authenticated holding every permission *except* the required one(s)** → `403`, which is
//!    the leg that matters. "No permission at all" only proves the route is gated by
//!    *something*; withholding exactly the declared capability and granting all 23 others
//!    proves it is gated by the *right* thing. A handler that asked for `ProvidersRead` where
//!    the table says `ProvidersWrite` passes the weak form and fails this one;
//! 3. **authenticated holding exactly the required capability** → anything but `401`/`403`,
//!    which proves the grant actually unlocks the route rather than the route being
//!    unreachable for everyone.
//!
//! Leg 2 also asserts the refusal was audited and named the missing capability, because a
//! silent `403` tells an incident responder nothing.
//!
//! [`the_matrix_covers_every_admin_endpoint_in_the_openapi_document`] is what keeps this file
//! honest as the API grows: a new admin route that nobody adds here fails the build rather
//! than quietly shipping unverified.
//!
//! Opt-in: gated behind the `integration` feature because it requires Docker.
#![cfg(feature = "integration")]

use std::collections::{BTreeSet, HashMap};

use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use serde_json::{Value, json};
use tankovault_domain::{AccountStatus, Permission};
use tankovault_test_support::{TestApp, TestConfig};

/// A syntactically valid UUID that names nothing. Every path parameter in the matrix uses one
/// of these: authorization is decided before the row is looked up, so the leg-3 outcome is a
/// clean `404` rather than a mutation of seeded state.
const ABSENT_A: &str = "00000000-0000-7000-8000-00000000000a";
const ABSENT_B: &str = "00000000-0000-7000-8000-00000000000b";

/// One permission-gated admin endpoint, as the matrix drives it.
struct Gate {
    /// Uppercase HTTP method.
    method: &'static str,
    /// The `OpenAPI` path template (`/v1/admin/users/{id}`), used to reconcile the matrix
    /// against the published document.
    template: &'static str,
    /// The concrete path actually requested, with path parameters and any *mandatory* query
    /// parameters filled in. Mandatory query parameters must be present even on the `403`
    /// legs: `Query` extraction runs before the handler body, so a missing one would produce a
    /// `400` and mask the authorization result.
    path: &'static str,
    /// The capability set the handler declares. More than one where it calls `require_all`.
    required: &'static [Permission],
    /// A body that *deserializes*. Same reasoning as the query parameters: `Json` extraction
    /// runs before the handler, so a malformed body would return `422` and never reach the
    /// permission check.
    body: fn() -> Option<Value>,
}

/// No request body.
fn empty() -> Option<Value> {
    None
}

/// Every permission-gated endpoint under `/v1/admin`, with the capability it declares.
///
/// Kept as data rather than as one test per route so the three legs are applied uniformly —
/// a route that only ever gets its happy path tested is exactly how an authorization hole
/// survives review.
///
/// One long table by design: splitting it per module would make it possible for a whole module
/// to fall out of the matrix without anything noticing.
#[allow(clippy::too_many_lines)]
fn admin_gates() -> Vec<Gate> {
    vec![
        // --- feature flags ---
        Gate {
            method: "GET",
            template: "/v1/admin/feature-flags",
            path: "/v1/admin/feature-flags",
            required: &[Permission::FlagsRead],
            body: empty,
        },
        Gate {
            method: "PUT",
            template: "/v1/admin/feature-flags/{key}",
            path: "/v1/admin/feature-flags/catalogue.browse",
            required: &[Permission::FlagsWrite],
            // `enabled: true` is the shipped default, so the authorized leg writes an override
            // that changes nothing observable for the rest of the matrix.
            body: || Some(json!({ "enabled": true })),
        },
        Gate {
            method: "DELETE",
            template: "/v1/admin/feature-flags/{key}",
            path: "/v1/admin/feature-flags/catalogue.browse",
            required: &[Permission::FlagsWrite],
            body: empty,
        },
        // --- merge queue ---
        Gate {
            method: "GET",
            template: "/v1/admin/merge-candidates",
            path: "/v1/admin/merge-candidates",
            required: &[Permission::MergeRead],
            body: empty,
        },
        Gate {
            method: "POST",
            template: "/v1/admin/series/merge",
            path: "/v1/admin/series/merge",
            required: &[Permission::MergeWrite],
            body: || Some(json!({ "keep": ABSENT_A, "merge": ABSENT_B })),
        },
        Gate {
            method: "POST",
            template: "/v1/admin/merge-candidates/dismiss",
            path: "/v1/admin/merge-candidates/dismiss",
            required: &[Permission::MergeWrite],
            body: || Some(json!({ "id": ABSENT_A })),
        },
        // --- privacy queue ---
        Gate {
            method: "GET",
            template: "/v1/admin/privacy/requests",
            path: "/v1/admin/privacy/requests",
            required: &[Permission::PrivacyRead],
            body: empty,
        },
        Gate {
            method: "POST",
            template: "/v1/admin/privacy/requests/{id}/claim",
            path: "/v1/admin/privacy/requests/00000000-0000-7000-8000-00000000000a/claim",
            required: &[Permission::PrivacyWrite],
            body: empty,
        },
        Gate {
            method: "POST",
            template: "/v1/admin/privacy/requests/{id}/resolve",
            path: "/v1/admin/privacy/requests/00000000-0000-7000-8000-00000000000a/resolve",
            required: &[Permission::PrivacyWrite],
            body: || Some(json!({ "status": "completed", "note": "matrix" })),
        },
        Gate {
            method: "POST",
            template: "/v1/admin/privacy/requests/{id}/extend",
            path: "/v1/admin/privacy/requests/00000000-0000-7000-8000-00000000000a/extend",
            required: &[Permission::PrivacyWrite],
            body: || {
                Some(json!({
                    "due_at": "2099-01-01T00:00:00Z",
                    "reason": "matrix",
                }))
            },
        },
        Gate {
            method: "GET",
            template: "/v1/admin/privacy/requests/{id}/export",
            path: "/v1/admin/privacy/requests/00000000-0000-7000-8000-00000000000a/export",
            // Disclosing another person's whole record is deliberately a *separate* capability
            // from working the queue. If this ever collapses into `privacy.write`, leg 2 fails.
            required: &[Permission::PrivacyExport],
            body: empty,
        },
        Gate {
            method: "POST",
            template: "/v1/admin/privacy/requests/{id}/fulfil-erasure",
            path: "/v1/admin/privacy/requests/00000000-0000-7000-8000-00000000000a/fulfil-erasure",
            // The one action needing two authorities: working the queue *and* being able to
            // destroy an account. Leg 2 runs twice here, once per withheld capability.
            required: &[Permission::PrivacyWrite, Permission::UsersDelete],
            body: || Some(json!({ "confirm_username": "nobody" })),
        },
        // --- providers ---
        Gate {
            method: "GET",
            template: "/v1/admin/providers",
            path: "/v1/admin/providers",
            required: &[Permission::ProvidersRead],
            body: empty,
        },
        Gate {
            method: "POST",
            template: "/v1/admin/providers",
            path: "/v1/admin/providers",
            required: &[Permission::ProvidersCreate],
            // A loopback literal: the SSRF guard rejects it from its range table without a DNS
            // lookup, so the authorized leg reaches a deterministic `400` and the suite makes
            // no network call.
            body: || {
                Some(json!({
                    "slug": "matrix-probe",
                    "name": "Matrix Probe",
                    "base_url": "https://127.0.0.1/",
                    "adapter": "generic_config",
                }))
            },
        },
        Gate {
            method: "PATCH",
            template: "/v1/admin/providers/{id}",
            path: "/v1/admin/providers/00000000-0000-7000-8000-00000000000a",
            required: &[Permission::ProvidersWrite],
            body: || {
                Some(json!({
                    "name": "Matrix Probe",
                    "base_url": "https://127.0.0.1/",
                }))
            },
        },
        Gate {
            method: "DELETE",
            template: "/v1/admin/providers/{id}",
            path: "/v1/admin/providers/00000000-0000-7000-8000-00000000000a",
            required: &[Permission::ProvidersDelete],
            body: empty,
        },
        Gate {
            method: "POST",
            template: "/v1/admin/providers/{id}/state",
            path: "/v1/admin/providers/00000000-0000-7000-8000-00000000000a/state",
            required: &[Permission::ProvidersState],
            body: || Some(json!({ "state": "disabled" })),
        },
        Gate {
            method: "POST",
            template: "/v1/admin/providers/{id}/resolve",
            path: "/v1/admin/providers/00000000-0000-7000-8000-00000000000a/resolve",
            required: &[Permission::ProvidersTest],
            body: empty,
        },
        Gate {
            method: "GET",
            template: "/v1/admin/providers/stats",
            path: "/v1/admin/providers/stats",
            required: &[Permission::ProvidersRead],
            body: empty,
        },
        Gate {
            method: "POST",
            template: "/v1/admin/providers/{id}/test",
            path: "/v1/admin/providers/00000000-0000-7000-8000-00000000000a/test",
            required: &[Permission::ProvidersTest],
            body: empty,
        },
        // --- scans ---
        Gate {
            method: "POST",
            template: "/v1/admin/scans",
            path: "/v1/admin/scans",
            required: &[Permission::ScansRun],
            body: || Some(json!({ "mode": "fast" })),
        },
        Gate {
            method: "GET",
            template: "/v1/admin/scans",
            path: "/v1/admin/scans",
            required: &[Permission::ScansRead],
            body: empty,
        },
        Gate {
            method: "GET",
            template: "/v1/admin/scans/{run_id}",
            path: "/v1/admin/scans/00000000-0000-7000-8000-00000000000a",
            required: &[Permission::ScansRead],
            body: empty,
        },
        Gate {
            method: "GET",
            template: "/v1/admin/scan-failures",
            path: "/v1/admin/scan-failures",
            required: &[Permission::ScansRead],
            body: empty,
        },
        Gate {
            method: "GET",
            template: "/v1/admin/scans/stream",
            path: "/v1/admin/scans/stream",
            required: &[Permission::ScansRead],
            body: empty,
        },
        // --- external sync administration ---
        Gate {
            method: "GET",
            template: "/v1/admin/sync/accounts",
            path: "/v1/admin/sync/accounts",
            required: &[Permission::SyncAdminRead],
            body: empty,
        },
        Gate {
            method: "GET",
            template: "/v1/admin/sync/mappings",
            path: "/v1/admin/sync/mappings",
            required: &[Permission::SyncAdminRead],
            body: empty,
        },
        Gate {
            method: "POST",
            template: "/v1/admin/sync/mappings",
            path: "/v1/admin/sync/mappings",
            required: &[Permission::SyncAdminWrite],
            body: || {
                Some(json!({
                    "series_id": ABSENT_A,
                    "provider": "anilist",
                    "external_id": "1",
                }))
            },
        },
        Gate {
            method: "POST",
            template: "/v1/admin/sync/pull",
            path: "/v1/admin/sync/pull",
            required: &[Permission::SyncAdminWrite],
            body: || Some(json!({ "user_id": ABSENT_A, "provider": "anilist" })),
        },
        Gate {
            method: "POST",
            template: "/v1/admin/sync/push",
            path: "/v1/admin/sync/push",
            required: &[Permission::SyncAdminWrite],
            body: || Some(json!({ "user_id": ABSENT_A, "provider": "anilist" })),
        },
        Gate {
            method: "POST",
            template: "/v1/admin/sync/unlink",
            path: "/v1/admin/sync/unlink",
            required: &[Permission::SyncAdminWrite],
            body: || Some(json!({ "user_id": ABSENT_A, "provider": "anilist" })),
        },
        Gate {
            method: "POST",
            template: "/v1/admin/sync/mappings/clear",
            path: "/v1/admin/sync/mappings/clear",
            required: &[Permission::SyncAdminWrite],
            body: || Some(json!({ "series_id": ABSENT_A, "provider": "anilist" })),
        },
        Gate {
            method: "GET",
            template: "/v1/admin/sync/series/{id}",
            path: "/v1/admin/sync/series/00000000-0000-7000-8000-00000000000a",
            required: &[Permission::SyncAdminRead],
            body: empty,
        },
        Gate {
            method: "GET",
            template: "/v1/admin/sync/unmapped",
            path: "/v1/admin/sync/unmapped?provider=anilist",
            required: &[Permission::SyncAdminRead],
            body: empty,
        },
        Gate {
            method: "GET",
            template: "/v1/admin/sync/unmatched",
            path: "/v1/admin/sync/unmatched?provider=anilist",
            required: &[Permission::SyncAdminRead],
            body: empty,
        },
        Gate {
            method: "GET",
            template: "/v1/admin/sync/suggest",
            path: "/v1/admin/sync/suggest?title=matrix",
            required: &[Permission::SyncAdminRead],
            body: empty,
        },
        Gate {
            method: "POST",
            template: "/v1/admin/sync/assign",
            path: "/v1/admin/sync/assign",
            required: &[Permission::SyncAdminWrite],
            body: || {
                Some(json!({
                    "user_id": ABSENT_A,
                    "provider": "anilist",
                    "external_id": "1",
                    "series_id": ABSENT_B,
                }))
            },
        },
        // --- observability ---
        Gate {
            method: "GET",
            template: "/v1/admin/stats",
            path: "/v1/admin/stats",
            required: &[Permission::SystemStats],
            body: empty,
        },
        Gate {
            method: "GET",
            template: "/v1/admin/audit",
            path: "/v1/admin/audit",
            required: &[Permission::AuditRead],
            body: empty,
        },
        // --- user administration ---
        Gate {
            method: "GET",
            template: "/v1/admin/users",
            path: "/v1/admin/users",
            required: &[Permission::UsersRead],
            body: empty,
        },
        Gate {
            method: "GET",
            template: "/v1/admin/users/{id}",
            path: "/v1/admin/users/00000000-0000-7000-8000-00000000000a",
            required: &[Permission::UsersRead],
            body: empty,
        },
        Gate {
            method: "PATCH",
            template: "/v1/admin/users/{id}",
            path: "/v1/admin/users/00000000-0000-7000-8000-00000000000a",
            required: &[Permission::UsersWrite],
            body: || Some(json!({})),
        },
        Gate {
            method: "DELETE",
            template: "/v1/admin/users/{id}",
            path: "/v1/admin/users/00000000-0000-7000-8000-00000000000a",
            required: &[Permission::UsersDelete],
            body: || Some(json!({ "confirm_username": "nobody" })),
        },
        Gate {
            method: "POST",
            template: "/v1/admin/users/{id}/status",
            path: "/v1/admin/users/00000000-0000-7000-8000-00000000000a/status",
            required: &[Permission::UsersWrite],
            body: || Some(json!({ "status": "suspended", "reason": "matrix" })),
        },
        Gate {
            method: "PUT",
            template: "/v1/admin/users/{id}/permissions",
            // The meta-capability: a holder can escalate anyone, including themselves. It must
            // never be implied by `users.write`, which leg 2 is what proves.
            path: "/v1/admin/users/00000000-0000-7000-8000-00000000000a/permissions",
            required: &[Permission::UsersPermissions],
            body: || Some(json!({ "permissions": [] })),
        },
        Gate {
            method: "POST",
            template: "/v1/admin/users/{id}/revoke-sessions",
            path: "/v1/admin/users/00000000-0000-7000-8000-00000000000a/revoke-sessions",
            required: &[Permission::UsersSessions],
            body: empty,
        },
        Gate {
            method: "POST",
            template: "/v1/admin/users/{id}/verify-email",
            path: "/v1/admin/users/00000000-0000-7000-8000-00000000000a/verify-email",
            required: &[Permission::UsersWrite],
            body: empty,
        },
        Gate {
            method: "GET",
            template: "/v1/admin/permissions",
            path: "/v1/admin/permissions",
            required: &[Permission::UsersRead],
            body: empty,
        },
    ]
}

/// Mints (and caches) bearer tokens for a given capability set so the matrix seeds ~25 accounts
/// rather than one per leg per route.
struct Callers<'a> {
    app: &'a TestApp,
    cache: HashMap<Vec<Permission>, String>,
    next: usize,
}

impl<'a> Callers<'a> {
    fn new(app: &'a TestApp) -> Self {
        Self {
            app,
            cache: HashMap::new(),
            next: 0,
        }
    }

    /// A bearer for an active account holding exactly `perms`.
    async fn holding(&mut self, perms: &[Permission]) -> String {
        let mut key = perms.to_vec();
        key.sort_unstable();
        key.dedup();
        if let Some(bearer) = self.cache.get(&key) {
            return bearer.clone();
        }
        let username = format!("matrix{:03}", self.next);
        self.next += 1;
        let user = self
            .app
            .seed_user(&username, &key, AccountStatus::Active)
            .await;
        let bearer = self.app.bearer(user);
        self.cache.insert(key, bearer.clone());
        bearer
    }

    /// A bearer for an active account holding every capability *except* `withheld`.
    async fn holding_all_but(&mut self, withheld: &[Permission]) -> String {
        let perms: Vec<Permission> = Permission::all()
            .iter()
            .copied()
            .filter(|p| !withheld.contains(p))
            .collect();
        self.holding(&perms).await
    }
}

/// Drive one request and return its status **without draining the body**.
///
/// `/v1/admin/scans/stream` answers with an open Server-Sent-Events stream that never ends;
/// reading its body to completion — which `TestApp::call` does — would hang the suite forever.
/// The matrix only ever asserts on the status line, so the body is dropped unread.
async fn status_of(
    app: &TestApp,
    method: &str,
    path: &str,
    bearer: Option<&str>,
    body: Option<Value>,
) -> StatusCode {
    let mut builder = Request::builder().method(method).uri(path);
    if let Some(bearer) = bearer {
        builder = builder.header(header::AUTHORIZATION, bearer);
    }
    let request = match body {
        Some(json) => builder
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(serde_json::to_vec(&json).expect("serialize")))
            .expect("build request"),
        None => builder.body(Body::empty()).expect("build request"),
    };
    app.request(request).await.status()
}

#[tokio::test]
async fn no_admin_endpoint_answers_without_a_session() {
    let app = TestApp::spawn_with(TestConfig::new().without_rate_limiting()).await;

    for gate in admin_gates() {
        let status = status_of(&app, gate.method, gate.path, None, (gate.body)()).await;
        assert_eq!(
            status,
            StatusCode::UNAUTHORIZED,
            "{} {} without a token must be 401",
            gate.method,
            gate.template
        );
    }
}

#[tokio::test]
async fn every_admin_endpoint_refuses_a_caller_missing_exactly_its_own_capability() {
    let app = TestApp::spawn_with(TestConfig::new().without_rate_limiting()).await;
    let mut callers = Callers::new(&app);

    for gate in admin_gates() {
        // One pass per declared capability: for a two-capability handler, withholding either
        // one alone must still refuse. Withholding both at once would not distinguish a
        // handler that checks only the first.
        for withheld in gate.required {
            let bearer = callers.holding_all_but(&[*withheld]).await;
            let before = app.audit.denials().len();

            let status =
                status_of(&app, gate.method, gate.path, Some(&bearer), (gate.body)()).await;
            assert_eq!(
                status,
                StatusCode::FORBIDDEN,
                "{} {} must be 403 for a caller holding every capability but {withheld}",
                gate.method,
                gate.template
            );

            // A refusal that leaves no trace is half a control: the audit record is what tells
            // an incident responder which capability was reached for.
            let denials = app.audit.denials();
            assert_eq!(
                denials.len(),
                before + 1,
                "{} {} must audit exactly one denial",
                gate.method,
                gate.template
            );
            let event = denials.last().expect("a denial was just recorded");
            assert_eq!(event.action, "authz.denied");
            let missing = event.detail["missing"]
                .as_array()
                .expect("the denial names what was missing");
            assert!(
                missing.iter().any(|m| m == withheld.as_str()),
                "{} {} denial must name {withheld}, got {:?}",
                gate.method,
                gate.template,
                event.detail
            );
        }
    }
}

#[tokio::test]
async fn every_admin_endpoint_admits_a_caller_holding_exactly_its_capability() {
    let app = TestApp::spawn_with(TestConfig::new().without_rate_limiting()).await;
    let mut callers = Callers::new(&app);

    for gate in admin_gates() {
        let bearer = callers.holding(gate.required).await;
        let status = status_of(&app, gate.method, gate.path, Some(&bearer), (gate.body)()).await;

        // Deliberately not `is_success()`: these requests name absent rows and unreachable
        // upstreams on purpose, so `404`/`409`/`502` are correct outcomes. What must never
        // happen is the authorization layer refusing a caller that holds precisely what the
        // handler declares — that would mean the declared capability is not the one enforced.
        assert!(
            status != StatusCode::UNAUTHORIZED && status != StatusCode::FORBIDDEN,
            "{} {} must admit a caller holding {:?}, got {status}",
            gate.method,
            gate.template,
            gate.required
        );
    }
}

#[tokio::test]
async fn the_matrix_covers_every_admin_endpoint_in_the_openapi_document() {
    // The published document is the authority on what the service exposes. Reconciling against
    // it — rather than against a hand-maintained list — is what stops this file rotting: adding
    // an admin route without a matrix row turns the build red on the pull request that adds it.
    let spec = serde_json::to_value(tankovault_api::full_openapi()).expect("serialize openapi");
    let paths = spec["paths"].as_object().expect("openapi has paths");

    let mut published: BTreeSet<String> = BTreeSet::new();
    for (path, item) in paths {
        if !path.starts_with("/v1/admin") {
            continue;
        }
        for method in item.as_object().expect("path item is an object").keys() {
            // `parameters`/`summary` and friends sit alongside the operations.
            let upper = method.to_uppercase();
            if matches!(
                upper.as_str(),
                "GET" | "POST" | "PUT" | "PATCH" | "DELETE" | "HEAD" | "OPTIONS"
            ) {
                published.insert(format!("{upper} {path}"));
            }
        }
    }

    let covered: BTreeSet<String> = admin_gates()
        .iter()
        .map(|g| format!("{} {}", g.method, g.template))
        .collect();

    let uncovered: Vec<&String> = published.difference(&covered).collect();
    assert!(
        uncovered.is_empty(),
        "these admin endpoints are published but not in the access-control matrix: {uncovered:?}"
    );

    let stale: Vec<&String> = covered.difference(&published).collect();
    assert!(
        stale.is_empty(),
        "these matrix rows name endpoints the service no longer publishes: {stale:?}"
    );
}
