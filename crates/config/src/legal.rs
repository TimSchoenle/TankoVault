//! Operator-supplied legal documents: Terms of Service, Data Policy, Imprint and whatever else
//! a deployment is obliged to publish.
//!
//! # Why this is configuration and not content
//!
//! Every deployment is a different operator under different law. The text changes without a
//! release — a new sub-processor, a moved registered office — and an Imprint is a statutory
//! requirement in some jurisdictions and meaningless in others. Baking any of it into the WASM
//! bundle would make a legal correction a build, and would ship one operator's obligations to
//! all of them.
//!
//! So the shape is deliberately open: [`LegalConfig::documents`] is **slug-keyed**, and an
//! operator can publish a document this build has never heard of (`dmca`, `acceptable_use`)
//! without a code change. The frontend renders whatever the index returns rather than a fixed
//! list, so an operator with no Imprint gets no Imprint link instead of a dead one.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::Deserialize;

use crate::ConfigError;

/// Where the legal documents live and what they are.
///
/// Every field defaults, so an absent `[legal]` section is valid and simply publishes nothing.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct LegalConfig {
    /// Root that relative [`LegalDocument::sources`] paths resolve against.
    ///
    /// Absent means paths are taken as given, which is what makes a single absolute path in an
    /// environment variable work without also setting a root.
    pub dir: Option<PathBuf>,
    /// The published documents, keyed by the slug their URL uses (`terms`, `privacy`,
    /// `imprint`, …).
    ///
    /// A map rather than a struct of known documents on purpose — see the module docs.
    pub documents: BTreeMap<String, LegalDocument>,
}

/// One published document: either files to serve, or somewhere else to send the reader.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct LegalDocument {
    /// Markdown file per locale, keyed by the language code the frontend uses (`en`, `de`).
    ///
    /// A plain `String` key rather than a shipped-locale enum: an operator publishing French
    /// terms should not need this build to know about French.
    pub sources: BTreeMap<String, PathBuf>,
    /// Somewhere else entirely — a corporate imprint already hosted by the operator.
    ///
    /// Mutually exclusive with [`Self::sources`]; see [`LegalConfig::validate`].
    pub url: Option<String>,
    /// The "last updated" line, as the operator wants it shown (`2026-08-04`).
    ///
    /// Free text rather than a date, because it is displayed verbatim and a file mtime is the
    /// wrong answer — touching a file is not amending a policy.
    pub updated: Option<String>,
    /// Per-locale display title. A locale with no entry falls back to the frontend's own
    /// catalogue for the slugs it knows, and to the slug itself for the ones it does not.
    pub title: BTreeMap<String, String>,
}

impl LegalDocument {
    /// Whether this document is served from files rather than pointing elsewhere.
    #[must_use]
    pub fn is_inline(&self) -> bool {
        self.url.is_none()
    }

    /// The locales this document is available in, in code order.
    #[must_use]
    pub fn locales(&self) -> Vec<&str> {
        self.sources.keys().map(String::as_str).collect()
    }

    /// The file backing `locale`, resolved against `dir`, or `None` when that locale is not
    /// published.
    #[must_use]
    pub fn source_for(&self, dir: Option<&Path>, locale: &str) -> Option<PathBuf> {
        let path = self.sources.get(locale)?;
        Some(match dir {
            // An absolute `source` wins over the root: an operator who spelled the whole path
            // meant it, and silently re-rooting it under `dir` would read the wrong file.
            Some(root) if path.is_relative() => root.join(path),
            _ => path.clone(),
        })
    }

    /// Which locale to serve, given what the reader asked for.
    ///
    /// Requested first, then each `Accept-Language` preference in order, then the first
    /// configured locale. The last arm is why the answer states the locale it served: a reader
    /// asking for `de` and receiving the only available English text has to be told so rather
    /// than left to conclude the operator writes German like that.
    #[must_use]
    pub fn resolve_locale(&self, requested: Option<&str>, accepted: &[String]) -> Option<&str> {
        let has = |code: &str| self.sources.contains_key(code).then(|| code.to_owned());
        requested
            .and_then(&has)
            .or_else(|| accepted.iter().find_map(|code| has(code)))
            .and_then(|code| self.sources.keys().find(|k| **k == code))
            .map(String::as_str)
            .or_else(|| self.sources.keys().next().map(String::as_str))
    }
}

impl LegalConfig {
    /// The document at `slug`, if the operator publishes one.
    #[must_use]
    pub fn document(&self, slug: &str) -> Option<&LegalDocument> {
        self.documents.get(slug)
    }

    /// Refuse a document that cannot be served.
    ///
    /// Both cases are a misconfiguration that would otherwise surface as a permanent 404 on a
    /// link the footer publishes — which is worse than not publishing the link, and worse than
    /// failing to boot: an operator who has just written a Data Policy and mistyped the section
    /// name should hear about it now.
    ///
    /// # Errors
    /// [`ConfigError::Invalid`] naming the slug, when a document has neither `sources` nor
    /// `url`, or has both, or a `url` that is not absolute `http`/`https`.
    pub fn validate(&self) -> Result<(), ConfigError> {
        for (slug, doc) in &self.documents {
            match (&doc.url, doc.sources.is_empty()) {
                (None, true) => {
                    return Err(ConfigError::Invalid(format!(
                        "legal.documents.{slug} has neither `sources` nor `url`; give it a \
                         Markdown file per locale, or a `url` to send readers to"
                    )));
                }
                (Some(_), false) => {
                    return Err(ConfigError::Invalid(format!(
                        "legal.documents.{slug} sets both `sources` and `url`; a document is \
                         either served from here or hosted elsewhere, not both"
                    )));
                }
                (Some(url), true) => {
                    if !url.starts_with("http://") && !url.starts_with("https://") {
                        return Err(ConfigError::Invalid(format!(
                            "legal.documents.{slug}.url must be an absolute http(s) URL, got \
                             {url:?}"
                        )));
                    }
                }
                (None, false) => {}
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn inline(locales: &[&str]) -> LegalDocument {
        LegalDocument {
            sources: locales
                .iter()
                .map(|code| {
                    (
                        (*code).to_owned(),
                        PathBuf::from(format!("terms.{code}.md")),
                    )
                })
                .collect(),
            ..LegalDocument::default()
        }
    }

    #[test]
    fn an_empty_section_publishes_nothing_and_is_valid() {
        let cfg = LegalConfig::default();
        assert!(cfg.validate().is_ok());
        assert!(cfg.document("terms").is_none());
    }

    /// A document with no way to serve it is refused at boot rather than 404ing on a link the
    /// footer publishes from the same config that omitted the file.
    #[test]
    fn a_document_with_no_source_and_no_url_is_refused_by_slug() {
        let mut cfg = LegalConfig::default();
        cfg.documents
            .insert("imprint".to_owned(), LegalDocument::default());
        let err = cfg.validate().expect_err("must not boot");
        assert!(format!("{err}").contains("imprint"), "{err}");
    }

    #[test]
    fn a_document_that_is_both_hosted_and_inline_is_refused() {
        let mut cfg = LegalConfig::default();
        cfg.documents.insert(
            "terms".to_owned(),
            LegalDocument {
                url: Some("https://example.org/terms".to_owned()),
                ..inline(&["en"])
            },
        );
        assert!(cfg.validate().is_err());
    }

    #[test]
    fn a_relative_url_is_refused_but_an_absolute_one_is_not() {
        let mut cfg = LegalConfig::default();
        cfg.documents.insert(
            "imprint".to_owned(),
            LegalDocument {
                url: Some("/impressum".to_owned()),
                ..LegalDocument::default()
            },
        );
        assert!(cfg.validate().is_err());

        cfg.documents.insert(
            "imprint".to_owned(),
            LegalDocument {
                url: Some("https://example.org/impressum".to_owned()),
                ..LegalDocument::default()
            },
        );
        assert!(cfg.validate().is_ok());
    }

    /// `dir` is a root for relative paths only. Re-rooting an absolute path under it would read
    /// a file the operator did not name.
    #[test]
    fn dir_roots_relative_sources_and_leaves_absolute_ones_alone() {
        let doc = LegalDocument {
            sources: BTreeMap::from([
                ("en".to_owned(), PathBuf::from("terms.en.md")),
                ("de".to_owned(), PathBuf::from("/etc/other/terms.de.md")),
            ]),
            ..LegalDocument::default()
        };
        let root = Path::new("/etc/tankovault/legal");
        assert_eq!(
            doc.source_for(Some(root), "en"),
            Some(root.join("terms.en.md"))
        );
        assert_eq!(
            doc.source_for(Some(root), "de"),
            Some(PathBuf::from("/etc/other/terms.de.md"))
        );
        assert_eq!(doc.source_for(Some(root), "fr"), None);
    }

    /// The fallback chain is what lets a German reader see the only text there is instead of a
    /// 404 — and why the response has to say which locale it served.
    #[test]
    fn locale_resolution_prefers_the_request_then_the_header_then_what_exists() {
        let doc = inline(&["de", "en"]);
        assert_eq!(doc.resolve_locale(Some("en"), &[]), Some("en"));
        assert_eq!(
            doc.resolve_locale(Some("fr"), &["en".to_owned()]),
            Some("en"),
            "an unavailable request falls through to the header",
        );
        assert_eq!(
            doc.resolve_locale(None, &["fr".to_owned()]),
            Some("de"),
            "nothing matched, so the first configured locale",
        );
        assert_eq!(LegalDocument::default().resolve_locale(None, &[]), None);
    }
}
