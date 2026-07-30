//! HTML extraction helpers shared by the config-driven adapters.
//!
//! Selector syntax: a plain CSS selector takes the element's inner text; a `sel@attr`
//! suffix takes an attribute instead (design §7). All parsing here is deterministic and
//! unit-tested so a provider markup change fails a fixture test, not production silently.

use crate::error::AdapterError;
use scraper::{ElementRef, Selector};
use std::collections::HashMap;
use std::sync::{Arc, LazyLock, RwLock};
use tankovault_domain::SeriesStatus;
use tankovault_fetch::FetchResponse;
use time::OffsetDateTime;
use time::macros::format_description;
use url::Url;

/// Compiled selectors, keyed by their source text.
///
/// Selectors come from `providers.config`, not from constants, so `LazyLock<Selector>` cannot
/// be used — but the *set* of them is tiny and fixed for a deployment (a handful per provider
/// row), which is exactly the shape a memo fits.
///
/// Without this, `Selector::parse` ran on every call of every extractor, and the extractors
/// are called inside per-item loops: a 100-item catalogue page cost 200 re-parses of two
/// constant strings, and a sitemap-shard page — kunmanga yields up to 20 000 entries in one
/// page — cost 40 000, which is tens to hundreds of milliseconds of pure re-tokenising per
/// page, on every page of every scan.
static SELECTOR_CACHE: LazyLock<RwLock<HashMap<String, Arc<Selector>>>> =
    LazyLock::new(|| RwLock::new(HashMap::new()));

/// Upper bound on distinct cached selectors.
///
/// The legitimate population is bounded by the provider table, so this is not a working-set
/// limit but a guard against a pathological config turning a memo into a leak. On overflow the
/// cache stops growing and later selectors are simply parsed each time — slower, never wrong.
const SELECTOR_CACHE_CAP: usize = 4096;

/// Parse a CSS selector, mapping failures to a typed error. Memoised — see [`SELECTOR_CACHE`].
///
/// Returns an `Arc` rather than a `Selector` so a cache hit is a refcount bump. Call sites are
/// unchanged: `root.select(&sel)` coerces through the `Arc`.
///
/// # Errors
/// [`AdapterError::Selector`] if `spec` is not a valid selector.
pub fn parse_selector(spec: &str) -> Result<Arc<Selector>, AdapterError> {
    if let Ok(cache) = SELECTOR_CACHE.read() {
        if let Some(hit) = cache.get(spec) {
            return Ok(Arc::clone(hit));
        }
    }

    let parsed = Arc::new(Selector::parse(spec).map_err(|e| AdapterError::Selector {
        selector: spec.to_owned(),
        reason: e.to_string(),
    })?);

    if let Ok(mut cache) = SELECTOR_CACHE.write() {
        if cache.len() < SELECTOR_CACHE_CAP {
            cache.insert(spec.to_owned(), Arc::clone(&parsed));
        }
    }
    Ok(parsed)
}

/// Parse a response body as HTML and run `extract` over it on the blocking thread pool.
///
/// `Html::parse_document` is html5ever's full tokenise + tree-build: 5-50 ms of
/// **uninterruptible** CPU for a 500 KB-2 MB catalogue page. Run inline on a Tokio worker
/// thread, as every adapter did, that thread serves no other task for the whole window — and
/// with several large pages parsing concurrently on a runtime sized to core count, *every*
/// async task stalls, including the `JetStream` ack heartbeats the queue module is careful to
/// keep on time.
///
/// The whole parse-and-extract phase has to move together, because `scraper::Html` and
/// `ElementRef` are not `Send`: nothing borrowed from the document may cross back out, so the
/// closure returns owned data. It also receives the [`FetchResponse`] — the diagnostics in
/// [`AdapterError::missing`] and the challenge detectors need the envelope, and it is right
/// there.
///
/// # Errors
/// Whatever `extract` returns, or [`AdapterError::Parse`] if the blocking task panicked.
pub async fn parse_blocking<T, F>(resp: FetchResponse, extract: F) -> Result<T, AdapterError>
where
    T: Send + 'static,
    F: FnOnce(ElementRef<'_>, &FetchResponse) -> Result<T, AdapterError> + Send + 'static,
{
    tokio::task::spawn_blocking(move || {
        let doc = scraper::Html::parse_document(&resp.body);
        extract(doc.root_element(), &resp)
    })
    .await
    // A join error here means the blocking task panicked (or the runtime is shutting down).
    // `Parse` rather than a new variant: from the caller's point of view the document could
    // not be turned into data, which is what `Parse` means, and it is correctly classified as
    // non-retryable — a panic will reproduce on replay.
    .map_err(|e| AdapterError::Parse(format!("HTML parse task failed: {e}")))?
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
///
/// Only ever yields a **finite** value. A digit run too long for `f64` parses to `inf` rather
/// than failing, and an infinite chapter number is not a large chapter number — it is a value
/// that cannot be compared, ordered or serialised. See the regression test
/// `an_unrepresentable_digit_run_is_no_number_at_all`.
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
    let value = num.trim_end_matches('.').parse::<f64>().ok()?;
    value.is_finite().then_some(value)
}

/// Parse a bare release year (e.g. `"2025"`) from extracted text, discarding anything
/// outside a representable `i32` range rather than silently wrapping.
#[must_use]
#[allow(clippy::cast_possible_truncation)]
pub fn parse_year(text: &str) -> Option<i32> {
    let y = parse_number(text)?;
    // `is_finite` is kept although [`parse_number`] now guarantees it: this range check is
    // what makes the cast below sound, and reading the guard next to the cast is the point.
    // It was *also* the only finiteness check in this module for a while, which is how
    // `parse_chapter_number` came to return `inf` — see that function's regression note.
    if y.is_finite() && y >= f64::from(i32::MIN) && y <= f64::from(i32::MAX) {
        Some(y as i32)
    } else {
        None
    }
}

/// Parse a chapter number from listing text, preferring the number after a
/// chapter/episode marker so `"Volume 2 Chapter 10.5"` yields `10.5`, not `2`.
///
/// The marker search and the tail slice both run against the lowercased copy. Indexing the
/// *original* with an offset found in the lowercased string is a panic: `to_lowercase` is not
/// length-preserving (`"İ"` is 2 bytes and lowercases to 3), so the offset can land inside a
/// multi-byte character. Only ASCII digits are read afterwards, so the case folding is
/// immaterial to the result.
///
/// The return value becomes `chapters.number` and, through it, every reading-progress
/// comparison and the `chapter.discovered` fan-out. Each case below is a label a real listing
/// prints, and each is the reason a rule exists:
///
/// ```
/// use tankovault_adapters::html::parse_chapter_number;
///
/// // The marker wins over an earlier number, which is what makes volume-prefixed listings
/// // parse at all.
/// assert_eq!(parse_chapter_number("Volume 2 Chapter 10.5"), Some(10.5));
/// assert_eq!(parse_chapter_number("CHAPTER 8"), Some(8.0));
/// assert_eq!(parse_chapter_number("Ch. 99"), Some(99.0));
/// assert_eq!(parse_chapter_number("#7 - The End"), Some(7.0));
///
/// // With no marker there is nothing to prefer, so the first number wins. This is the
/// // fallback, not a special case.
/// assert_eq!(parse_chapter_number("1024.1 - The End"), Some(1024.1));
///
/// // The markers are searched in a fixed order, and `"chapter"` is found before `"ch "`.
/// // So a label carrying both reads the *chapter* number, not the volume's.
/// assert_eq!(parse_chapter_number("Ch 3 (Chapter 40)"), Some(40.0));
///
/// // A digit run too long for f64 is **no number at all**, not a very large one. This looks
/// // like a lost chapter and is the only correct answer: `"9".repeat(320).parse::<f64>()` is
/// // `Ok(inf)`, and an infinite chapter number freezes `latest_chapter` forever and
/// // serialises to JSON `null`. See `an_unrepresentable_digit_run_is_no_number_at_all`.
/// assert_eq!(parse_chapter_number(&format!("Chapter {}", "9".repeat(320))), None);
///
/// // Non-ASCII in the label is not an error — it is Tuesday. This is F-01's input.
/// assert_eq!(parse_chapter_number("İİİ Chapter 12"), Some(12.0));
///
/// // Only the absence of any digit is `None`.
/// assert_eq!(parse_chapter_number("Prologue"), None);
/// ```
#[must_use]
pub fn parse_chapter_number(text: &str) -> Option<f64> {
    let lower = text.to_lowercase();
    for marker in ["chapter", "episode", "chap", "ch.", "ch ", "#"] {
        if let Some(idx) = lower.find(marker) {
            let tail = &lower[idx + marker.len()..];
            if let Some(n) = parse_number(tail) {
                return Some(n);
            }
        }
    }
    parse_number(&lower)
}

/// Resolve `href` against the absolute `page_url` and reduce it to a **relative** path
/// (`/path?query`) suitable for storage. Handles absolute, root-relative, and
/// document-relative hrefs.
///
/// What comes back is what is stored in `chapters.path` / `sources.path` and later resolved
/// against the provider's configured `base_url`, which is why the host is dropped: a provider
/// that changes domain must not require a data migration.
///
/// ```
/// use tankovault_adapters::html::relativize;
///
/// const PAGE: &str = "https://provider.test/manga/solo-leveling/";
///
/// // All three href shapes a listing uses reduce to the same stored value.
/// assert_eq!(relativize(PAGE, "chapter-10/"), "/manga/solo-leveling/chapter-10/");
/// assert_eq!(relativize(PAGE, "/manga/solo-leveling/chapter-10/"), "/manga/solo-leveling/chapter-10/");
/// assert_eq!(
///     relativize(PAGE, "https://provider.test/manga/solo-leveling/chapter-10/"),
///     "/manga/solo-leveling/chapter-10/"
/// );
///
/// // The query survives, because paginated listings carry their page there.
/// assert_eq!(relativize(PAGE, "?page=2"), "/manga/solo-leveling/?page=2");
///
/// // The fragment does not. Two anchors into one document are one page.
/// assert_eq!(relativize(PAGE, "chapter-10/#top"), "/manga/solo-leveling/chapter-10/");
///
/// // A *different* host is silently flattened to its path. This looks like a bug and is the
/// // deliberate contract: the caller has already decided which provider it is talking to, and
/// // a cross-host link on a scanlation listing is a mirror of the same work far more often
/// // than it is a link somewhere else. Cover images take the opposite view and keep their host
/// // — that is what `absolutize` is for.
/// assert_eq!(relativize(PAGE, "https://mirror.other.test/x/1/"), "/x/1/");
///
/// // A foreign scheme is NOT rooted, because `Url::join` honours it and the path is all that
/// // is left. Named here rather than papered over; see `relativize_yields_a_rooted_path` in
/// // `tests/prop_html.rs`, whose strategy excludes `:` for exactly this reason.
/// assert_eq!(relativize(PAGE, "mailto:staff@provider.test"), "staff@provider.test");
/// ```
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

/// Unescape the five predefined XML/HTML entities (`&amp;` resolved last so a
/// double-encoded `&amp;lt;` decodes one level, not two).
///
/// Enough for text that a challenge solver or an XML viewer re-encoded on its way through a
/// DOM — JSON or XML wrapped in rendered markup — which is the only place adapters need it;
/// real page text is unescaped by the HTML parser itself.
#[must_use]
pub fn unescape_entities(s: &str) -> String {
    s.replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&#039;", "'")
        .replace("&amp;", "&")
}

#[cfg(test)]
mod tests {
    use super::*;
    use scraper::Html;

    /// Regression: `to_lowercase` is not length-preserving, so a byte offset found in the
    /// lowercased copy must never be applied to the original. `"İ"` (U+0130, 2 bytes) folds
    /// to `"i̇"` (3 bytes), which shifted every later offset and split a multi-byte character.
    /// Reachable from any provider's chapter-anchor text.
    #[test]
    fn parse_chapter_number_survives_non_length_preserving_case_folding() {
        assert_eq!(parse_chapter_number("İİİİ Chapter 12"), Some(12.0));
        assert_eq!(parse_chapter_number("İ#7"), Some(7.0));
        assert_eq!(parse_chapter_number("ΣΣΣ Episode 3.5 ΣΣ"), Some(3.5));
        // No marker: the fallback path must be boundary-safe too.
        assert_eq!(parse_chapter_number("İİİ 42"), Some(42.0));
        assert_eq!(parse_chapter_number("İİİ"), None);
    }

    /// **Regression: a long enough digit run used to become an *infinite* chapter number.**
    ///
    /// `parse_number` ended in `.parse::<f64>().ok()`, and Rust's float parser does not fail on
    /// a decimal outside `f64`'s range — `"9".repeat(320).parse::<f64>()` is `Ok(inf)`. So a
    /// listing whose anchor read `Chapter 999…9` yielded `Some(f64::INFINITY)`.
    ///
    /// `parse_year` had guarded against exactly this since it was written (`y.is_finite()`);
    /// `parse_chapter_number` never did, and it is the one whose value is persisted. The
    /// consequences are all silent:
    ///
    /// - `chapters.number` is `double precision`, which accepts `Infinity`, so it stores.
    /// - `latest_chapter` then never advances again — nothing is greater than `inf` — so every
    ///   genuinely new chapter of that series stops being reported as new.
    /// - `floor(number)` is `inf`, so the read-progress predicates (PERF-12) match nothing.
    /// - **`serde_json` serialises a non-finite float as `null`.** `chapter.discovered` carries
    ///   `Vec<f64>`, so the message goes onto the bus with a `null` where a number belongs,
    ///   fails to deserialise in the notifier, and is dropped as undecodable (ARCH-14) — taking
    ///   the whole scan's notification fan-out with it.
    ///
    /// Found while writing the oracle for the `adapters_generic_series_page` fuzz target, which
    /// needed an answer to "what may a `ChapterMeta.number` be?". Fixed in `parse_number` rather
    /// than in `parse_chapter_number`, so every present and future caller inherits it; the
    /// property `parse_number_is_always_finite` in `tests/prop_html.rs` is the standing guard.
    ///
    /// A rejected number is *skipped*, not clamped: `GenericConfigAdapter` drops a chapter whose
    /// number will not parse, which is the right answer for a label no ordering can place.
    #[test]
    fn an_unrepresentable_digit_run_is_no_number_at_all() {
        let overlong = "9".repeat(320);
        assert_eq!(
            overlong.parse::<f64>().map(f64::is_infinite),
            Ok(true),
            "premise: Rust's float parser returns inf, not an error, for this input"
        );

        assert_eq!(parse_number(&overlong), None);
        assert_eq!(parse_chapter_number(&format!("Chapter {overlong}")), None);
        assert_eq!(parse_year(&overlong), None);

        // The boundary stays usable: a value f64 can represent is still a number, however
        // absurd, because the guard is about representability and not about plausibility.
        assert_eq!(
            parse_number(&"9".repeat(308)).map(f64::is_finite),
            Some(true)
        );
    }

    #[test]
    fn parse_chapter_number_prefers_the_marker() {
        assert_eq!(parse_chapter_number("Volume 2 Chapter 10.5"), Some(10.5));
        assert_eq!(parse_chapter_number("CHAPTER 8"), Some(8.0));
        assert_eq!(parse_chapter_number("Ch. 99"), Some(99.0));
    }

    /// The point of the memo is that the second parse of the same spec is free. Pinned by
    /// identity rather than by timing, which is the fact that actually matters and does not
    /// flake on a loaded machine.
    #[test]
    fn selectors_are_parsed_once_and_reused() {
        let spec = "div.selector-cache-probe > a.title";
        let first = parse_selector(spec).unwrap();
        let second = parse_selector(spec).unwrap();
        assert!(
            Arc::ptr_eq(&first, &second),
            "a repeat parse must hit the cache; extractors call this inside per-item loops"
        );
        assert!(!Arc::ptr_eq(
            &first,
            &parse_selector("div.other-probe").unwrap()
        ));
    }

    #[test]
    fn an_invalid_selector_is_still_an_error_and_is_not_cached() {
        assert!(matches!(
            parse_selector(">>>not a selector<<<"),
            Err(AdapterError::Selector { .. })
        ));
        // Twice, to prove the failure path does not poison or populate the cache.
        assert!(parse_selector(">>>not a selector<<<").is_err());
    }

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
