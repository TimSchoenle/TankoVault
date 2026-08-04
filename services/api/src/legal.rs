//! Operator-supplied legal documents: the index the footer builds its Legal column from, and
//! the documents themselves.
//!
//! **Unauthenticated on purpose.** A reader is owed the Terms and the Data Policy *before* they
//! register, because registering is the act of accepting them; the register form links these,
//! and a link behind a login is not a link.
//!
//! Files are read on demand behind an mtime check rather than slurped at boot, so correcting a
//! policy is an edit and not a restart. A file that disappears degrades to `404`, logged once.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, RwLock};
use std::time::SystemTime;

use axum::Json;
use axum::extract::{Path, Query, State};
use axum::http::HeaderMap;
use axum::http::header::{ACCEPT_LANGUAGE, CACHE_CONTROL, ETAG};
use axum::response::{IntoResponse, Response};
use serde::{Deserialize, Serialize};
use tankovault_config::{LegalConfig, LegalDocument};
use utoipa::{IntoParams, ToSchema};

use crate::error::{ApiError, ApiResult};
use crate::openapi::LEGAL_TAG;
use crate::state::AppState;

/// How long a client may reuse a legal document without revalidating.
///
/// Five minutes: these are the cheapest documents the API serves and the footer asks for the
/// index on every page load, but an operator who has just corrected a policy should not have to
/// wait an hour for readers to see it. `public` because the documents are identical for every
/// reader — nothing here is per-session — so a shared cache may hold one copy.
const CACHE_POLICY: &str = "public, max-age=300";

/// Largest document served inline.
///
/// A guard against a mis-pointed `source` — an operator who aims a document at a log file
/// should get a `500` naming the slug, not a 2GB response body.
const MAX_DOCUMENT_BYTES: u64 = 1024 * 1024;

/// The legal documents, read through an mtime check.
///
/// Held in [`AppState`] so the cache is per process rather than per request. The key is
/// `(slug, locale)`; the value is the text and the mtime it was read at, so a `stat` decides
/// whether to re-read rather than a timer.
#[derive(Clone)]
pub struct LegalDocs {
    config: Arc<LegalConfig>,
    /// `RwLock` rather than a lock-free map: reads hugely dominate, the critical section is a
    /// clone of an `Arc<str>`, and a poisoned lock here should not take the process down — see
    /// [`LegalDocs::cached`].
    cache: Arc<RwLock<HashMap<(String, String), Cached>>>,
}

#[derive(Clone)]
struct Cached {
    text: Arc<str>,
    /// Content hash, for the `ETag`. Kept beside the text so a conditional request costs a
    /// `stat` and nothing else.
    etag: String,
    modified: Option<SystemTime>,
}

impl LegalDocs {
    #[must_use]
    pub fn new(config: LegalConfig) -> Self {
        Self {
            config: Arc::new(config),
            cache: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    #[must_use]
    pub fn config(&self) -> &LegalConfig {
        &self.config
    }

    /// The cached entry for `key`, if it is still current for `modified`.
    ///
    /// A poisoned lock is treated as an empty cache: the only thing behind it is a copy of a
    /// file on disk, so the correct recovery is to read the file again, not to panic every
    /// subsequent request on a route that exists to serve a privacy policy.
    fn cached(&self, key: &(String, String), modified: Option<SystemTime>) -> Option<Cached> {
        let guard = self.cache.read().ok()?;
        let hit = guard.get(key)?;
        (hit.modified == modified).then(|| hit.clone())
    }

    fn store(&self, key: (String, String), value: Cached) {
        if let Ok(mut guard) = self.cache.write() {
            guard.insert(key, value);
        }
    }

    /// Read `path`, serving the cached copy when its mtime has not moved.
    fn read(&self, key: (String, String), path: &PathBuf) -> ApiResult<Cached> {
        let metadata = std::fs::metadata(path).map_err(|e| {
            // The path is operator configuration, not reader input, so naming it is a
            // deployment aid rather than a disclosure — but the reader gets a plain 404.
            tracing::warn!(slug = %key.0, locale = %key.1, path = %path.display(), error = %e,
                           "configured legal document is unreadable");
            ApiError::NotFound
        })?;
        if metadata.len() > MAX_DOCUMENT_BYTES {
            tracing::error!(slug = %key.0, path = %path.display(), bytes = metadata.len(),
                            "legal document exceeds the inline size cap");
            return Err(ApiError::NotFound);
        }
        let modified = metadata.modified().ok();
        if let Some(hit) = self.cached(&key, modified) {
            return Ok(hit);
        }

        let text = std::fs::read_to_string(path).map_err(|e| {
            tracing::warn!(slug = %key.0, path = %path.display(), error = %e,
                           "legal document could not be read");
            ApiError::NotFound
        })?;
        let entry = Cached {
            etag: etag_of(&text),
            text: Arc::from(text.as_str()),
            modified,
        };
        self.store(key, entry.clone());
        Ok(entry)
    }
}

/// A weak `ETag` derived from the content.
///
/// Weak, and a plain hash rather than a cryptographic digest: this identifies a revision for
/// caching, it does not authenticate one. `FxHash`-style folding over `DefaultHasher` is
/// deliberate — the value never leaves this process's responses and is compared for equality
/// only.
fn etag_of(text: &str) -> String {
    use std::hash::{Hash as _, Hasher as _};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    text.hash(&mut hasher);
    format!("W/\"{:016x}\"", hasher.finish())
}

/// One entry in the index the footer renders.
#[derive(Debug, Serialize, ToSchema)]
pub struct LegalIndexEntry {
    /// The URL slug, and the key an operator configured this document under.
    pub slug: String,
    /// The operator's title for the requested locale, when they set one.
    pub title: Option<String>,
    /// The "last updated" line, verbatim as configured.
    pub updated: Option<String>,
    /// `inline` for a document this API serves, `external` for one hosted elsewhere.
    pub kind: LegalKind,
    /// Where to send the reader, for an `external` document.
    pub url: Option<String>,
    /// The locales an `inline` document is published in.
    pub locales: Vec<String>,
}

/// Whether a document is served from here or hosted elsewhere.
#[derive(Debug, Clone, Copy, Serialize, ToSchema)]
#[serde(rename_all = "lowercase")]
pub enum LegalKind {
    Inline,
    External,
}

/// One document, in the locale that was actually served.
#[derive(Debug, Serialize, ToSchema)]
pub struct LegalDocumentView {
    pub slug: String,
    /// The locale served, which is **not** necessarily the one requested — a reader asking for
    /// German and receiving the only available English text has to be told, or they conclude
    /// the operator writes German like that.
    pub locale: String,
    pub title: Option<String>,
    pub updated: Option<String>,
    /// Always `markdown`. Present so a future format is a value rather than a new endpoint.
    pub format: String,
    pub body: String,
}

/// Which locale the caller wants.
#[derive(Debug, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct LegalParams {
    /// Language code (`en`, `de`). Falls back to `Accept-Language`, then to the first locale
    /// the operator configured.
    #[serde(default)]
    pub lang: Option<String>,
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
    let accepted = accepted_languages(&headers);
    let docs = state.legal.config();
    let entries: Vec<LegalIndexEntry> = docs
        .documents
        .iter()
        .map(|(slug, doc)| {
            let locale = doc.resolve_locale(params.lang.as_deref(), &accepted);
            LegalIndexEntry {
                slug: slug.clone(),
                title: title_for(doc, params.lang.as_deref(), &accepted, locale),
                updated: doc.updated.clone(),
                kind: if doc.is_inline() {
                    LegalKind::Inline
                } else {
                    LegalKind::External
                },
                url: doc.url.clone(),
                locales: doc.locales().into_iter().map(str::to_owned).collect(),
            }
        })
        .collect();

    // The index changes only when the operator edits the config, which is a restart — but it
    // is still requested on every page load, so it carries the same short freshness window as
    // the documents rather than none.
    ([(CACHE_CONTROL, CACHE_POLICY)], Json(entries)).into_response()
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
        (status = 404, description = "no such document, or its file is unreadable", body = crate::error::ProblemDetails),
    )
)]
pub async fn legal_document(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(slug): Path<String>,
    Query(params): Query<LegalParams>,
) -> ApiResult<Response> {
    let accepted = accepted_languages(&headers);
    let doc = state
        .legal
        .config()
        .document(&slug)
        .ok_or(ApiError::NotFound)?;
    // An external document has no body to serve; the index already told the client where to go,
    // and inventing a redirect here would hide that from a client that only reads this route.
    let locale = doc
        .resolve_locale(params.lang.as_deref(), &accepted)
        .ok_or(ApiError::NotFound)?
        .to_owned();
    let path = doc
        .source_for(state.legal.config().dir.as_deref(), &locale)
        .ok_or(ApiError::NotFound)?;

    let cached = state.legal.read((slug.clone(), locale.clone()), &path)?;
    let view = LegalDocumentView {
        title: title_for(doc, params.lang.as_deref(), &accepted, Some(&locale)),
        slug,
        locale,
        updated: doc.updated.clone(),
        format: "markdown".to_owned(),
        body: cached.text.to_string(),
    };
    Ok((
        [
            (ETAG, cached.etag),
            (CACHE_CONTROL, CACHE_POLICY.to_owned()),
        ],
        Json(view),
    )
        .into_response())
}

/// The operator's title for the locale that will be served, falling back through the same chain
/// the body does, then to `None` — at which point the client names the slug from its own
/// catalogue.
fn title_for(
    doc: &LegalDocument,
    requested: Option<&str>,
    accepted: &[String],
    served: Option<&str>,
) -> Option<String> {
    requested
        .and_then(|code| doc.title.get(code))
        .or_else(|| accepted.iter().find_map(|code| doc.title.get(code)))
        .or_else(|| served.and_then(|code| doc.title.get(code)))
        .or_else(|| doc.title.values().next())
        .cloned()
}

/// The language codes in `Accept-Language`, most-preferred first.
///
/// Primary subtags only: a reader whose browser reports `de-AT` is served `de` rather than being
/// bounced to English over a regional suffix nobody configured a document for. `q` values are
/// parsed rather than assumed — a header listing `en;q=0.2, de;q=0.9` means German.
fn accepted_languages(headers: &HeaderMap) -> Vec<String> {
    let Some(raw) = headers.get(ACCEPT_LANGUAGE).and_then(|v| v.to_ok_str()) else {
        return Vec::new();
    };
    let mut ranked: Vec<(f32, String)> = raw
        .split(',')
        .filter_map(|part| {
            let mut bits = part.split(';');
            let tag = bits.next()?.trim();
            if tag.is_empty() || tag == "*" {
                return None;
            }
            let quality = bits
                .find_map(|bit| bit.trim().strip_prefix("q=")?.parse::<f32>().ok())
                .unwrap_or(1.0);
            let primary = tag.split(['-', '_']).next()?.to_ascii_lowercase();
            Some((quality, primary))
        })
        .collect();
    // Stable, so equal-quality tags keep the order the client listed them in.
    ranked.sort_by(|a, b| b.0.total_cmp(&a.0));
    let mut seen = Vec::new();
    for (_, code) in ranked {
        if !seen.contains(&code) {
            seen.push(code);
        }
    }
    seen
}

/// `to_str`, named for what it means here: a header with non-ASCII bytes is a header we ignore.
trait HeaderValueExt {
    fn to_ok_str(&self) -> Option<&str>;
}

impl HeaderValueExt for axum::http::HeaderValue {
    fn to_ok_str(&self) -> Option<&str> {
        self.to_str().ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::HeaderValue;

    fn headers(value: &str) -> HeaderMap {
        let mut map = HeaderMap::new();
        map.insert(ACCEPT_LANGUAGE, HeaderValue::from_str(value).unwrap());
        map
    }

    /// `q` values decide the order. Reading the header positionally would serve English to a
    /// browser that spelled out it wants German.
    #[test]
    fn accept_language_is_ordered_by_quality_not_by_position() {
        assert_eq!(
            accepted_languages(&headers("en;q=0.2, de;q=0.9")),
            vec!["de".to_owned(), "en".to_owned()]
        );
    }

    /// A regional suffix nobody configures a document for must not cost the reader their
    /// language.
    #[test]
    fn a_regional_tag_matches_its_primary_subtag() {
        assert_eq!(
            accepted_languages(&headers("de-AT, en-GB;q=0.5")),
            vec!["de".to_owned(), "en".to_owned()]
        );
    }

    #[test]
    fn a_missing_or_wildcard_header_expresses_no_preference() {
        assert!(accepted_languages(&HeaderMap::new()).is_empty());
        assert!(accepted_languages(&headers("*")).is_empty());
    }

    /// The same file, read twice, must not be read twice — and must be re-read once its mtime
    /// moves, which is what lets an operator correct a policy without a restart.
    #[test]
    fn a_document_is_cached_until_its_mtime_moves() {
        let dir = std::env::temp_dir().join(format!("tv-legal-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("scratch dir");
        let path = dir.join("terms.en.md");
        std::fs::write(&path, "first").expect("write");

        let docs = LegalDocs::new(LegalConfig::default());
        let key = ("terms".to_owned(), "en".to_owned());
        let first = docs.read(key.clone(), &path).expect("read");
        assert_eq!(&*first.text, "first");
        assert!(
            docs.cached(&key, first.modified).is_some(),
            "the second request must not touch the file's contents again",
        );

        // A new mtime is what invalidates it; the filesystem's resolution is coarse enough
        // that the write alone is not a reliable signal, so it is set explicitly.
        std::fs::write(&path, "second").expect("rewrite");
        let bumped = SystemTime::now() + std::time::Duration::from_secs(2);
        // Opened for **writing**: a read-only handle cannot set a file's timestamps on Windows,
        // which fails as `PermissionDenied` rather than as anything about mtimes.
        std::fs::OpenOptions::new()
            .write(true)
            .open(&path)
            .expect("open")
            .set_modified(bumped)
            .expect("touch");
        let second = docs.read(key, &path).expect("re-read");
        assert_eq!(&*second.text, "second", "an edit must not need a restart");
        assert_ne!(first.etag, second.etag, "the ETag follows the content");

        std::fs::remove_dir_all(&dir).ok();
    }

    /// A configured document whose file is gone is a 404, never a panic: the operator's mount
    /// failing must not take the API down.
    #[test]
    fn a_missing_file_is_a_not_found_rather_than_a_panic() {
        let docs = LegalDocs::new(LegalConfig::default());
        let missing = std::env::temp_dir().join("tv-legal-does-not-exist.md");
        assert!(matches!(
            docs.read(("terms".to_owned(), "en".to_owned()), &missing),
            Err(ApiError::NotFound)
        ));
    }
}
