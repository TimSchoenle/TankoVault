//! The migration-safe link resolver.
//!
//! Every persisted location is a **relative path** on a provider. Absolute URLs are
//! computed at read time from `provider.base_url + path`. This is the single resolver
//! function referenced by the API, worker, and frontend — see design §5.
//!
//! Storing relative paths means a provider domain migration is a one-row
//! `UPDATE providers SET base_url = ...` with zero link rewrites.

use thiserror::Error;
use url::Url;

/// Failure modes when resolving a stored path against a provider base URL.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum ResolveError {
    /// The provider base URL did not parse as an absolute URL.
    #[error("invalid provider base_url: {0}")]
    InvalidBase(String),
    /// The base URL used a scheme other than http/https.
    #[error("unsupported base_url scheme: {0}")]
    UnsupportedScheme(String),
    /// The concatenated result was not a valid URL.
    #[error("could not resolve path {path:?} against base {base:?}")]
    Unresolvable { base: String, path: String },
}

/// Resolve a stored relative `path` against a provider `base_url` into an absolute URL.
///
/// Guarantees:
/// - Trailing slash on the base and leading slash on the path are normalized so no
///   double slash appears at the join and no slash is dropped.
/// - A subpath in the base (`https://host/read`) is preserved.
/// - Only `http`/`https` bases are accepted.
/// - A defensively-stored absolute `http(s)` path is returned as-is (normalized).
///
/// # Errors
/// Returns [`ResolveError`] if the base URL is missing/invalid, uses a non-web scheme,
/// or the join cannot be parsed.
pub fn resolve_link(base_url: &str, path: &str) -> Result<String, ResolveError> {
    // Defensive path: a stored value that is already an absolute web URL passes through.
    if let Ok(parsed) = Url::parse(path)
        && matches!(parsed.scheme(), "http" | "https")
    {
        return Ok(parsed.to_string());
    }

    let base = Url::parse(base_url).map_err(|_| ResolveError::InvalidBase(base_url.to_owned()))?;
    if !matches!(base.scheme(), "http" | "https") {
        return Err(ResolveError::UnsupportedScheme(base.scheme().to_owned()));
    }

    // Preserve any base subpath by concatenating rather than using `Url::join`
    // (which would discard the base path for a root-anchored relative path).
    let trimmed_base = base_url.trim_end_matches('/');
    let normalized_path = if path.starts_with('/') {
        std::borrow::Cow::Borrowed(path)
    } else {
        std::borrow::Cow::Owned(format!("/{path}"))
    };

    let candidate = format!("{trimmed_base}{normalized_path}");
    let resolved = Url::parse(&candidate).map_err(|_| ResolveError::Unresolvable {
        base: base_url.to_owned(),
        path: path.to_owned(),
    })?;
    Ok(resolved.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn joins_without_double_slash() {
        assert_eq!(
            resolve_link("https://example.com/", "/manga/x/chapter-1/").unwrap(),
            "https://example.com/manga/x/chapter-1/"
        );
    }

    #[test]
    fn joins_without_dropping_slash() {
        assert_eq!(
            resolve_link("https://example.com", "/manga/x/").unwrap(),
            "https://example.com/manga/x/"
        );
    }

    #[test]
    fn adds_missing_leading_slash_on_path() {
        assert_eq!(
            resolve_link("https://example.com", "manga/x").unwrap(),
            "https://example.com/manga/x"
        );
    }

    #[test]
    fn preserves_base_subpath() {
        assert_eq!(
            resolve_link("https://example.com/read/", "/series/1").unwrap(),
            "https://example.com/read/series/1"
        );
    }

    #[test]
    fn domain_migration_reresolves_every_link() {
        // The same stored path resolves correctly after a base_url change — no rewrite.
        let path = "/manga/solo-leveling/chapter-1/";
        let old = resolve_link("https://old-domain.test", path).unwrap();
        let new = resolve_link("https://new-domain.test", path).unwrap();
        assert_eq!(
            old,
            "https://old-domain.test/manga/solo-leveling/chapter-1/"
        );
        assert_eq!(
            new,
            "https://new-domain.test/manga/solo-leveling/chapter-1/"
        );
    }

    #[test]
    fn rejects_non_web_base_scheme() {
        let err = resolve_link("ftp://example.com", "/x").unwrap_err();
        assert!(matches!(err, ResolveError::UnsupportedScheme(_)));
    }

    #[test]
    fn rejects_unparseable_base() {
        let err = resolve_link("not a url", "/x").unwrap_err();
        assert!(matches!(err, ResolveError::InvalidBase(_)));
    }

    #[test]
    fn absolute_stored_path_passes_through() {
        assert_eq!(
            resolve_link("https://example.com", "https://cdn.example.com/cover.jpg").unwrap(),
            "https://cdn.example.com/cover.jpg"
        );
    }
}
