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
/// There is **no exception any more**. `GET /v1/me/stream` used to be listed here by name as the
/// one operation whose credential no `security` scheme could express, because it carried a raw
/// access token in the query string. SEC-8 replaced that with a single-use ticket, and a ticket in
/// a query parameter is precisely an `apiKey`/`in: query` scheme — so the operation declares its
/// requirement like every other private route and the carve-out is gone. Do not add another: an
/// operation that cannot express how it is authenticated is a finding, not a special case.
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
/// Three artefacts have to agree on the string `ticket`: the `apiKey` scheme in
/// `components.securitySchemes`, the operation's own query parameter, and — outside this
/// workspace, so outside any compiler that could check it — `web/frontend/src/api.rs`, which
/// hand-builds the `EventSource` URL because `EventSource` is created by the browser rather than
/// by the generated client.
///
/// That hand-built URL is where a real, shipped bug lived: the frontend sent
/// `?token=…` while the handler required `?access_token=…`, so `Query<StreamQuery>` rejected every
/// connection with `400` and live notifications had never worked. Nothing noticed, because
/// `live.rs` treats a stream failure as a silent best-effort degradation. The frontend carries the
/// matching half of this assertion (`the_stream_url_uses_the_parameter_the_published_document_declares`).
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
