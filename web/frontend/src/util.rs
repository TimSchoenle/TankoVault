//! Small formatting helpers shared across screens.
//!
//! Deliberately dependency-free: dates are parsed and compared with the browser's own
//! `Date`, because pulling a date crate into the wasm bundle to render "3d ago" would cost
//! more bytes than the whole module. Anything with words in it resolves through
//! [`crate::i18n`] rather than baking English into the formatter.

use crate::i18n::Translator;

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
}
