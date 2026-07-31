//! Totality and algebraic properties of the HTML/text extraction helpers.
//!
//! Every function here is called on **provider-controlled** text: anchor labels, attribute
//! values, status captions, date cells. The existing unit tests are all hand-written ASCII,
//! which is exactly the blind spot that hid F-01 — `parse_chapter_number` panicked on any
//! title containing `U+0130`, because a byte offset found in a `to_lowercase` copy was applied
//! to the original string. That bug lived behind three green tests.
//!
//! These properties are the standing version of that check: they run over arbitrary Unicode on
//! every `cargo test`, on stable, with no extra toolchain. A panic here is a worker task dying
//! mid-scan on a page a scanlation site can publish at will.

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
/// Generated structurally rather than hoped for. A regex strategy will not produce a 300-digit
/// run at any realistic size, which is exactly how the infinity defect survived a file already
/// full of properties over `".*"` — see [`parse_number_is_always_finite`]. Prop-b is the
/// standing reminder that a strategy which cannot generate the case does not test it.
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
    /// The direct, stable-toolchain guard for F-01. `parse_chapter_number` lowercases its
    /// input and then indexes into a string; `str::to_lowercase` is not length-preserving
    /// (`"İ"` is two bytes and folds to three), so any offset arithmetic across the two is a
    /// panic waiting for a Turkish, Vietnamese or combining-mark chapter title.
    #[test]
    fn parse_chapter_number_never_panics(text in ".*") {
        let _ = parse_chapter_number(&text);
    }

    /// Every number this module yields is **finite**.
    ///
    /// Regression, and the reason the strategy above exists. `parse_number` ended in
    /// `.parse::<f64>().ok()`, and Rust's float parser answers `Ok(inf)` — not `Err` — for a
    /// decimal outside `f64`'s range. So `"Chapter 999…9"` produced an *infinite* chapter
    /// number, which stores fine in a `double precision` column, then freezes `latest_chapter`
    /// forever (nothing exceeds `inf`) and serialises to `null` in the `chapter.discovered`
    /// message, which the notifier drops as undecodable. `crates/adapters/src/html.rs` carries
    /// the full note as `an_unrepresentable_digit_run_is_no_number_at_all`.
    ///
    /// Asserted over both the ordinary and the overlong generator, because the invariant is
    /// about every input, not only the pathological one.
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

    /// `parse_number` finds a number exactly when there is an ASCII digit to find. Stated as
    /// an equivalence because both directions matter: a `None` on text that does have a digit
    /// silently drops a chapter, and a `Some` on text that has none invents one.
    ///
    /// The bound on the strategy is load-bearing, not cosmetic: the equivalence is only true of
    /// a *representable* number, and 64 characters cannot hold a digit run that exhausts `f64`
    /// (~1e64 against a limit near 1e308). Widening it re-admits the case
    /// [`parse_number_is_always_finite`] owns, and this property would then be the one that
    /// fails. The bound lives here rather than in `proptest`'s default `".*"` expansion so a
    /// version bump cannot move it.
    #[test]
    fn parse_number_finds_a_number_exactly_when_one_is_present(text in ".{0,64}") {
        prop_assert_eq!(
            parse_number(&text).is_some(),
            text.chars().any(|c| c.is_ascii_digit()),
            "disagreement on {:?}", text
        );
    }

    /// With no chapter/episode marker present there is nothing to prefer, so
    /// `parse_chapter_number` must degrade to exactly `parse_number`. If these two ever
    /// diverge on unmarked text, the same listing parses differently depending on wording.
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

    /// The value `relativize` returns is stored in `chapters.path` and later resolved against
    /// the provider's `base_url`. A result that is not rooted would resolve relative to
    /// whatever path happened to precede it, silently pointing at the wrong page.
    ///
    /// The href strategy excludes `:` deliberately: `relativize(page, "mailto:a@b")` returns
    /// `"a@b"`, unrooted, because `Url::join` honours the foreign scheme. That is a real
    /// (harmless-today) hole in the contract rather than something this property should
    /// paper over, so it is named here instead of being generated away silently.
    #[test]
    fn relativize_yields_a_rooted_path(page in page_url(), href in scheme_less_href()) {
        let path = relativize(&page, &href);
        prop_assert!(path.starts_with('/'), "{path:?} from href {href:?}");
    }

    /// `absolutize` normalises through `Url`, so applying it to its own output must be a
    /// no-op. Covers are re-absolutized on several paths; a non-idempotent version would
    /// mangle a CDN URL the second time round.
    #[test]
    fn absolutize_is_idempotent(page in page_url(), href in scheme_less_href()) {
        let once = absolutize(&page, &href);
        prop_assert_eq!(absolutize(&page, &once), once.clone());
    }

    /// Unescaping only ever replaces an entity with something shorter, so the output can never
    /// grow. A version that could grow would be an amplification primitive on a body already
    /// capped at 8 MiB.
    #[test]
    fn unescape_entities_never_grows_its_input(text in ".*") {
        prop_assert!(unescape_entities(&text).len() <= text.len());
    }

    /// Totality of the remaining provider-text helpers. None of these may panic, whatever a
    /// site puts in an attribute or a status caption.
    #[test]
    fn the_text_helpers_are_total(text in ".*") {
        let _ = parse_year(&text);
        let _ = parse_ymd_date(&text);
        let _ = map_status(&text);
        let _ = split_attr(&text);
        let _ = unescape_entities(&text);
    }

    /// `split_attr` partitions its input: the selector part is always a prefix of the spec, and
    /// when an attribute is split off it is always the suffix after the final `@`. A split that
    /// dropped or duplicated characters would silently select the wrong element.
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

    /// `relativize` and `absolutize` are called with whatever the page said; neither may panic
    /// on a hostile or malformed `href`, including one carrying a foreign scheme.
    #[test]
    fn the_url_helpers_are_total(page in ".*", href in ".*") {
        let _ = relativize(&page, &href);
        let _ = absolutize(&page, &href);
    }
}
