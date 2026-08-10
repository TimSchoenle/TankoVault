//! HTML extraction helpers shared by the config-driven adapters. Selector syntax: a plain CSS
//! selector takes the element's inner text; a `sel@attr` suffix takes an attribute instead.

use crate::error::AdapterError;
use scraper::{ElementRef, Selector};
use std::collections::HashMap;
use std::sync::{Arc, LazyLock, RwLock};
use tankovault_domain::SeriesStatus;
use tankovault_fetch::FetchResponse;
use time::OffsetDateTime;
use time::macros::format_description;
use url::Url;

/// Compiled selectors, keyed by source text.
///
/// Selectors come from `providers.config`, not constants, so `LazyLock<Selector>` alone can't
/// memoise them; without this cache, `Selector::parse` re-runs inside every per-item extractor
/// loop, costing tens of milliseconds of re-tokenising per catalogue/sitemap page.
static SELECTOR_CACHE: LazyLock<RwLock<HashMap<String, Arc<Selector>>>> =
    LazyLock::new(|| RwLock::new(HashMap::new()));

/// Upper bound on distinct cached selectors — a guard against a pathological config turning
/// the memo into a leak, not a working-set limit. Past this, later selectors are parsed each
/// time instead of cached: slower, never wrong.
const SELECTOR_CACHE_CAP: usize = 4096;

/// Parse a CSS selector, mapping failures to a typed error. Memoised — see [`SELECTOR_CACHE`].
///
/// Returns an `Arc` so a cache hit is a refcount bump; call sites are unchanged.
///
/// # Errors
/// [`AdapterError::Selector`] if `spec` is not a valid selector.
pub fn parse_selector(spec: &str) -> Result<Arc<Selector>, AdapterError> {
    if let Ok(cache) = SELECTOR_CACHE.read()
        && let Some(hit) = cache.get(spec)
    {
        return Ok(Arc::clone(hit));
    }

    let parsed = Arc::new(Selector::parse(spec).map_err(|e| AdapterError::Selector {
        selector: spec.to_owned(),
        reason: e.to_string(),
    })?);

    if let Ok(mut cache) = SELECTOR_CACHE.write()
        && cache.len() < SELECTOR_CACHE_CAP
    {
        cache.insert(spec.to_owned(), Arc::clone(&parsed));
    }
    Ok(parsed)
}

/// Parse a response body as HTML and run `extract` over it on the blocking thread pool.
///
/// `Html::parse_document` is uninterruptible CPU (5-50 ms for a large page); run inline on a
/// Tokio worker it stalls every other async task on that thread, including queue heartbeats.
/// The parse-and-extract phase must move together since `scraper::Html`/`ElementRef` are not
/// `Send` — the closure returns owned data, and gets the [`FetchResponse`] since
/// [`AdapterError::missing`] needs the envelope.
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
    // Join error means the task panicked; classified as `Parse` (non-retryable) since a panic
    // reproduces identically on replay.
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

/// Pseudo-attribute selecting an element's **own** text, excluding text inside child elements.
///
/// Themes decorate a heading with sibling elements rather than a separate field — Madara puts
/// its `HOT`/`NEW`/`END` badge inside the `<h1>`, so the default "all descendant text" reading
/// stores `Solo Leveling END` as the canonical title. That title is normalised into the
/// matching key, so the badge does not stay cosmetic: it changes which sources a series
/// collects and what catalogue search will find.
pub const OWN_TEXT_ATTR: &str = "text";

fn value_of(el: ElementRef<'_>, attr: Option<&str>) -> String {
    match attr {
        Some(a) if a == OWN_TEXT_ATTR => el
            .children()
            .filter_map(|node| node.value().as_text().map(|t| t.to_string()))
            .collect::<String>()
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" "),
        Some(a) => el.value().attr(a).unwrap_or_default().trim().to_owned(),
        None => text_of(el),
    }
}

/// Split a label/value cell (`Author(s)`, `Artist(s)`, …) into its entries on `,`/`;`.
///
/// Themes render these as one joined string; a value that legitimately contains a comma also
/// splits, which is accepted. **Not** for alternative titles — see [`split_titles`].
#[must_use]
pub fn split_list(value: &str) -> Vec<String> {
    value
        .split([',', ';'])
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
        .collect()
}

/// Split an `Alternative` cell into the titles it lists, refusing to shred one title that
/// happens to contain a comma.
///
/// A fragment of a sentence is not an inert row in `series_titles`: `find_candidates` searches
/// those keys and `best_title_match` scores an exact hit on one as a name the series answers
/// to, so `a prince` — cut out of "…Reincarnated as a Mere Villager, a Prince, a Saint? Nay…"
/// by an unconditional comma split — pulled every unrelated series whose title normalises to
/// those two words onto one row. One live series absorbed 281 sources that way.
///
/// The rule: `;`/`|` are unambiguous list separators and always split. A comma splits only
/// when *every* resulting fragment could stand alone as a title, which a fragment starting
/// with a lower-case letter cannot — it was cut out of the middle of a sentence. Scripts
/// without case are unaffected, since they have no lower-case letters to start with.
#[must_use]
pub fn split_titles(value: &str) -> Vec<String> {
    let mut titles = Vec::new();
    for part in value.split([';', '|']).map(str::trim) {
        if part.is_empty() {
            continue;
        }
        let fragments = || part.split(',').map(str::trim);
        if part.contains(',')
            && fragments().all(|f| !f.is_empty() && !f.starts_with(char::is_lowercase))
        {
            titles.extend(fragments().map(str::to_owned));
        } else {
            titles.push(part.to_owned());
        }
    }
    titles
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
/// Only ever yields a **finite** value: a digit run too long for `f64` parses to `inf` rather
/// than failing, and an infinite value cannot be compared, ordered or serialised.
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
#[expect(
    clippy::cast_possible_truncation,
    reason = "the range check immediately below is what makes the cast sound"
)]
pub fn parse_year(text: &str) -> Option<i32> {
    let y = parse_number(text)?;
    // Redundant with `parse_number`'s guarantee, but this is what makes the cast below sound —
    // removing it silently reopens the bug `parse_chapter_number`'s regression test pins.
    if y.is_finite() && y >= f64::from(i32::MIN) && y <= f64::from(i32::MAX) {
        Some(y as i32)
    } else {
        None
    }
}

/// Parse a chapter number from listing text, preferring the number after a
/// chapter/episode marker so `"Volume 2 Chapter 10.5"` yields `10.5`, not `2`.
///
/// Marker search and slicing both use the lowercased copy: indexing the original with an
/// offset from `to_lowercase` can panic, since case folding is not length-preserving (e.g.
/// `İ` grows by a byte). Only ASCII digits are read after the marker, so folding cannot
/// corrupt the result.
///
/// ```
/// use tankovault_adapters::html::parse_chapter_number;
///
/// assert_eq!(parse_chapter_number("Volume 2 Chapter 10.5"), Some(10.5));
/// assert_eq!(parse_chapter_number("CHAPTER 8"), Some(8.0));
/// assert_eq!(parse_chapter_number("Ch. 99"), Some(99.0));
/// assert_eq!(parse_chapter_number("#7 - The End"), Some(7.0));
/// assert_eq!(parse_chapter_number("1024.1 - The End"), Some(1024.1));
///
/// // Markers are searched in a fixed order: "chapter" before "ch ".
/// assert_eq!(parse_chapter_number("Ch 3 (Chapter 40)"), Some(40.0));
///
/// // A digit run too long for f64 is no number at all, not a very large one.
/// assert_eq!(parse_chapter_number(&format!("Chapter {}", "9".repeat(320))), None);
///
/// assert_eq!(parse_chapter_number("İİİ Chapter 12"), Some(12.0));
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
/// The host is dropped since this is stored in `chapters.path`/`sources.path` and resolved
/// later against the provider's `base_url` — a provider that changes domain must not require
/// a data migration.
///
/// ```
/// use tankovault_adapters::html::relativize;
///
/// const PAGE: &str = "https://provider.test/manga/solo-leveling/";
///
/// assert_eq!(relativize(PAGE, "chapter-10/"), "/manga/solo-leveling/chapter-10/");
/// assert_eq!(relativize(PAGE, "/manga/solo-leveling/chapter-10/"), "/manga/solo-leveling/chapter-10/");
/// assert_eq!(
///     relativize(PAGE, "https://provider.test/manga/solo-leveling/chapter-10/"),
///     "/manga/solo-leveling/chapter-10/"
/// );
/// assert_eq!(relativize(PAGE, "?page=2"), "/manga/solo-leveling/?page=2");
/// assert_eq!(relativize(PAGE, "chapter-10/#top"), "/manga/solo-leveling/chapter-10/");
///
/// // Deliberate: a different host is flattened to its path, since the caller already knows
/// // which provider it's talking to. Covers keep their host instead; see `absolutize`.
/// assert_eq!(relativize(PAGE, "https://mirror.other.test/x/1/"), "/x/1/");
///
/// // A foreign scheme is not rooted: `Url::join` honours it, so only the path remains (see
/// // `relativize_yields_a_rooted_path` in tests/prop_html.rs).
/// assert_eq!(relativize(PAGE, "mailto:staff@provider.test"), "staff@provider.test");
/// ```
#[must_use]
pub fn relativize(page_url: &str, href: &str) -> String {
    if let Ok(base) = Url::parse(page_url)
        && let Ok(joined) = base.join(href.trim())
    {
        let mut path = joined.path().to_owned();
        if let Some(q) = joined.query() {
            path.push('?');
            path.push_str(q);
        }
        return path;
    }
    if href.starts_with('/') {
        href.to_owned()
    } else {
        format!("/{href}")
    }
}

/// Resolve `href` against the absolute `page_url` into an **absolute** URL string.
///
/// Unlike [`relativize`], preserves the host: an already-absolute href (e.g. a CDN-hosted
/// cover) passes through unchanged. Falls back to trimmed `href` if `page_url` is
/// unparseable. Used for values consumed directly by clients (covers), not stored links.
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

/// Month names as providers spell them, long and abbreviated, indexed from January.
const MONTHS: [&str; 12] = [
    "january",
    "february",
    "march",
    "april",
    "may",
    "june",
    "july",
    "august",
    "september",
    "october",
    "november",
    "december",
];

/// Parse a chapter release label in any of the shapes providers actually publish, relative to
/// `now`.
///
/// The four shapes, tried in order: RFC 3339 (`2026-08-09T15:05:06.175797Z`, what the JSON APIs
/// and Astro islands carry), ISO `YYYY-MM-DD`, a month-name date (`August 9, 2026`, the Madara
/// themes), and a relative label (`3 days ago`). Anything else yields `None`, leaving the date
/// unset rather than guessed — a wrong `published_at` reorders the release feed.
///
/// Relative labels resolve to whole units before `now`, which is as precise as the label is.
///
/// ```
/// use tankovault_adapters::html::parse_date_label;
/// use time::macros::datetime;
///
/// let now = datetime!(2026-08-10 12:00 UTC);
/// assert_eq!(parse_date_label("2026-08-09T15:05:06.175797Z", now).map(|d| d.day()), Some(9));
/// assert_eq!(parse_date_label("2026-08-09", now).map(|d| d.day()), Some(9));
/// assert_eq!(parse_date_label("August 9, 2026", now).map(|d| d.day()), Some(9));
/// assert_eq!(parse_date_label("3 days ago", now).map(|d| d.day()), Some(7));
/// assert_eq!(parse_date_label("Prologue", now), None);
/// ```
#[must_use]
pub fn parse_date_label(text: &str, now: OffsetDateTime) -> Option<OffsetDateTime> {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return None;
    }
    if let Ok(dt) = OffsetDateTime::parse(trimmed, &time::format_description::well_known::Rfc3339) {
        return Some(dt);
    }
    if let Some(d) = parse_ymd_date(trimmed) {
        return Some(d);
    }
    if let Some(d) = parse_month_name_date(trimmed) {
        return Some(d);
    }
    parse_relative_label(trimmed, now)
}

/// `August 9, 2026` / `Aug 9 2026` / `9 August 2026`.
fn parse_month_name_date(text: &str) -> Option<OffsetDateTime> {
    let lower = text.to_lowercase();
    // Matched on the three-letter prefix so both `August` and `Aug` resolve. A false positive
    // still produces nothing: a day in 1..=31 and a year >= 1900 both have to be present too.
    let month = MONTHS.iter().position(|m| lower.contains(&m[..3]))?;
    // Two bare numbers remain once the month word is out: the day (1-31) and the year. The
    // year is always last in every shape providers use (`August 9, 2026`, `9 August 2026`,
    // and Toonily's two-digit `May 31, 23`), which is what disambiguates them — `23` is not
    // distinguishable from a day by range alone.
    let numbers: Vec<u32> = lower
        .split(|c: char| !c.is_ascii_digit())
        .filter(|s| !s.is_empty())
        .filter_map(|s| s.parse().ok())
        .collect();
    let (&year_raw, rest) = numbers.split_last()?;
    let year = if year_raw < 100 {
        2000 + year_raw
    } else {
        year_raw
    };
    if !(1900..=2200).contains(&year) {
        return None;
    }
    let day = rest.iter().copied().find(|n| (1..=31).contains(n))?;
    let month = time::Month::try_from(u8::try_from(month + 1).ok()?).ok()?;
    time::Date::from_calendar_date(i32::try_from(year).ok()?, month, u8::try_from(day).ok()?)
        .ok()
        .map(|d| d.midnight().assume_utc())
}

/// `3 days ago`, `an hour ago`, `just now`.
fn parse_relative_label(text: &str, now: OffsetDateTime) -> Option<OffsetDateTime> {
    let lower = text.to_lowercase();
    if !lower.contains("ago") && !lower.contains("now") {
        return None;
    }
    if lower.contains("now") {
        return Some(now);
    }
    // "an hour"/"a day" carry no digits and mean one. Read as an integer directly rather than
    // through `parse_number`, so no float ever has to be cast back down to a count.
    let count: i32 = lower
        .split(|c: char| !c.is_ascii_digit())
        .find(|s| !s.is_empty())
        .map_or(Ok(1), str::parse)
        .ok()?;
    let unit = [
        ("year", time::Duration::days(365)),
        ("month", time::Duration::days(30)),
        ("week", time::Duration::weeks(1)),
        ("day", time::Duration::days(1)),
        ("hour", time::Duration::hours(1)),
        ("min", time::Duration::minutes(1)),
        ("sec", time::Duration::seconds(1)),
    ]
    .into_iter()
    .find(|(name, _)| lower.contains(name))?
    .1;
    unit.checked_mul(count).and_then(|d| now.checked_sub(d))
}

/// Unescape the five predefined XML/HTML entities (`&amp;` resolved last so a
/// double-encoded `&amp;lt;` decodes one level, not two).
///
/// Only needed for markup re-encoded by a challenge solver or XML viewer; real page text is
/// already unescaped by the HTML parser.
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

    /// Regression: an unconditional comma split shredded one long alternative title into
    /// sentence fragments (`a prince`, `a beautiful girl`, `a saint? nay`), and every fragment
    /// became a key `find_candidates` searches and `best_title_match` scores as identity — the
    /// mechanism behind one live series absorbing 281 unrelated sources.
    #[test]
    fn a_sentence_that_contains_commas_stays_one_alternative_title() {
        let sentence = "A Mere Villager Was Reincarnated, a Prince, a Saint? Nay, a Beautiful Girl";
        assert_eq!(split_titles(sentence), vec![sentence]);
    }

    /// A genuine list still splits: an explicit `;`/`|` always separates, and a comma does when
    /// every fragment could stand alone as a title.
    #[test]
    fn a_list_of_titles_still_splits() {
        assert_eq!(
            split_titles("Shibuya Noir, 시부야 느와르; Shibuya Nowaru"),
            vec!["Shibuya Noir", "시부야 느와르", "Shibuya Nowaru"]
        );
        assert_eq!(
            split_titles("Berserk | ベルセルク"),
            vec!["Berserk", "ベルセルク"]
        );
        assert!(split_titles("   ").is_empty());
        assert!(split_titles("").is_empty());
    }

    /// Regression: a byte offset found in the lowercased copy must never index the original —
    /// `to_lowercase` isn't length-preserving (`İ` grows a byte), which used to split a
    /// multi-byte character.
    #[test]
    fn parse_chapter_number_survives_non_length_preserving_case_folding() {
        assert_eq!(parse_chapter_number("İİİİ Chapter 12"), Some(12.0));
        assert_eq!(parse_chapter_number("İ#7"), Some(7.0));
        assert_eq!(parse_chapter_number("ΣΣΣ Episode 3.5 ΣΣ"), Some(3.5));
        // No marker: the fallback path must be boundary-safe too.
        assert_eq!(parse_chapter_number("İİİ 42"), Some(42.0));
        assert_eq!(parse_chapter_number("İİİ"), None);
    }

    /// Regression: a digit run too long for `f64` used to parse as infinity
    /// (`"9".repeat(320).parse::<f64>()` is `Ok(inf)`) — a non-finite chapter number never
    /// compares greater than anything and serialises to JSON `null`, so `latest_chapter` froze
    /// and the chapter-discovered notification silently stopped firing. Fixed in
    /// `parse_number`, guarded by `parse_number_is_always_finite` in `tests/prop_html.rs`.
    ///
    /// A rejected number is skipped, not clamped — correct for a label no ordering can place.
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

        // Still usable at the boundary: the guard is about representability, not plausibility.
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

    /// The memo means a repeat parse is free; pinned by identity, not timing, so it doesn't
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
