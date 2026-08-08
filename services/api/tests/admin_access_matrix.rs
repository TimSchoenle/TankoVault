//! The complete admin access-control matrix: every `/v1/admin` endpoint driven anonymous, holding
//! every permission but the required one(s) (403, audited), and holding exactly it (not
//! 401/403), reconciled against the published `OpenAPI` document. Gated behind the `integration`
//! feature; requires Docker.
#![cfg(feature = "integration")]

use std::collections::{BTreeSet, HashMap};

use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use serde_json::{Value, json};
use tankovault_api_test_support::{TestApp, TestConfig};
use tankovault_domain::{AccountStatus, Permission};

/// A syntactically valid UUID that names nothing, so the leg-3 outcome is a clean `404` rather
/// than a mutation of seeded state.
const ABSENT_A: &str = "00000000-0000-7000-8000-00000000000a";
const ABSENT_B: &str = "00000000-0000-7000-8000-00000000000b";

/// One permission-gated admin endpoint, as the matrix drives it.
struct Gate {
    /// Uppercase HTTP method.
    method: &'static str,
    /// The `OpenAPI` path template (`/v1/admin/users/{id}`), used to reconcile the matrix
    /// against the published document.
    template: &'static str,
    /// Concrete path with parameters filled in. Mandatory query parameters must be present even
    /// on the `403` legs, or `Query` extraction would 400 before the authorization check runs.
    path: &'static str,
    /// The capability set the handler declares. More than one where it calls `require_all`.
    required: &'static [Permission],
    /// A body that *deserializes*, so `Json` extraction doesn't 422 before the permission check.
    body: fn() -> Option<Value>,
}

/// No request body.
fn empty() -> Option<Value> {
    None
}

/// Every permission-gated endpoint under `/v1/admin`, with the capability it declares.
///
/// Kept as one data table rather than one test per route, and not split per module, so every
/// route gets all three legs and none can fall out of the matrix unnoticed.
#[expect(
    clippy::too_many_lines,
    reason = "the matrix is one endpoint per line; splitting it would hide what it covers"
)]
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
            // The shipped default, so this write changes nothing observable elsewhere.
            body: || Some(json!({ "enabled": true })),
        },
        Gate {
            method: "DELETE",
            template: "/v1/admin/feature-flags/{key}",
            path: "/v1/admin/feature-flags/catalogue.browse",
            required: &[Permission::FlagsWrite],
            body: empty,
        },
        // --- the recommender's control plane ---
        Gate {
            method: "GET",
            template: "/v1/admin/recommendations/health",
            path: "/v1/admin/recommendations/health",
            required: &[Permission::RecsysRead],
            body: empty,
        },
        Gate {
            method: "GET",
            template: "/v1/admin/recommendations/tunables",
            path: "/v1/admin/recommendations/tunables",
            required: &[Permission::RecsysRead],
            body: empty,
        },
        Gate {
            method: "PUT",
            template: "/v1/admin/recommendations/tunables/{key}",
            path: "/v1/admin/recommendations/tunables/recsys.diversity.lambda",
            required: &[Permission::RecsysWrite],
            // The shipped default, so this write changes nothing observable elsewhere.
            body: || Some(json!({ "value": 0.7 })),
        },
        Gate {
            method: "DELETE",
            template: "/v1/admin/recommendations/tunables/{key}",
            path: "/v1/admin/recommendations/tunables/recsys.diversity.lambda",
            required: &[Permission::RecsysWrite],
            body: empty,
        },
        Gate {
            method: "POST",
            template: "/v1/admin/recommendations/rebuild",
            path: "/v1/admin/recommendations/rebuild",
            required: &[Permission::RecsysWrite],
            // Forwarded to a control plane the harness points at `.invalid`, so the leg-3
            // outcome is a `502` — after the permission check the matrix is here to assert.
            body: || Some(json!({ "mode": "incremental" })),
        },
        // --- catalogue maintenance ---
        Gate {
            method: "GET",
            template: "/v1/admin/catalogue/series",
            path: "/v1/admin/catalogue/series",
            required: &[Permission::CatalogueRead],
            body: empty,
        },
        Gate {
            method: "GET",
            template: "/v1/admin/catalogue/summary",
            path: "/v1/admin/catalogue/summary",
            required: &[Permission::CatalogueRead],
            body: empty,
        },
        Gate {
            method: "POST",
            template: "/v1/admin/catalogue/series/delete",
            path: "/v1/admin/catalogue/series/delete",
            // Deliberately not `merge.write`, which also removes series: leg 2 fails if the two
            // ever collapse into one capability.
            required: &[Permission::CatalogueDelete],
            body: || Some(json!({ "series_ids": [ABSENT_A] })),
        },
        Gate {
            method: "POST",
            template: "/v1/admin/catalogue/purge",
            path: "/v1/admin/catalogue/purge",
            required: &[Permission::CatalogueDelete],
            // A confirmation that does not echo the scope, so the leg-3 outcome is a clean `400`
            // instead of this matrix emptying the fixture catalogue out from under the suite.
            body: || Some(json!({ "scope": "everything", "confirm": "no" })),
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
        Gate {
            method: "POST",
            template: "/v1/admin/merge-candidates/sweep",
            path: "/v1/admin/merge-candidates/sweep",
            required: &[Permission::MergeWrite],
            // Forwarded to a control plane the harness points at `.invalid`, so the leg-3
            // outcome is a `502` — after the permission check the matrix is here to assert.
            body: empty,
        },
        Gate {
            method: "POST",
            template: "/v1/admin/matching/rebuild-keys",
            path: "/v1/admin/matching/rebuild-keys",
            required: &[Permission::MergeWrite],
            body: empty,
        },
        // --- merge decision journal ---
        // Reading the journal and reversing it are separate capabilities on purpose: the revert
        // is the only action in the system that resurrects a deleted series.
        Gate {
            method: "GET",
            template: "/v1/admin/merge-decisions",
            path: "/v1/admin/merge-decisions",
            required: &[Permission::MergeAudit],
            body: empty,
        },
        Gate {
            method: "POST",
            template: "/v1/admin/merge-decisions/{id}/revert",
            path: "/v1/admin/merge-decisions/00000000-0000-0000-0000-0000000000aa/revert",
            required: &[Permission::MergeRevert],
            body: || Some(json!({ "reason": "not the same work" })),
        },
        Gate {
            method: "POST",
            template: "/v1/admin/merge-decisions/{id}/flag",
            path: "/v1/admin/merge-decisions/00000000-0000-0000-0000-0000000000aa/flag",
            required: &[Permission::MergeRevert],
            body: || Some(json!({ "reason": "not the same work" })),
        },
        // --- sync decision journal ---
        Gate {
            method: "GET",
            template: "/v1/admin/sync/decisions",
            path: "/v1/admin/sync/decisions",
            required: &[Permission::SyncAudit],
            body: empty,
        },
        Gate {
            method: "POST",
            template: "/v1/admin/sync/decisions/{id}/revert",
            path: "/v1/admin/sync/decisions/00000000-0000-0000-0000-0000000000aa/revert",
            required: &[Permission::SyncRevert],
            // Forwarded to a sync service the harness points at `.invalid`, so the leg-3
            // outcome is a `502` — after the permission check the matrix is here to assert.
            body: || Some(json!({ "reason": "wrong match" })),
        },
        Gate {
            method: "POST",
            template: "/v1/admin/sync/decisions/{id}/flag",
            path: "/v1/admin/sync/decisions/00000000-0000-0000-0000-0000000000aa/flag",
            required: &[Permission::SyncRevert],
            body: || Some(json!({ "reason": "wrong match", "block_match": true })),
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
            // Deliberately a separate capability from `privacy.write`; leg 2 fails if it collapses.
            required: &[Permission::PrivacyExport],
            body: empty,
        },
        Gate {
            method: "POST",
            template: "/v1/admin/privacy/requests/{id}/fulfil-erasure",
            path: "/v1/admin/privacy/requests/00000000-0000-7000-8000-00000000000a/fulfil-erasure",
            // The one action needing two authorities; leg 2 runs once per withheld capability.
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
            // Loopback literal: the SSRF guard rejects it without a DNS lookup, so this is a
            // deterministic `400` and the suite makes no network call.
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
            template: "/v1/admin/scan-failures/grouped",
            path: "/v1/admin/scan-failures/grouped",
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
            template: "/v1/admin/sync/enrichment",
            path: "/v1/admin/sync/enrichment",
            required: &[Permission::SyncAdminRead],
            body: empty,
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
        Gate {
            method: "GET",
            template: "/v1/admin/audit/actions",
            path: "/v1/admin/audit/actions",
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
            // The meta-capability, letting a holder escalate anyone; must never be implied by
            // `users.write`, which leg 2 proves.
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

/// An administrative caller: a session, and the elevation every mutating route now demands.
///
/// Both are needed on every leg because the authorization funnel checks three things in order —
/// the account holds a second factor, a mutating capability was elevated, and the grants are
/// present — and a matrix that only supplied the third would fail every route on the first two
/// with a `403` that says nothing about the capability under test.
#[derive(Clone)]
struct Caller {
    bearer: String,
    step_up: String,
}

/// Mints (and caches) callers for a given capability set so the matrix seeds ~25 accounts
/// rather than one per leg per route.
///
/// Every seeded account is enrolled with an authenticator app and holds a live step-up. That is
/// not incidental scaffolding: an administrator *without* a second factor is refused before any
/// capability is consulted (`AuthUser::require_all`), which is a rule `mfa.rs` pins and this
/// matrix must satisfy rather than re-test.
struct Callers<'a> {
    app: &'a TestApp,
    cache: HashMap<Vec<Permission>, Caller>,
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

    /// An enrolled, elevated account holding exactly `perms`.
    async fn holding(&mut self, perms: &[Permission]) -> Caller {
        let mut key = perms.to_vec();
        key.sort_unstable();
        key.dedup();
        if let Some(caller) = self.cache.get(&key) {
            return caller.clone();
        }
        let username = format!("matrix{:03}", self.next);
        self.next += 1;
        let user = self
            .app
            .seed_user(&username, &key, AccountStatus::Active)
            .await;
        let caller = Caller {
            bearer: self.app.bearer(user),
            step_up: self.app.enrolled_and_elevated(user).await,
        };
        self.cache.insert(key, caller.clone());
        caller
    }

    /// An enrolled, elevated account holding every capability *except* `withheld`.
    ///
    /// `grantable()`, not `all()`: seeding the super user grant would answer every check and
    /// turn every expected 403 in the matrix into a pass.
    async fn holding_all_but(&mut self, withheld: &[Permission]) -> Caller {
        let perms: Vec<Permission> = Permission::grantable()
            .into_iter()
            .filter(|p| !withheld.contains(p))
            .collect();
        self.holding(&perms).await
    }
}

/// Drive one request and return its status **without draining the body** — `/v1/admin/scans/stream`
/// is an open SSE stream that never ends, and `TestApp::call` drains fully, which would hang.
async fn status_of(
    app: &TestApp,
    method: &str,
    path: &str,
    caller: Option<&Caller>,
    body: Option<Value>,
) -> StatusCode {
    response_of(app, method, path, caller, body, Elevation::Presented)
        .await
        .status()
}

/// Whether a leg presents the grant its caller holds.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Elevation {
    Presented,
    /// The caller is enrolled and authorized but has not confirmed itself — the state every
    /// administrator is in before the console's prompt.
    Withheld,
}

/// Drive one request. See [`status_of`] for why the body is left undrained.
async fn response_of(
    app: &TestApp,
    method: &str,
    path: &str,
    caller: Option<&Caller>,
    body: Option<Value>,
    elevation: Elevation,
) -> axum::response::Response {
    let mut builder = Request::builder().method(method).uri(path);
    if let Some(caller) = caller {
        builder = builder.header(header::AUTHORIZATION, &caller.bearer);
        if elevation == Elevation::Presented {
            builder = builder.header(tankovault_api::STEP_UP_HEADER, &caller.step_up);
        }
    }
    let request = match body {
        Some(json) => builder
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(serde_json::to_vec(&json).expect("serialize")))
            .expect("build request"),
        None => builder.body(Body::empty()).expect("build request"),
    };
    app.request(request).await
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
        // One pass per declared capability: withholding both at once wouldn't catch a handler
        // that only checks the first.
        for withheld in gate.required {
            let caller = callers.holding_all_but(&[*withheld]).await;
            let before = app.audit.denials().len();

            let status =
                status_of(&app, gate.method, gate.path, Some(&caller), (gate.body)()).await;
            assert_eq!(
                status,
                StatusCode::FORBIDDEN,
                "{} {} must be 403 for a caller holding every capability but {withheld}",
                gate.method,
                gate.template
            );

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
        let caller = callers.holding(gate.required).await;
        let status = status_of(&app, gate.method, gate.path, Some(&caller), (gate.body)()).await;

        // Deliberately not `is_success()`: these requests name absent rows on purpose, so
        // `404`/`409`/`502` are correct. Only 401/403 would mean the wrong capability is enforced.
        assert!(
            status != StatusCode::UNAUTHORIZED && status != StatusCode::FORBIDDEN,
            "{} {} must admit a caller holding {:?}, got {status}",
            gate.method,
            gate.template,
            gate.required
        );
    }
}

/// Every administrative **write** demands a confirmed identity; every read does not.
///
/// The line is `Permission::is_mutating`, applied in `AuthUser::require_all`, so the whole
/// surface inherits it from one place — which is exactly why it is worth sweeping the whole
/// surface. A handler that resolves its capabilities some other way (as `/v1/me/stream` once
/// did) escapes the funnel and every other leg of this matrix still passes: it holds a
/// capability check, it just no longer holds the elevation one. The console's most destructive
/// calls — erase a user, revert a merge, grant permissions — would then need nothing but a
/// stolen token from an operator who was already signed in.
///
/// `mfa.rs` pins the rule on one write and one read; this pins that no route slipped out of it.
/// Reads are asserted in the same pass because the exemption is the tempting thing to widen:
/// prompting to load a dashboard keeps a standing elevation open all day, which is worse than
/// not prompting at all.
#[tokio::test]
async fn every_administrative_write_demands_a_confirmed_identity() {
    let app = TestApp::spawn_with(TestConfig::new().without_rate_limiting()).await;
    let mut callers = Callers::new(&app);

    for gate in admin_gates() {
        let caller = callers.holding(gate.required).await;
        let mutating = gate.required.iter().any(|p| p.is_mutating());
        let response = response_of(
            &app,
            gate.method,
            gate.path,
            Some(&caller),
            (gate.body)(),
            Elevation::Withheld,
        )
        .await;

        let status = response.status();
        if !mutating {
            // Undrained: `/v1/admin/scans/stream` is an SSE stream that never ends.
            assert!(
                status != StatusCode::UNAUTHORIZED && status != StatusCode::FORBIDDEN,
                "{} {} is a read and must not demand an elevation, got {status}",
                gate.method,
                gate.template
            );
            continue;
        }

        assert_eq!(
            status,
            StatusCode::FORBIDDEN,
            "{} {} is a write and must refuse a caller who has not confirmed themselves",
            gate.method,
            gate.template
        );
        // The type, not just the status: `403` is also how a missing capability is refused, and
        // this caller holds every capability the route asked for.
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("read body");
        let body: Value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
        assert_eq!(
            body["title"], "step_up_required",
            "{} {} must name the step-up as the reason, got {body}",
            gate.method, gate.template
        );
    }
}

/// Admin endpoints deliberately outside this file, each with the reason and where they *are*
/// covered.
///
/// The reconciliation test consults this list, so "not in the matrix" is always a decision
/// somebody wrote down rather than an omission.
fn covered_elsewhere() -> Vec<(&'static str, &'static str)> {
    vec![(
        // Ticket-authenticated, not bearer: every leg of this matrix sends an `Authorization`
        // header and no `?ticket=`, so all three would 401 for the same uninteresting reason
        // and prove nothing about the permission check.
        "GET /v1/admin/stream",
        "the_console_stream_* tests below, driven with real tickets",
    )]
}

/// A bearer token does not open the console stream, and a session alone does not either.
///
/// The credential rides in the query string, so this route skips `AuthUser` entirely — which
/// means its permission check is hand-written rather than inherited, and hand-written checks
/// are the ones a refactor drops. `403` is the signal that redemption succeeded and
/// authorization then refused: a redeemed ticket proves a session existed, never authority.
#[tokio::test]
async fn the_console_stream_refuses_a_session_that_holds_none_of_its_permissions() {
    let app = TestApp::spawn_with(TestConfig::new().without_rate_limiting()).await;
    let user = app
        .seed_user(
            "streamdenied",
            &[Permission::FlagsRead],
            AccountStatus::Active,
        )
        .await;

    let bearer_only = status_of(
        &app,
        "GET",
        "/v1/admin/stream",
        Some(&Caller {
            bearer: app.bearer(user),
            step_up: app.enrolled_and_elevated(user).await,
        }),
        None,
    )
    .await;
    assert_eq!(
        bearer_only,
        StatusCode::BAD_REQUEST,
        "no `?ticket=` at all must fail extraction, never fall through to an open stream"
    );

    let ticket = app.stream_ticket(user).await;
    let refused = status_of(
        &app,
        "GET",
        &format!("/v1/admin/stream?ticket={ticket}"),
        None,
        None,
    )
    .await;
    assert_eq!(
        refused,
        StatusCode::FORBIDDEN,
        "`flags.read` carries neither payload this stream pushes, so it opens nothing"
    );
}

/// A ticket minted for an operator who may read scan runs opens the stream.
#[tokio::test]
async fn the_console_stream_opens_for_a_permitted_operator() {
    let app = TestApp::spawn_with(TestConfig::new().without_rate_limiting()).await;
    let user = app
        .seed_user("streamok", &[Permission::ScansRead], AccountStatus::Active)
        .await;
    let ticket = app.stream_ticket(user).await;

    let opened = status_of(
        &app,
        "GET",
        &format!("/v1/admin/stream?ticket={ticket}"),
        None,
        None,
    )
    .await;
    assert_eq!(opened, StatusCode::OK);
}

#[tokio::test]
async fn the_matrix_covers_every_admin_endpoint_in_the_openapi_document() {
    // Reconciling against the published document, not a hand-maintained list, is what stops
    // this file rotting as routes are added.
    let spec = serde_json::to_value(tankovault_api::full_openapi()).expect("serialize openapi");
    let paths = spec["paths"].as_object().expect("openapi has paths");

    let mut published: BTreeSet<String> = BTreeSet::new();
    for (path, item) in paths {
        if !path.starts_with("/v1/admin") {
            continue;
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

    let mut covered: BTreeSet<String> = admin_gates()
        .iter()
        .map(|g| format!("{} {}", g.method, g.template))
        .collect();
    covered.extend(covered_elsewhere().iter().map(|(op, _)| (*op).to_owned()));

    let uncovered: Vec<&String> = published.difference(&covered).collect();
    assert!(
        uncovered.is_empty(),
        "these admin endpoints are published but no access-control matrix classifies them — \
         add a row to admin_gates(), or to covered_elsewhere() with the reason: {uncovered:?}"
    );

    let stale: Vec<&String> = covered.difference(&published).collect();
    assert!(
        stale.is_empty(),
        "these matrix rows name endpoints the service no longer publishes: {stale:?}"
    );
}
