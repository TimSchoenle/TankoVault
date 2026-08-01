//! Fuzzes `GenericConfigAdapter`'s full extraction pipeline — html5ever's tree repair, then
//! selector walking, then text parsing — against the `manhuaus` preset (the plain Madara
//! defaults every config-driven provider inherits) on malformed upstream HTML.
//!
//! Leaf-level fuzzing alone misses the tree builder's contribution: element reconstruction can
//! duplicate an `<a>` across siblings, so extracted text need not be a substring of the input.
//! `demonicscans`/`kunmanga` are excluded: they're custom adapters, and `kunmanga`'s chapter
//! walk is a paginated loop needing a fetcher that terminates rather than one that repeats.
//!
//! # Oracle
//! `Ok` or a typed [`AdapterError`] — never a panic, never a hang. Content correctness is
//! covered by `crates/adapters/tests/madara_presets_fixture.rs`.

#![no_main]

use async_trait::async_trait;
use libfuzzer_sys::fuzz_target;
use std::sync::{Arc, LazyLock};
use tankovault_adapters::{Ctx, SourceAdapter, build_adapter, builtin_presets};
use tankovault_fetch::{FetchError, FetchRequest, FetchResponse, Fetcher};

const PRESET: &str = "manhuaus";

/// Answers every request with the same fuzz input.
///
/// Sound only because each call fetches exactly one document — a paginated walk against this
/// would never terminate, which is why this target excludes `kunmanga`.
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

/// One runtime for the whole campaign: `parse_blocking` requires one (html5ever must not run
/// on a Tokio worker), and building one per iteration would dominate the measurement.
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
