//! Fuzzes `tankovault_adapters::json::parse_json_body`, the recovery path for a provider JSON
//! API response that didn't arrive as raw JSON (challenge-solver `<pre>` blocks, entity-escaped,
//! or shredded JSON-viewer markup).
//!
//! # Oracle
//! 1. No panic: `strip_tags` and the brace walk index a `&str` by byte offset.
//! 2. Completion inside libFuzzer's `-timeout` (run with `-timeout=2 -rss_limit_mb=512`): the
//!    body scan was once quadratic in body length, and a wall-clock oracle is what catches
//!    that class of bug — a property test can't express "finishes".

#![no_main]

use libfuzzer_sys::fuzz_target;
use tankovault_fetch::FetchResponse;

fuzz_target!(|data: &str| {
    // Shaped like what the fetch stack hands an adapter; `status: 200` matters since a
    // non-success status is rejected by `Ctx::fetch` before the body is ever parsed.
    let resp = FetchResponse {
        status: 200,
        url: "https://provider.test/api/comics/some-series/chapters?page=1".to_owned(),
        headers: vec![("content-type".to_owned(), "application/json".to_owned())],
        body: data.to_owned(),
        from_cache: false,
    };

    let _ = tankovault_adapters::__fuzz::parse_json_body_value("fuzz target", &resp);
});
