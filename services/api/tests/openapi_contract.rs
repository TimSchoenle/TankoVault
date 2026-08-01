//! Self-consistency checks on the published `OpenAPI` document, not just that it matches the
//! code (`xtask openapi --check`'s job) but that it is internally coherent. Not behind
//! `integration`: needs no database or router, so it runs in the fast CI job.

use std::collections::BTreeSet;

use serde_json::Value;

fn document() -> Value {
    serde_json::to_value(tankovault_api::full_openapi()).expect("serialize openapi")
}

/// Every security requirement names a scheme the document actually defines.
///
/// # The bug this pins
///
/// `DELETE /v1/me` and `GET /v1/me/export` declared `security(("bearer" = []))` while the only
/// defined scheme is `bearer_auth` — an unresolvable name that a generator reads as no
/// requirement, publishing two account-destroying endpoints as needing no authentication.
/// `utoipa` takes the scheme name as a string, so the compiler can't catch this typo.
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
/// A route that forgets this is documented as public even where enforcement is fine
/// (`me_access_matrix.rs` covers that half) — the failure a client author trips over. No
/// exceptions: an operation that can't express how it's authenticated is a finding, not a
/// special case.
#[test]
fn every_private_operation_declares_a_security_requirement() {
    let spec = document();
    let mut undeclared: Vec<String> = Vec::new();
    for (path, item) in spec["paths"].as_object().expect("paths") {
        if !(path.starts_with("/v1/me") || path.starts_with("/v1/admin")) {
            continue;
        }
        for (method, operation) in item.as_object().expect("path item") {
            let op = format!("{} {path}", method.to_uppercase());
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

/// The stream's declared credential is the query parameter the handler actually reads.
///
/// # The bug this pins
///
/// `web/frontend/src/api.rs` hand-builds the `EventSource` URL outside any compiler that checks
/// it against this scheme, and once sent `?token=…` while the handler required `?access_token=…`
/// — every connection rejected with `400`, unnoticed because a stream failure degrades silently.
#[test]
fn the_stream_credential_is_declared_under_the_name_the_handler_reads() {
    let spec = document();

    let scheme = &spec["components"]["securitySchemes"]["stream_ticket"];
    assert_eq!(scheme["type"], "apiKey");
    assert_eq!(scheme["in"], "query");
    assert_eq!(
        scheme["name"], "ticket",
        "the scheme must name the query parameter the handler extracts"
    );

    let operation = &spec["paths"]["/v1/me/stream"]["get"];
    let parameters = operation["parameters"]
        .as_array()
        .expect("the stream declares its query parameters");
    let names: Vec<&str> = parameters
        .iter()
        .filter(|p| p["in"] == "query")
        .filter_map(|p| p["name"].as_str())
        .collect();
    assert_eq!(
        names,
        vec!["ticket"],
        "the stream takes exactly one query credential, named `ticket`"
    );
    assert!(
        parameters.iter().all(|p| p["required"] == true),
        "an optional credential would make the route reachable without one"
    );

    // And the mint endpoint it points at must exist, or the scheme documents a flow no client
    // can start.
    assert!(
        spec["paths"]["/v1/me/stream-ticket"]["post"].is_object(),
        "the ticket scheme is unusable without the endpoint that mints one"
    );
}
