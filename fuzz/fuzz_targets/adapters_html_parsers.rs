//! **F-T1** — the text and URL helpers in `tankovault_adapters::html`, over arbitrary UTF-8.
//!
//! Every one of these functions is handed a string that came off a provider's page: anchor
//! text, an attribute value, a status label, a date cell. Nothing between the socket and here
//! constrains the character set, and this is where the audit's first verified crash lived —
//! `parse_chapter_number` panicked on `U+0130` (LATIN CAPITAL LETTER I WITH DOT ABOVE),
//! because `to_lowercase()` expands it to two chars and the byte offsets computed before the
//! lowercase were then applied to the string after it. Every fixture in the repository is
//! ASCII, so no example test could have found it. This target is the general form of that
//! guard.
//!
//! # Oracle
//!
//! No panic, on any input. That is the whole assertion: these are total functions returning
//! `Option`/`String`/an enum, so *any* abort is a bug. Nothing here is checked for a
//! *correct* answer — the algebraic side of that lives in
//! `crates/adapters/tests/prop_html.rs` (P-01), which runs on stable in the ordinary test job.
//!
//! `parse_selector` is deliberately absent even though it is `pub` and takes provider-supplied
//! text: it writes into a process-wide bounded memo, so consecutive fuzz iterations would stop
//! being independent and a reproducer would depend on execution order. Its bound is pinned by
//! a unit test instead.

#![no_main]

use libfuzzer_sys::fuzz_target;
use tankovault_adapters::html;

/// A fixed page URL for the two URL helpers. Fuzzing the *base* as well would mostly explore
/// the `url` crate's parser rather than our resolution logic; `prop_html.rs` covers the
/// both-arguments-arbitrary case on stable.
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

    // The composition the adapters actually perform: a link is relativised for storage and
    // later absolutised again to be fetched. Each half is exercised above; this is the pair,
    // which is where an offset bug in one shows up as a panic in the other.
    let _ = html::absolutize(PAGE_URL, &html::relativize(PAGE_URL, data));
});
