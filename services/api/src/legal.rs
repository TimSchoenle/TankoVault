//! Operator-supplied legal documents: the index the footer builds its Legal column from, and
//! the documents themselves.
//!
//! **Unauthenticated on purpose.** A reader is owed the Terms and the Data Policy *before* they
//! register, because registering is the act of accepting them; the register form links these,
//! and a link behind a login is not a link.
//!
//! Serving is `terrace_legal`'s: each body is a configuration value, validated into a catalog
//! once per configuration generation, so a request is a map lookup with no file I/O, and an edit
//! to a mounted document is a configuration reload. The two handlers stay here only because the
//! operation tag and identifiers in `openapi.json` are this service's.

use axum::extract::{Path, Query, State};
use axum::http::HeaderMap;
use axum::response::Response;
use terrace_legal::model::{LegalDocumentView, LegalIndexEntry, LegalParams};
use terrace_legal::{Catalog, CatalogBuilder, ConfigIssues, Legal, LegalConfig};
use terrace_legal_axum::{respond_document, respond_index};

use crate::error::{ApiError, ApiResult};
use crate::openapi::LEGAL_TAG;
use crate::state::AppState;

/// Where [`crate::config::Config`] mounts the section, and so the prefix of every key in it.
pub const LEGAL_SECTION: &str = "legal";

/// The rules the `[legal]` section is held to beyond the library's built-in checks.
///
/// The one source for both halves: the runtime validates with it and `config-contract` refines
/// the published `legal` keys with it, so a rule added here is enforced and published from the
/// same value and the two cannot drift. No document is required: an absent section is a valid
/// deployment that publishes no Legal column.
#[must_use]
pub fn legal_rules() -> CatalogBuilder {
    Catalog::builder()
}

/// Validate the operator's `[legal]` section into the documents this runtime serves.
///
/// Warnings (an external document on plain `http`) are logged and do not refuse the section.
///
/// # Errors
/// Every problem in the section at once, keyed as the operator wrote it (`legal.documents.…`).
pub fn legal_documents(config: &LegalConfig) -> Result<Legal, ConfigIssues> {
    let rules = legal_rules();
    let catalog = rules
        .build(config)
        .map_err(|issues| issues.with_prefix(LEGAL_SECTION))?;
    for warning in catalog.warnings() {
        tracing::warn!(key = %format!("{LEGAL_SECTION}.{}", warning.key()), "{}", warning.message());
    }
    Ok(Legal::with_builder(catalog, rules))
}

/// List the legal documents
///
/// Only what this deployment actually publishes. An operator who configures no Imprint gets no
/// Imprint entry, so the footer renders no dead link rather than one that 404s.
#[utoipa::path(
    get,
    path = "/v1/legal",
    tag = LEGAL_TAG,
    params(LegalParams),
    responses(
        (status = 200, description = "The configured documents", body = Vec<LegalIndexEntry>),
    )
)]
pub async fn legal_index(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(params): Query<LegalParams>,
) -> Response {
    respond_index(&state.legal, &headers, params.lang).await
}

/// Get one legal document
///
/// The Markdown as the operator wrote it. Rendering — and sanitising the result — is the
/// client's job: this is operator input, not developer input, so it is never trusted as HTML.
#[utoipa::path(
    get,
    path = "/v1/legal/{slug}",
    tag = LEGAL_TAG,
    params(("slug" = String, Path, description = "Document slug"), LegalParams),
    responses(
        (status = 200, description = "The document in the served locale", body = LegalDocumentView),
        (status = 404, description = "no such document, or one hosted elsewhere", body = crate::error::ProblemDetails),
    )
)]
pub async fn legal_document(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(slug): Path<String>,
    Query(params): Query<LegalParams>,
) -> ApiResult<Response> {
    // Both refusals are a `404` in this service's problem shape: an external document has no
    // body here, and the index already told the client where it lives.
    respond_document(&state.legal, &headers, slug, params.lang)
        .await
        .map_err(|_| ApiError::NotFound)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The section's issues must name the key the operator wrote. Without the prefix a refusal
    /// reads `documents.terms.body.EN`, which is no variable or TOML key anyone can find.
    #[test]
    fn a_refused_section_names_keys_under_legal() {
        let mut config = LegalConfig::default();
        config
            .documents
            .insert("terms".to_owned(), terrace_legal::LegalDocument::default());
        let issues = legal_documents(&config).expect_err("a document with no body and no url");
        let keys: Vec<String> = issues.iter().map(terrace_legal::ConfigIssue::key).collect();
        assert_eq!(keys, ["legal.documents.terms"]);
    }

    /// A rule's refinement must land on the key this binary actually reads. Refining under a
    /// mount that no longer matches the `legal` field is refused by terrace-config, so a renamed
    /// section would fail the contract build instead of publishing an unrefined `documents`.
    #[test]
    fn a_rule_is_published_under_the_section_the_config_reads() {
        let rules = legal_rules().rule(terrace_legal::RequiredDocuments::new(["terms"]));
        let schema = crate::config::published_schema_with(&rules).expect("the mount resolves");
        let key = schema
            .keys
            .iter()
            .find(|key| key.path == "legal.documents")
            .expect("legal.documents is published");
        let constraint = key.constraint.as_ref().expect("a map constraint");

        assert_eq!(constraint["required"], serde_json::json!(["terms"]));
    }

    #[test]
    fn an_absent_section_serves_an_empty_index() {
        let legal = legal_documents(&LegalConfig::default()).expect("valid");
        assert!(legal.catalog().is_empty());
    }
}
