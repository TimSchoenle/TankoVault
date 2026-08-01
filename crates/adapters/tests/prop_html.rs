//! Totality and algebraic properties of the HTML/text extraction helpers, run over arbitrary
//! Unicode input on every `cargo test`.

use proptest::prelude::*;
use tankovault_adapters::html::{
    absolutize, map_status, parse_chapter_number, parse_number, parse_year, parse_ymd_date,
    relativize, split_attr, unescape_entities,
};

/// A page URL of the shape the crawler actually holds when these are called.
fn page_url() -> impl Strategy<Value = String> {
    "https://[a-z]{1,8}\\.test/manga/[a-z0-9-]{0,10}/"
}

/// An `href` as it appears in markup, excluding a scheme. Deliberately no `:` — see
/// [`relativize_yields_a_rooted_path`] for why that case is called out separately.
fn scheme_less_href() -> impl Strategy<Value = String> {
    "[a-zA-Z0-9/._~%?&=#-]{0,32}"
}

/// Provider text carrying a digit run long enough to exhaust `f64`, optionally behind a chapter
/// marker so both entry points are reached.
///
/// Generated structurally rather than via regex: a regex strategy won't produce a 300-digit
/// run at any realistic size, so simplifying this back to a plain `".*"`-style strategy would
/// silently stop testing the overflow case.
fn unrepresentable_number_text() -> impl Strategy<Value = String> {
    (
        prop::sample::select(vec![
            "",
            "Chapter ",
            "Episode ",
            "Ch. ",
            "#",
            "Volume 2 Chapter ",
        ]),
        250usize..400usize,
        prop::option::of("[a-z .-]{0,8}"),
    )
        .prop_map(|(marker, digits, tail)| {
            format!("{marker}{}{}", "9".repeat(digits), tail.unwrap_or_default())
        })
}

proptest! {
    /// Guards against a `to_lowercase`-offset panic: `str::to_lowercase` isn't length-preserving
    /// (`İ` grows a byte), so indexing across the two panics on some Unicode chapter titles.
    #[test]
    fn parse_chapter_number_never_panics(text in ".*") {
        let _ = parse_chapter_number(&text);
    }

    /// Every number this module yields is finite.
    ///
    /// Regression: `parse_number` used `.parse::<f64>().ok()`, and Rust's float parser returns
    /// `Ok(inf)`, not `Err`, for a decimal outside `f64`'s range — so an overlong digit run
    /// produced an infinite chapter number that froze `latest_chapter` and serialised to JSON
    /// `null`. Asserted over both the ordinary and overlong generator, since the invariant
    /// covers every input.
    #[test]
    fn parse_number_is_always_finite(
        text in prop_oneof![3 => ".*", 1 => unrepresentable_number_text()],
    ) {
        for value in [parse_number(&text), parse_chapter_number(&text)]
            .into_iter()
            .flatten()
        {
            prop_assert!(value.is_finite(), "{value} parsed out of {text:?}");
        }
    }

    /// `parse_number` finds a number exactly when there is an ASCII digit present, stated as
    /// an equivalence since both directions matter (a false `None` drops a chapter; a false
    /// `Some` invents one).
    ///
    /// The `.{0,64}` bound is load-bearing: widening it re-admits digit runs that exhaust
    /// `f64`, which only [`parse_number_is_always_finite`] should own.
    #[test]
    fn parse_number_finds_a_number_exactly_when_one_is_present(text in ".{0,64}") {
        prop_assert_eq!(
            parse_number(&text).is_some(),
            text.chars().any(|c| c.is_ascii_digit()),
            "disagreement on {:?}", text
        );
    }

    /// With no chapter/episode marker, `parse_chapter_number` must degrade to exactly
    /// `parse_number` — divergence would parse the same listing differently depending on wording.
    #[test]
    fn without_a_marker_the_chapter_parser_is_the_plain_number_parser(text in ".*") {
        let lower = text.to_lowercase();
        prop_assume!(
            !["chapter", "episode", "chap", "ch.", "ch ", "#"]
                .iter()
                .any(|m| lower.contains(m))
        );
        prop_assert_eq!(parse_chapter_number(&text), parse_number(&text));
    }

    /// `relativize`'s output is stored in `chapters.path` and resolved later against the
    /// provider's `base_url`; an unrooted result would resolve against whatever path preceded
    /// it, silently pointing at the wrong page.
    ///
    /// The href strategy excludes `:` deliberately: a foreign scheme (e.g. `mailto:`) yields
    /// an unrooted result since `Url::join` honours it — a known, harmless hole named here
    /// rather than generated away silently.
    #[test]
    fn relativize_yields_a_rooted_path(page in page_url(), href in scheme_less_href()) {
        let path = relativize(&page, &href);
        prop_assert!(path.starts_with('/'), "{path:?} from href {href:?}");
    }

    /// `absolutize` normalises through `Url`, so applying it to its own output must be a no-op
    /// — covers are re-absolutized on several paths, and a non-idempotent version would mangle
    /// a URL the second time round.
    #[test]
    fn absolutize_is_idempotent(page in page_url(), href in scheme_less_href()) {
        let once = absolutize(&page, &href);
        prop_assert_eq!(absolutize(&page, &once), once.clone());
    }

    /// Unescaping only ever replaces an entity with something shorter, so output can never
    /// grow — a growing version would be an amplification primitive on an already-capped body.
    #[test]
    fn unescape_entities_never_grows_its_input(text in ".*") {
        prop_assert!(unescape_entities(&text).len() <= text.len());
    }

    /// Totality of the remaining provider-text helpers: none may panic on arbitrary input.
    #[test]
    fn the_text_helpers_are_total(text in ".*") {
        let _ = parse_year(&text);
        let _ = parse_ymd_date(&text);
        let _ = map_status(&text);
        let _ = split_attr(&text);
        let _ = unescape_entities(&text);
    }

    /// `split_attr` partitions its input: the selector is always a prefix of the spec, and a
    /// split-off attribute is always the suffix after the final `@` — a dropped or duplicated
    /// character here would silently select the wrong element.
    #[test]
    fn split_attr_partitions_the_spec(spec in "[a-zA-Z0-9@.# \\[\\]>-]{0,32}") {
        let (selector, attr) = split_attr(&spec);
        prop_assert!(spec.starts_with(selector), "{selector:?} is not a prefix of {spec:?}");
        if let Some(attr) = attr {
            prop_assert_eq!(
                format!("{selector}@{attr}"),
                spec.clone(),
                "the split did not reassemble"
            );
        } else {
            prop_assert_eq!(selector, spec.as_str());
        }
    }

    /// `relativize`/`absolutize` must not panic on a hostile or malformed href, including one
    /// carrying a foreign scheme.
    #[test]
    fn the_url_helpers_are_total(page in ".*", href in ".*") {
        let _ = relativize(&page, &href);
        let _ = absolutize(&page, &href);
    }
}
