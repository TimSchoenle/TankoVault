//! Small formatting helpers shared across screens.
//!
//! Deliberately dependency-free: dates are parsed and compared with the browser's own
//! `Date`, because pulling a date crate into the wasm bundle to render "3d ago" would cost
//! more bytes than the whole module. Anything with words in it resolves through
//! [`crate::i18n`] rather than baking English into the formatter.

use crate::i18n::Translator;

/// Hand `contents` to the browser as a download named `filename`.
///
/// Built from a `Blob` + object URL + a synthetic anchor click, which is the only way to make a
/// browser save a document the app already holds in memory. The alternative — pointing an
/// anchor at the endpoint — cannot work here: the export is bearer-authenticated and a plain
/// navigation carries no `Authorization` header.
///
/// The object URL is revoked immediately after the click. The download has already been handed
/// to the browser at that point, and leaving it alive pins the blob for the lifetime of the
/// document — which, for a personal-data export, means keeping the reader's entire record in
/// memory until they navigate away.
///
/// # Errors
/// A **catalogue key**, not a sentence. Every failure here means the browser is missing
/// something ordinary, so there is one generic message rather than one per DOM call — but it
/// used to be baked in as English and handed verbatim to the reader by both callers, in
/// contradiction of this module's own contract (see the module docs). Callers hold a
/// [`Translator`]; resolving there is the same pattern `politeness_json` uses.
pub(crate) fn save_text_file(
    filename: &str,
    mime: &str,
    contents: &str,
) -> Result<(), &'static str> {
    use wasm_bindgen::JsCast as _;

    let failed = || "common.downloadRefused";

    let parts = js_sys::Array::new();
    parts.push(&wasm_bindgen::JsValue::from_str(contents));
    let options = web_sys::BlobPropertyBag::new();
    options.set_type(mime);
    let blob =
        web_sys::Blob::new_with_str_sequence_and_options(&parts, &options).map_err(|_| failed())?;
    let url = web_sys::Url::create_object_url_with_blob(&blob).map_err(|_| failed())?;

    let document = web_sys::window()
        .and_then(|w| w.document())
        .ok_or_else(failed)?;
    let anchor = document
        .create_element("a")
        .map_err(|_| failed())?
        .dyn_into::<web_sys::HtmlAnchorElement>()
        .map_err(|_| failed())?;
    anchor.set_href(&url);
    anchor.set_download(filename);
    anchor.click();

    let _ = web_sys::Url::revoke_object_url(&url);
    Ok(())
}

/// Render a chapter number without a trailing `.0`, so whole chapters read `#152` and part
/// releases keep their fraction (`#152.6`).
pub(crate) fn chapter_number(n: f64) -> String {
    if n.fract() == 0.0 {
        // Chapter numbers are small positive counts, so the truncating cast is exact for
        // every value the API can produce; the guard above has already ruled out fractions.
        #[allow(clippy::cast_possible_truncation)]
        return format!("{}", n as i64);
    }
    format!("{n}")
}

/// Group a large count for the console's KPI tiles and stat tables (`1,284,903`).
pub(crate) fn thousands(n: i64) -> String {
    // `unsigned_abs`, not `abs`: `i64::MIN.abs()` overflows and would panic.
    let digits = n.unsigned_abs().to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3 + 1);
    if n < 0 {
        out.push('-');
    }
    for (index, digit) in digits.chars().enumerate() {
        if index > 0 && (digits.len() - index).is_multiple_of(3) {
            out.push(',');
        }
        out.push(digit);
    }
    out
}

/// The date component of an RFC-3339 timestamp (`2026-07-25`), or an empty string.
///
/// Slices rather than parses: the API always emits RFC-3339, whose first ten bytes are the
/// date, and a slice cannot fail on a value the browser's parser would reject.
pub(crate) fn iso_date(ts: Option<&str>) -> &str {
    ts.and_then(|s| s.get(0..10)).unwrap_or("")
}

/// Format an RFC-3339 timestamp as a coarse "time ago" label. `None`/empty renders as an
/// em dash; a value the browser can't parse falls back to the raw string rather than lying.
pub(crate) fn rel_time(i18n: Translator, ts: Option<&str>) -> String {
    let Some(s) = ts.filter(|s| !s.is_empty()) else {
        return i18n.t("time.unknown");
    };
    let parsed = js_sys::Date::parse(s);
    if parsed.is_nan() {
        return s.to_owned();
    }
    let age = Age::of(js_sys::Date::now() - parsed);
    i18n.args(age.key(), &[("count", &age.count().to_string())])
}

/// How recent a timestamp has to be to count as "hours fresh" — the chapter list tints a
/// release jade below this and leaves it faint above.
const FRESH_MS: f64 = 48.0 * 3_600_000.0;

/// Whether `ts` is recent enough to be worth calling out. `None`, empty and unparseable
/// timestamps are not fresh: an unknown age is never presented as a new one.
pub(crate) fn is_fresh(ts: Option<&str>) -> bool {
    let Some(s) = ts.filter(|s| !s.is_empty()) else {
        return false;
    };
    let parsed = js_sys::Date::parse(s);
    if parsed.is_nan() {
        return false;
    }
    fresh_age(js_sys::Date::now() - parsed)
}

/// The freshness rule itself, split out from the clock so the boundary is testable on the host
/// target — reading the browser clock needs a wasm runtime, deciding what the number means
/// does not. A negative age means a clock skew, which is not evidence of freshness.
fn fresh_age(age_ms: f64) -> bool {
    (0.0..FRESH_MS).contains(&age_ms)
}

/// A millisecond age reduced to the unit it should be read in.
///
/// Kept separate from the wording so the bucket boundaries stay unit-testable on the host
/// target — the phrasing needs a Dioxus runtime, the arithmetic does not — and so a
/// translation can put the number wherever its language wants it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Age {
    JustNow,
    Minutes(i64),
    Hours(i64),
    Days(i64),
    Months(i64),
    Years(i64),
}

impl Age {
    fn of(diff_ms: f64) -> Self {
        if diff_ms < 45_000.0 {
            return Self::JustNow;
        }
        // A difference in minutes cannot overflow `i64` for any timestamp the API emits.
        #[allow(clippy::cast_possible_truncation)]
        let mins = (diff_ms / 60_000.0) as i64;
        if mins < 60 {
            return Self::Minutes(mins);
        }
        let hours = mins / 60;
        if hours < 24 {
            return Self::Hours(hours);
        }
        let days = hours / 24;
        if days < 30 {
            return Self::Days(days);
        }
        let months = days / 30;
        if months < 12 {
            return Self::Months(months);
        }
        Self::Years(days / 365)
    }

    /// The catalogue key wording this bucket; every entry but `justNow` takes `{count}`.
    fn key(self) -> &'static str {
        match self {
            Self::JustNow => "time.justNow",
            Self::Minutes(_) => "time.minutesAgo",
            Self::Hours(_) => "time.hoursAgo",
            Self::Days(_) => "time.daysAgo",
            Self::Months(_) => "time.monthsAgo",
            Self::Years(_) => "time.yearsAgo",
        }
    }

    fn count(self) -> i64 {
        match self {
            Self::JustNow => 0,
            Self::Minutes(n)
            | Self::Hours(n)
            | Self::Days(n)
            | Self::Months(n)
            | Self::Years(n) => n,
        }
    }
}

/// The uppercase first character of `text`, for avatar and cover-fallback tiles.
pub(crate) fn initial(text: &str) -> String {
    text.chars()
        .next()
        .unwrap_or('?')
        .to_uppercase()
        .to_string()
}

/// A two-letter monogram for a provider or tracker tile: the initials of a multi-word name,
/// or the first two letters of a single-word one.
pub(crate) fn monogram(name: &str) -> String {
    let mut words = name.split_whitespace().filter(|word| !word.is_empty());
    match (words.next(), words.next()) {
        (Some(first), Some(second)) => format!("{}{}", initial(first), initial(second)),
        (Some(only), None) => only
            .chars()
            .filter(|c| c.is_alphanumeric())
            .take(2)
            .collect::<String>()
            .to_uppercase(),
        _ => "?".to_owned(),
    }
}

/// The catalogue key of the time-of-day greeting, from the browser clock.
pub(crate) fn greeting_key() -> &'static str {
    match js_sys::Date::new_0().get_hours() {
        5..=11 => "greeting.morning",
        12..=17 => "greeting.afternoon",
        18..=21 => "greeting.evening",
        _ => "greeting.night",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn trims_whole_chapter_numbers_but_keeps_parts() {
        assert_eq!(chapter_number(152.0), "152");
        assert_eq!(chapter_number(152.6), "152.6");
        assert_eq!(chapter_number(0.0), "0");
    }

    #[test]
    fn groups_thousands() {
        assert_eq!(thousands(0), "0");
        assert_eq!(thousands(999), "999");
        assert_eq!(thousands(1_000), "1,000");
        assert_eq!(thousands(1_284_903), "1,284,903");
        assert_eq!(thousands(-4_512), "-4,512");
    }

    #[test]
    fn the_most_negative_integer_does_not_overflow() {
        assert_eq!(thousands(i64::MIN), "-9,223,372,036,854,775,808");
    }

    #[test]
    fn slices_the_date_out_of_a_timestamp() {
        assert_eq!(iso_date(Some("2026-07-25T09:31:00Z")), "2026-07-25");
        assert_eq!(iso_date(Some("short")), "");
        assert_eq!(iso_date(None), "");
    }

    #[test]
    fn buckets_ages_across_every_unit_boundary() {
        assert_eq!(Age::of(1_000.0), Age::JustNow);
        assert_eq!(Age::of(120_000.0), Age::Minutes(2));
        assert_eq!(Age::of(3.0 * 3_600_000.0), Age::Hours(3));
        assert_eq!(Age::of(5.0 * 86_400_000.0), Age::Days(5));
        assert_eq!(Age::of(90.0 * 86_400_000.0), Age::Months(3));
        assert_eq!(Age::of(800.0 * 86_400_000.0), Age::Years(2));
    }

    #[test]
    fn initial_falls_back_for_empty_text() {
        assert_eq!(initial("kaz"), "K");
        assert_eq!(initial(""), "?");
    }

    #[test]
    fn monograms_prefer_initials_then_fall_back_to_the_first_two_letters() {
        assert_eq!(monogram("Asura Scans"), "AS");
        assert_eq!(monogram("MangaDex"), "MA");
        assert_eq!(monogram("Bato.to"), "BA");
        assert_eq!(monogram(""), "?");
    }

    #[test]
    fn a_missing_timestamp_is_never_presented_as_fresh() {
        // Only the two arms that short-circuit before reading the browser clock; parsing one
        // needs a wasm runtime, and the rule those arms guard is covered below.
        assert!(!is_fresh(None));
        assert!(!is_fresh(Some("")));
    }

    #[test]
    fn freshness_covers_the_last_two_days_and_nothing_before_or_after() {
        assert!(fresh_age(0.0));
        assert!(fresh_age(3.0 * 3_600_000.0));
        assert!(!fresh_age(FRESH_MS));
        assert!(!fresh_age(FRESH_MS + 1.0));
        // A future timestamp is a clock skew, not a new release.
        assert!(!fresh_age(-1.0));
    }
}
