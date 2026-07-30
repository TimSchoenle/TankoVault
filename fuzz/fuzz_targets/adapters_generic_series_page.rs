//! **F-T3** — the `scraper`-driven extraction in `GenericConfigAdapter`, end to end against a
//! real shipped preset, on malformed upstream HTML.
//!
//! Targets 1 and 2 fuzz the leaf helpers. This one fuzzes the *composition*: html5ever's tree
//! builder repairing a broken document, then `parse_selector`/`extract_first`/`extract_all`
//! walking the repaired tree, then `parse_chapter_number`/`parse_year`/`map_status` running on
//! whatever text came back. That last hand-off is where F-01 actually lived in production — the
//! panicking string was not a fixture, it was an anchor's text content — and no leaf-level
//! target reproduces the tree builder's contribution to it (element reconstruction can
//! *duplicate* an `<a>` across siblings, so the text a selector yields need not be a substring
//! of the input).
//!
//! The preset is `manhuaus`, the plain Madara one: its selectors are the shared defaults, so
//! this is the configuration every config-driven provider inherits rather than one site's
//! overrides. `demonicscans` and `kunmanga` are custom adapters and would each need their own
//! target; `kunmanga`'s chapter walk is a paginated loop, which needs a fetcher that terminates
//! it rather than one that answers every URL identically.
//!
//! # Oracle
//!
//! `Ok` or a typed [`AdapterError`] — never a panic, and never a hang. No stronger assertion is
//! made on the *contents*, deliberately: the extracted values are asserted against real markup
//! by `crates/adapters/tests/madara_presets_fixture.rs`, and asserting a shape here would only
//! re-encode the selectors.

#![no_main]

use async_trait::async_trait;
use libfuzzer_sys::fuzz_target;
use std::sync::{Arc, LazyLock};
use tankovault_adapters::{Ctx, SourceAdapter, build_adapter, builtin_presets};
use tankovault_fetch::{FetchError, FetchRequest, FetchResponse, Fetcher};

const PRESET: &str = "manhuaus";

/// Answers every request with the same fuzz input.
///
/// Sound only because each call driven below fetches exactly one document. It is the reason
/// this target does not drive `kunmanga`: a paginated walk against a fetcher that never runs
/// out of pages does not terminate, and libFuzzer would report that as a timeout in the
/// parser.
struct OneBody(Arc<str>);

#[async_trait]
impl Fetcher for OneBody {
    async fn get(&self, req: FetchRequest) -> Result<FetchResponse, FetchError> {
        Ok(FetchResponse {
            status: 200,
            url: req.url,
            headers: Vec::new(),
            body: self.0.to_string(),
            from_cache: false,
        })
    }
}

/// One runtime for the whole campaign. `parse_blocking` hands the parse to `spawn_blocking`
/// (PERF-9: html5ever must not run on a Tokio worker), so a runtime is required; building one
/// per iteration would dominate the measurement and starve the fuzzer of executions.
static RT: LazyLock<tokio::runtime::Runtime> =
    LazyLock::new(|| tokio::runtime::Runtime::new().expect("build a Tokio runtime"));

/// The preset adapter, built once. Its config is a fixed set of selector strings, so nothing
/// about it varies per input.
static ADAPTER: LazyLock<(Box<dyn SourceAdapter>, String, String)> = LazyLock::new(|| {
    let preset = builtin_presets()
        .into_iter()
        .find(|p| p.slug == PRESET)
        .expect("the manhuaus preset is shipped");
    let adapter = build_adapter(preset.adapter, preset.slug, &preset.config)
        .expect("the manhuaus preset builds an adapter");
    (adapter, preset.base_url.to_owned(), preset.slug.to_owned())
});

fuzz_target!(|data: &str| {
    let (adapter, base_url, slug) = &*ADAPTER;
    let ctx = Ctx {
        base_url: base_url.clone(),
        provider_slug: slug.clone(),
        fetcher: Arc::new(OneBody(Arc::from(data))),
    };

    RT.block_on(async {
        // All four entry points: each parses the same document through a different selector
        // set, and `list_catalog`/`list_latest` are the two that build links.
        let _ = adapter.list_catalog(&ctx, 1).await;
        let _ = adapter.list_latest(&ctx).await;
        let _ = adapter.fetch_series(&ctx, "/manga/some-series/").await;
        let _ = adapter.fetch_chapters(&ctx, "/manga/some-series/").await;
    });
});
