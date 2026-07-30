//! Self-consistency checks on the published `OpenAPI` document.
//!
//! Deliberately **not** behind the `integration` feature: these need no database and no
//! router, only `full_openapi()`, so they run in the fast CI job where a broken contract is
//! cheapest to notice.
//!
//! `xtask openapi --check` already guarantees the committed `openapi.json` matches what the
//! code generates. It says nothing about whether that document is *coherent* — and an
//! incoherent one is worse than a stale one, because `crates/api-client` and the frontend
//! are generated from it and will happily generate the wrong thing.

use std::collections::BTreeSet;

use serde_json::Value;

fn document() -> Value {
    serde_json::to_value(tankovault_api::full_openapi()).expect("serialize openapi")
}

/// Every security requirement names a scheme the document actually defines.
///
/// The bug this pins: `DELETE /v1/me` and `GET /v1/me/export` declared
/// `security(("bearer" = []))` while the only scheme in `components.securitySchemes` is
/// `bearer_auth`. `OpenAPI` requires the name to resolve there, so those two operations
/// referenced a scheme that did not exist — and a generator that resolves the reference reads
/// the requirement as absent, i.e. publishes two endpoints that delete an account and export
/// its entire personal record as needing no authentication at all. Both are in fact gated
/// (`me_access_matrix.rs` proves it against the real router); the *document* was the thing
/// that lied, which is the half a client author sees.
///
/// A typo cannot be caught by the compiler here — `utoipa` takes the scheme name as a string —
/// so it has to be caught by this.
#[test]
fn every_security_requirement_names_a_defined_scheme() {
    let spec = document();
    let defined: BTreeSet<&str> = spec["components"]["securitySchemes"]
        .as_object()
        .expect("the document defines security schemes")
        .keys()
        .map(String::as_str)
        .collect();

    let mut unresolved: Vec<String> = Vec::new();
    for (path, item) in spec["paths"].as_object().expect("paths") {
        for (method, operation) in item.as_object().expect("path item") {
            let Some(requirements) = operation.get("security").and_then(Value::as_array) else {
                continue;
            };
            for requirement in requirements {
                for scheme in requirement.as_object().into_iter().flat_map(|r| r.keys()) {
                    if !defined.contains(scheme.as_str()) {
                        unresolved.push(format!("{} {path} → {scheme:?}", method.to_uppercase()));
                    }
                }
            }
        }
    }

    assert!(
        unresolved.is_empty(),
        "these operations require a security scheme the document does not define {defined:?}: \
         {unresolved:?}"
    );
}

/// Every `/v1/me` and `/v1/admin` operation declares that it needs a session.
///
/// The declaration is what the generated client and the published docs are built from, so an
/// authenticated route that forgets it is documented as public. That is a different failure
/// from the route actually being open — `me_access_matrix.rs` covers enforcement — and it is
/// the one a client author trips over.
///
/// `GET /v1/me/stream` is the single exception and is listed by name: it takes its credential
/// in the query string because `EventSource` cannot set headers (SEC-8), which no `security`
/// scheme in this document expresses.
#[test]
fn every_private_operation_declares_a_security_requirement() {
    const QUERY_CREDENTIALLED: &[&str] = &["GET /v1/me/stream"];

    let spec = document();
    let mut undeclared: Vec<String> = Vec::new();
    for (path, item) in spec["paths"].as_object().expect("paths") {
        if !(path.starts_with("/v1/me") || path.starts_with("/v1/admin")) {
            continue;
        }
        for (method, operation) in item.as_object().expect("path item") {
            let op = format!("{} {path}", method.to_uppercase());
            if QUERY_CREDENTIALLED.contains(&op.as_str()) {
                continue;
            }
            let declared = operation
                .get("security")
                .and_then(Value::as_array)
                .is_some_and(|s| !s.is_empty());
            if !declared {
                undeclared.push(op);
            }
        }
    }

    assert!(
        undeclared.is_empty(),
        "these private operations are documented as needing no session: {undeclared:?}"
    );
}
