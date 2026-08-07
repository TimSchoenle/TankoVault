//! Small formatting helpers shared across screens.
//!
//! Deliberately dependency-free: dates are read through [`crate::platform`], which on the web
//! build is the browser's own `Date` — pulling a date crate into the wasm bundle to render
//! "3d ago" would cost more bytes than the whole module. Anything with words in it resolves
//! through [`crate::i18n`] rather than baking English into the formatter.

use crate::i18n::Translator;
use std::fmt::Write as _;

/// Render a chapter number without a trailing `.0`, so whole chapters read `#152` and part
/// releases keep their fraction (`#152.6`).
pub(crate) fn chapter_number(n: f64) -> String {
    if n.fract() == 0.0 {
        #[expect(
            clippy::cast_possible_truncation,
            reason = "the guard above has ruled out a fractional part, and chapter numbers are \
                      small positive counts, so the cast is exact"
        )]
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

/// A count narrowed to fit a badge: `999`, `1.2k`, `12k`, `1.4M`, `99M+`.
///
/// The bell's badge is a circle sized for three glyphs sitting on the icon's corner. A literal
/// count overflows it the moment an inbox passes 999 — the pill grows, pushes past the icon and
/// takes the bar's alignment with it — so the *string* is bounded rather than the layout being
/// asked to cope with an unbounded one.
///
/// One decimal below ten of each unit and none above: `1.2k` carries information a reader uses,
/// `12.4k` does not, and dropping it is what keeps the string within four glyphs.
///
/// Integer arithmetic throughout. The fractional digit is a division, not a rounded float, so
/// `999_600` prints `999k` rather than the `1.0k` a rounded one would put beside a bell that has
/// not reached a million.
#[must_use]
pub(crate) fn compact_count(n: i64) -> String {
    /// Past this the badge gives up on precision rather than on its width.
    const CEILING: i64 = 99_999_999;

    // Negative is not a state any counter this formats can reach; clamped rather than handled,
    // so a corrupted value renders as nothing alarming.
    let n = n.max(0);
    if n > CEILING {
        return "99M+".to_owned();
    }
    // Tenths of the chosen unit, so the decimal digit falls out of the remainder.
    let (tenths, unit) = if n >= 1_000_000 {
        (n / 100_000, "M")
    } else if n >= 1_000 {
        (n / 100, "k")
    } else {
        return n.to_string();
    };
    let whole = tenths / 10;
    let tenth = tenths % 10;
    if whole < 10 && tenth > 0 {
        format!("{whole}.{tenth}{unit}")
    } else {
        format!("{whole}{unit}")
    }
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
    let parsed = crate::platform::parse_timestamp_ms(s);
    if parsed.is_nan() {
        return s.to_owned();
    }
    let age = Age::of(crate::platform::now_ms() - parsed);
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
    let parsed = crate::platform::parse_timestamp_ms(s);
    if parsed.is_nan() {
        return false;
    }
    fresh_age(crate::platform::now_ms() - parsed)
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
        #[expect(
            clippy::cast_possible_truncation,
            reason = "a difference in minutes cannot overflow i64 for any timestamp the API emits"
        )]
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

/// The catalogue key of the time-of-day greeting, from the reader's own clock.
pub(crate) fn greeting_key() -> &'static str {
    match crate::platform::local_hour() {
        5..=11 => "greeting.morning",
        12..=17 => "greeting.afternoon",
        18..=21 => "greeting.evening",
        _ => "greeting.night",
    }
}

/// Percent-encode everything the query grammar reserves.
///
/// The router's own encoding leaves `&` and `=` alone, so a filter like `fate/stay & night`
/// would otherwise re-parse as two parameters.
pub(crate) fn encode_component(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for byte in value.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(byte as char);
            }
            // `write!` into a `String` is infallible, so discarding the `Result` is safe (and
            // avoids an allocation per escaped byte).
            _ => {
                let _ = write!(out, "%{byte:02X}");
            }
        }
    }
    out
}

/// The inverse of [`encode_component`]. A malformed escape is kept verbatim rather than
/// dropped — a hand-typed `100%` in the filter box should search for `100%`, not `100`.
pub(crate) fn decode_component(value: &str) -> String {
    let bytes = value.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let Ok(byte) = u8::from_str_radix(&value[i + 1..i + 3], 16) {
                out.push(byte);
                i += 3;
                continue;
            }
        }
        // `+` is a form-encoding convention `encode_component` never emits, but pasted URLs do.
        out.push(if bytes[i] == b'+' { b' ' } else { bytes[i] });
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A malformed escape is data, not an error.
    #[test]
    fn a_malformed_escape_is_kept_verbatim() {
        assert_eq!(decode_component("100%"), "100%");
        assert_eq!(decode_component("%zz"), "%zz");
    }

    /// The query grammar's own separators must survive a round trip, or a filter containing one
    /// re-parses as extra parameters and everything after it is lost.
    #[test]
    fn reserved_characters_round_trip() {
        for value in ["fate/stay & night = ?", "九番の鐘", ""] {
            assert_eq!(decode_component(&encode_component(value)), value);
        }
    }

    /// Pins the badge string's length, which is the whole point of the function.
    ///
    /// The bell's badge is a fixed circle on the icon's corner. It used to print the raw count,
    /// so an account with four-figure unread notifications rendered a pill wider than the icon
    /// it sat on and shoved the top bar's actions out of alignment. The boundaries below are the
    /// ones that regressed: 999 → 1k is where a third glyph would have become a fourth, and the
    /// truncation is what stops 999 600 claiming to be a million.
    #[test]
    fn a_badge_count_never_exceeds_four_glyphs() {
        for (count, expected) in [
            (0_i64, "0"),
            (7, "7"),
            (999, "999"),
            (1_000, "1k"),
            (1_240, "1.2k"),
            (9_990, "9.9k"),
            (10_000, "10k"),
            (12_400, "12k"),
            (999_600, "999k"),
            (1_000_000, "1M"),
            (1_400_000, "1.4M"),
            (99_999_999, "99M"),
            (100_000_000, "99M+"),
        ] {
            let rendered = compact_count(count);
            assert_eq!(rendered, expected, "compact_count({count})");
            assert!(
                rendered.chars().count() <= 4,
                "{rendered} does not fit the badge"
            );
        }
        // Not reachable from an unread count, but the formatter must not emit a minus sign into
        // a circle sized for three digits if one ever appears.
        assert_eq!(compact_count(-5), "0");
    }

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
