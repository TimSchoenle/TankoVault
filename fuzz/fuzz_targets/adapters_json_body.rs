//! **F-T2** — `tankovault_adapters::json::parse_json_body`, the recovery path for a provider
//! JSON API whose response did not arrive as JSON.
//!
//! A challenge solver returns what a headless browser *displayed*, so a JSON payload can come
//! back inside a `<pre>` block, entity-escaped, or shredded into a browser JSON viewer's
//! per-token markup. `parse_json_body` scans the body for balanced JSON objects and tries each
//! one. That scan is the audit's second verified defect (F-02): it used to re-scan to the end
//! of the document from every `{`, which is quadratic, and a 600 KB body of nested braces took
//! ~30 s against a fetch cap that admits 8 MiB.
//!
//! # Oracle
//!
//! Two, and the second is the point of this target:
//!
//! 1. **No panic.** `strip_tags` and the brace walk index a `&str` by byte offset.
//! 2. **Completion inside libFuzzer's `-timeout`.** Run this target with `-timeout=2` and
//!    `-rss_limit_mb=512`. A property test cannot express "finishes"; a wall-clock oracle can,
//!    and F-02 is precisely the class of bug it catches. The README's invocation sets both —
//!    without `-timeout`, the target still runs but has lost half its value.

#![no_main]

use libfuzzer_sys::fuzz_target;
use tankovault_fetch::FetchResponse;

fuzz_target!(|data: &str| {
    // Shaped like what the fetch stack hands an adapter. `status: 200` matters: a non-success
    // status is rejected by `Ctx::fetch` before the body is ever parsed, so 200 is the only
    // status on which this code runs.
    let resp = FetchResponse {
        status: 200,
        url: "https://provider.test/api/comics/some-series/chapters?page=1".to_owned(),
        headers: vec![("content-type".to_owned(), "application/json".to_owned())],
        body: data.to_owned(),
        from_cache: false,
    };

    let _ = tankovault_adapters::__fuzz::parse_json_body_value("fuzz target", &resp);
});
