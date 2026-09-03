//! Fixture tests for the prose guard every adapter is wrapped in.
//!
//! The markup is trimmed from `en-thunderscans.com`, which sells seven text novels from the
//! `/comics/` catalogue that carries its comics and marks the medium in the listed title alone —
//! its `span.type` badge is commented out of the template. Both assertions run the **shipped**
//! preset through `build_adapter`, because the guard lives at that seam and a test that built
//! the adapter directly would not exercise it.

use async_trait::async_trait;
use std::sync::Arc;
use tankovault_adapters::{Ctx, SourceAdapter, build_adapter, builtin_presets};
use tankovault_fetch::{FetchError, FetchRequest, FetchResponse, Fetcher};

const CATALOG: &str = include_str!("../fixtures/thunderscans/catalog.html");
const NOVELS_ONLY: &str = include_str!("../fixtures/thunderscans/novels-only.html");

/// Serves one body for every request.
struct OneBody(&'static str);

#[async_trait]
impl Fetcher for OneBody {
    async fn get(&self, req: FetchRequest) -> Result<FetchResponse, FetchError> {
        Ok(FetchResponse {
            status: 200,
            url: req.url.clone(),
            headers: Vec::new(),
            body: self.0.to_owned(),
            from_cache: false,
        })
    }
}

/// The shipped `thunderscans` preset, paired with a context serving `body`.
fn thunderscans(body: &'static str) -> (Box<dyn SourceAdapter>, Ctx) {
    let preset = builtin_presets()
        .into_iter()
        .find(|p| p.slug == "thunderscans")
        .expect("no shipped preset named thunderscans");
    let adapter = build_adapter(preset.adapter, preset.slug, &preset.config)
        .expect("thunderscans preset failed to build");
    let ctx = Ctx {
        base_url: preset.base_url.to_owned(),
        provider_slug: preset.slug.to_owned(),
        fetcher: Arc::new(OneBody(body)) as Arc<dyn Fetcher>,
    };
    (adapter, ctx)
}

/// Regression: the novels were registered as ordinary series, so a reader following one reached
/// a page of prose and every scan tracked chapters that have nothing to read. The fixture also
/// carries *The Novel's Extra (Remake)* — a comic whose title a substring test would drop.
#[tokio::test]
async fn a_catalogue_row_the_site_marks_as_a_novel_is_not_a_series() {
    let (adapter, ctx) = thunderscans(CATALOG);

    let page = adapter
        .list_catalog(&ctx, 1)
        .await
        .expect("catalogue parses");
    let titles: Vec<&str> = page.items.iter().map(|i| i.title.as_str()).collect();

    assert_eq!(
        titles,
        vec!["The Divine Brush", "The Novel\u{2019}s Extra (Remake)"],
        "the two [Novel] rows are dropped and the comic naming a novel is kept"
    );

    let updates = adapter.list_latest(&ctx).await.expect("feed parses");
    assert_eq!(
        updates.len(),
        2,
        "the feed reads the same listing and must agree with it: {:?}",
        updates.iter().map(|u| &u.title).collect::<Vec<_>>()
    );
}

/// The invariant that makes the guard safe to apply everywhere: it filters what the inner
/// adapter yielded, never what the inner adapter concluded. This install's paginator is
/// commented out of the markup, so `has_next` falls back to "the page yielded items" — and a
/// guard that emptied the page first would end the walk with the rest of the catalogue unseen.
#[tokio::test]
async fn a_page_of_nothing_but_novels_does_not_end_the_catalogue_walk() {
    let (adapter, ctx) = thunderscans(NOVELS_ONLY);

    let page = adapter
        .list_catalog(&ctx, 1)
        .await
        .expect("catalogue parses");

    assert!(page.items.is_empty(), "every row on the page is prose");
    assert!(page.has_next, "the walk continues past a page of prose");
}
