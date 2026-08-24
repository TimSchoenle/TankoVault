//! Resolves a stored relative path against `provider.base_url` at read time.
//!
//! Every persisted location is relative, which is what makes a provider changing domain a
//! one-row `base_url` update instead of a rewrite of every link under it.

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
    Unresolvable {
        /// The provider base URL the join was attempted against.
        base: String,
        /// The stored path that would not join onto it.
        path: String,
    },
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

/// The prefix a chapter path is stored relative to, for a source at `source_path`.
///
/// Always ends in `/`, which is what makes the stored form unambiguous: a compressed suffix can
/// never begin with `/`, and an uncompressed path always does (every writer goes through
/// `adapters::relativize`, which guarantees the leading slash).
fn source_prefix(source_path: &str) -> String {
    format!("{}/", source_path.trim_end_matches('/'))
}

/// Compress a chapter path for storage under a source at `source_path`.
///
/// `chapters.path` is the largest variable field in the largest table in the deployment, and for
/// most providers it is the series path with a few characters appended —
/// `/manga/<slug>/chapter-1050/` under `/manga/<slug>`. The prefix is already stored, once per
/// source, in a table every chapter query already joins, so storing it again on every chapter row
/// is pure duplication. Measured: mean path length 42.4 → 11.7 characters.
///
/// **Not every provider nests this way.** `MangaDex`'s series path is `/title/{uuid}` and its
/// chapter path is `/chapter/{uuid}` — no shared prefix at all. Those are stored whole, and the
/// leading slash is what says so. [`expand_chapter_path`] is the inverse; the SQL spelling of it
/// is the `chapter_url_path` function in migration 0055, which must stay in step with this.
#[must_use]
pub fn compress_chapter_path(source_path: &str, path: &str) -> String {
    path.strip_prefix(&source_prefix(source_path))
        .map_or_else(|| path.to_owned(), ToOwned::to_owned)
}

/// Expand a stored chapter path back to the site-relative path [`resolve_link`] expects.
///
/// The inverse of [`compress_chapter_path`]. A value beginning with `/` was stored whole.
#[must_use]
pub fn expand_chapter_path(source_path: &str, stored: &str) -> String {
    if stored.starts_with('/') {
        stored.to_owned()
    } else {
        format!("{}{stored}", source_prefix(source_path))
    }
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

    #[test]
    fn a_nesting_provider_stores_only_the_suffix() {
        assert_eq!(
            compress_chapter_path("/manga/solo-leveling", "/manga/solo-leveling/chapter-1050/"),
            "chapter-1050/"
        );
        // A trailing slash on the source path must not change the answer — adapters emit both.
        assert_eq!(
            compress_chapter_path(
                "/manga/solo-leveling/",
                "/manga/solo-leveling/chapter-1050/"
            ),
            "chapter-1050/"
        );
    }

    /// `MangaDex`'s chapter path shares no prefix with its series path (`/title/{uuid}` against
    /// `/chapter/{uuid}`). Compressing against a prefix that does not match must leave the value
    /// alone, and the leading slash it keeps is the only marker that says so.
    #[test]
    fn a_non_nesting_provider_stores_the_path_whole() {
        let stored = compress_chapter_path("/title/abc-123", "/chapter/def-456");
        assert_eq!(stored, "/chapter/def-456");
        assert!(stored.starts_with('/'), "the marker is the leading slash");
    }

    /// A near-miss must not be compressed: `/manga/solo-leveling-2/` is not under
    /// `/manga/solo-leveling/`, and stripping on the bare prefix rather than the slash-terminated
    /// one would silently rewrite one series' chapters as another's.
    #[test]
    fn a_sibling_slug_is_not_treated_as_nested() {
        assert_eq!(
            compress_chapter_path("/manga/solo-leveling", "/manga/solo-leveling-2/chapter-1/"),
            "/manga/solo-leveling-2/chapter-1/"
        );
    }

    #[test]
    fn compression_round_trips_for_both_shapes() {
        for (source, path) in [
            ("/manga/x", "/manga/x/chapter-1/"),
            ("/manga/x/", "/manga/x/chapter-1/"),
            ("/title/abc", "/chapter/def"),
            ("/manga/x", "/manga/x"),
        ] {
            let stored = compress_chapter_path(source, path);
            assert_eq!(
                expand_chapter_path(source, &stored),
                path,
                "{source} {path}"
            );
        }
    }
}
