//! HTML extraction helpers shared by the config-driven adapters.
//!
//! Selector syntax: a plain CSS selector takes the element's inner text; a `sel@attr`
//! suffix takes an attribute instead (design §7). All parsing here is deterministic and
//! unit-tested so a provider markup change fails a fixture test, not production silently.

use crate::error::AdapterError;
use tankovault_domain::SeriesStatus;
use scraper::{ElementRef, Selector};
use time::OffsetDateTime;
use time::macros::format_description;
use url::Url;

/// Parse a CSS selector, mapping failures to a typed error.
///
/// # Errors
/// [`AdapterError::Selector`] if `spec` is not a valid selector.
pub fn parse_selector(spec: &str) -> Result<Selector, AdapterError> {
    Selector::parse(spec).map_err(|e| AdapterError::Selector {
        selector: spec.to_owned(),
        reason: e.to_string(),
    })
}

/// Split a `sel@attr` spec into `(selector, Some(attr))`, or `(selector, None)`.
#[must_use]
pub fn split_attr(spec: &str) -> (&str, Option<&str>) {
    match spec.rsplit_once('@') {
        Some((sel, attr)) if !attr.contains([' ', '>', '.', '#', '[']) => (sel, Some(attr)),
        _ => (spec, None),
    }
}

fn value_of(el: ElementRef<'_>, attr: Option<&str>) -> String {
    match attr {
        Some(a) => el.value().attr(a).unwrap_or_default().trim().to_owned(),
        None => text_of(el),
    }
}

/// Collapse an element's inner text to a single, whitespace-normalised line.
#[must_use]
pub fn text_of(el: ElementRef<'_>) -> String {
    el.text()
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

/// Extract the first match of `spec` under `root` (text or `@attr`).
///
/// # Errors
/// [`AdapterError::Selector`] on an invalid selector.
pub fn extract_first(root: ElementRef<'_>, spec: &str) -> Result<Option<String>, AdapterError> {
    let (sel_str, attr) = split_attr(spec);
    let sel = parse_selector(sel_str)?;
    Ok(root
        .select(&sel)
        .next()
        .map(|el| value_of(el, attr))
        .filter(|s| !s.is_empty()))
}

/// Extract all non-empty matches of `spec` under `root`.
///
/// # Errors
/// [`AdapterError::Selector`] on an invalid selector.
pub fn extract_all(root: ElementRef<'_>, spec: &str) -> Result<Vec<String>, AdapterError> {
    let (sel_str, attr) = split_attr(spec);
    let sel = parse_selector(sel_str)?;
    Ok(root
        .select(&sel)
        .map(|el| value_of(el, attr))
        .filter(|s| !s.is_empty())
        .collect())
}

/// Parse the first numeric run in `text` (e.g. `"10.5"` from `"Chapter 10.5"`).
#[must_use]
pub fn parse_number(text: &str) -> Option<f64> {
    let mut num = String::new();
    let mut seen_dot = false;
    let mut started = false;
    for c in text.chars() {
        if c.is_ascii_digit() {
            num.push(c);
            started = true;
        } else if c == '.' && started && !seen_dot {
            num.push(c);
            seen_dot = true;
        } else if started {
            break;
        }
    }
    num.trim_end_matches('.').parse::<f64>().ok()
}

/// Parse a bare release year (e.g. `"2025"`) from extracted text, discarding anything
/// outside a representable `i32` range rather than silently wrapping.
#[must_use]
#[allow(clippy::cast_possible_truncation)]
pub fn parse_year(text: &str) -> Option<i32> {
    let y = parse_number(text)?;
    if y.is_finite() && y >= f64::from(i32::MIN) && y <= f64::from(i32::MAX) {
        Some(y as i32)
    } else {
        None
    }
}

/// Parse a chapter number from listing text, preferring the number after a
/// chapter/episode marker so `"Volume 2 Chapter 10.5"` yields `10.5`, not `2`.
#[must_use]
pub fn parse_chapter_number(text: &str) -> Option<f64> {
    let lower = text.to_lowercase();
    for marker in ["chapter", "episode", "chap", "ch.", "ch ", "#"] {
        if let Some(idx) = lower.find(marker) {
            let tail = &text[idx + marker.len()..];
            if let Some(n) = parse_number(tail) {
                return Some(n);
            }
        }
    }
    parse_number(text)
}

/// Resolve `href` against the absolute `page_url` and reduce it to a **relative** path
/// (`/path?query`) suitable for storage. Handles absolute, root-relative, and
/// document-relative hrefs.
#[must_use]
pub fn relativize(page_url: &str, href: &str) -> String {
    if let Ok(base) = Url::parse(page_url) {
        if let Ok(joined) = base.join(href.trim()) {
            let mut path = joined.path().to_owned();
            if let Some(q) = joined.query() {
                path.push('?');
                path.push_str(q);
            }
            return path;
        }
    }
    if href.starts_with('/') {
        href.to_owned()
    } else {
        format!("/{href}")
    }
}

/// Resolve `href` against the absolute `page_url` into an **absolute** URL string.
///
/// Unlike [`relativize`], this preserves the host: an already-absolute href (e.g. a cover
/// hosted on a separate CDN) passes through unchanged, while a relative one resolves
/// against the page. Falls back to the trimmed `href` if `page_url` is unparseable. Used
/// for values consumed directly by clients (covers), not for stored links.
#[must_use]
pub fn absolutize(page_url: &str, href: &str) -> String {
    Url::parse(page_url)
        .ok()
        .and_then(|base| base.join(href.trim()).ok())
        .map_or_else(|| href.trim().to_owned(), String::from)
}

/// Map a free-text provider status label to a normalised [`SeriesStatus`].
///
/// Matching is substring-based and case-insensitive, so wording variants
/// (ongoing/publishing, completed, hiatus, dropped/cancelled) all classify correctly.
#[must_use]
pub fn map_status(text: &str) -> SeriesStatus {
    let t = text.to_lowercase();
    if t.contains("ongoing") || t.contains("publishing") {
        SeriesStatus::Ongoing
    } else if t.contains("complete") {
        SeriesStatus::Completed
    } else if t.contains("hiatus") {
        SeriesStatus::Hiatus
    } else if t.contains("cancel") || t.contains("drop") {
        SeriesStatus::Cancelled
    } else {
        SeriesStatus::Unknown
    }
}

/// Parse an ISO `YYYY-MM-DD` date (a common chapter release-date shape) into an
/// [`OffsetDateTime`] at midnight UTC. Returns `None` for any other shape, so relative
/// labels ("7 hours ago") or empty cells leave the date simply unset.
#[must_use]
pub fn parse_ymd_date(text: &str) -> Option<OffsetDateTime> {
    let fmt = format_description!("[year]-[month]-[day]");
    time::Date::parse(text.trim(), &fmt)
        .ok()
        .map(|d| d.midnight().assume_utc())
}

#[cfg(test)]
mod tests {
    use super::*;
    use scraper::Html;

    #[test]
    fn splits_attr_suffix() {
        assert_eq!(split_attr("div img@src"), ("div img", Some("src")));
        assert_eq!(split_attr("a.next"), ("a.next", None));
        // An email-like value in an attribute selector must not be misread.
        assert_eq!(split_attr("a[href]"), ("a[href]", None));
    }

    #[test]
    fn extracts_text_and_attr() {
        let doc = Html::parse_document(
            r#"<div class="p"><h1>Solo Leveling</h1><img class="c" src="/cover.jpg"></div>"#,
        );
        let root = doc.root_element();
        assert_eq!(
            extract_first(root, "div.p h1").unwrap().as_deref(),
            Some("Solo Leveling")
        );
        assert_eq!(
            extract_first(root, "img.c@src").unwrap().as_deref(),
            Some("/cover.jpg")
        );
    }

    #[test]
    fn parses_numbers() {
        assert_eq!(parse_number("Chapter 10.5"), Some(10.5));
        assert_eq!(parse_number("Ch 1024"), Some(1024.0));
        assert_eq!(parse_number("no digits"), None);
    }

    #[test]
    fn chapter_number_prefers_marker() {
        assert_eq!(parse_chapter_number("Volume 2 Chapter 10.5"), Some(10.5));
        assert_eq!(parse_chapter_number("Episode 7"), Some(7.0));
        assert_eq!(parse_chapter_number("1024.1 - The End"), Some(1024.1));
    }

    #[test]
    fn relativizes_all_href_shapes() {
        let page = "https://prov.test/manga/x/";
        assert_eq!(
            relativize(page, "https://prov.test/manga/x/ch-1"),
            "/manga/x/ch-1"
        );
        assert_eq!(relativize(page, "/manga/x/ch-2"), "/manga/x/ch-2");
        assert_eq!(relativize(page, "ch-3"), "/manga/x/ch-3");
    }

    #[test]
    fn absolutizes_preserving_cdn_host() {
        let page = "https://prov.test/manga/x/";
        // An off-host CDN cover keeps its own host.
        assert_eq!(
            absolutize(page, "https://cdn.other.test/c.jpg"),
            "https://cdn.other.test/c.jpg"
        );
        // A document-relative cover resolves against the page host.
        assert_eq!(
            absolutize(page, "/uploads/c.jpg"),
            "https://prov.test/uploads/c.jpg"
        );
    }

    #[test]
    fn maps_status_variants() {
        assert_eq!(map_status("OnGoing"), SeriesStatus::Ongoing);
        assert_eq!(map_status("Publishing"), SeriesStatus::Ongoing);
        assert_eq!(map_status("Completed"), SeriesStatus::Completed);
        assert_eq!(map_status("On Hiatus"), SeriesStatus::Hiatus);
        assert_eq!(map_status("Dropped"), SeriesStatus::Cancelled);
        assert_eq!(map_status("weird"), SeriesStatus::Unknown);
    }

    #[test]
    fn parses_iso_dates_only() {
        let d = parse_ymd_date("2026-07-16").expect("iso date parses");
        assert_eq!(d.year(), 2026);
        assert_eq!(d.month() as u8, 7);
        assert_eq!(d.day(), 16);
        // Surrounding whitespace is tolerated; other shapes are rejected.
        assert!(parse_ymd_date("  2026-01-02 ").is_some());
        assert!(parse_ymd_date("7 hours ago").is_none());
        assert!(parse_ymd_date("").is_none());
    }
}
