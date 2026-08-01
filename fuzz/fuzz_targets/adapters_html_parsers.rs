//! Fuzzes the text/URL helpers in `tankovault_adapters::html` over arbitrary UTF-8, the shape
//! of text arriving off a provider's page (anchor text, attributes, status labels).
//!
//! # Oracle
//! No panic on any input: these are total functions returning `Option`/`String`/an enum, so any
//! abort is a bug. Correctness (not just totality) is covered by the property tests in
//! `crates/adapters/tests/prop_html.rs`.
//!
//! `parse_selector` is deliberately excluded even though it's `pub` and provider-supplied: it
//! writes into a process-wide memo, so consecutive iterations wouldn't be independent. Its
//! bound is pinned by a unit test instead.

#![no_main]

use libfuzzer_sys::fuzz_target;
use tankovault_adapters::html;

/// A fixed page URL for the two URL helpers; fuzzing the base too would mostly explore the
/// `url` crate's parser, not our resolution logic (`prop_html.rs` covers that case).
const PAGE_URL: &str = "https://provider.test/manga/some-series/";

fuzz_target!(|data: &str| {
    let _ = html::parse_chapter_number(data);
    let _ = html::parse_number(data);
    let _ = html::parse_year(data);
    let _ = html::parse_ymd_date(data);
    let _ = html::map_status(data);
    let _ = html::unescape_entities(data);
    let _ = html::split_attr(data);
    let _ = html::relativize(PAGE_URL, data);
    let _ = html::absolutize(PAGE_URL, data);

    // The composition adapters actually perform: relativised then absolutised again, where an
    // offset bug in one would show up as a panic in the other.
    let _ = html::absolutize(PAGE_URL, &html::relativize(PAGE_URL, data));
});
