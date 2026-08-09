//! The inline (one-shot) full and fast scan entry points.

use tankovault_domain::Provider;

use super::{Engine, ScanSummary, StageReporter, catalog_truncated};

impl Engine {
    /// One-shot full scan of a provider without the broker (the CLI `worker scan` path).
    ///
    /// Two phases: walk the entire catalogue registering every series, then fetch
    /// chapters + metadata per series. A malformed series fails only that series.
    pub(crate) async fn run_full_scan_inline(
        &self,
        provider: &Provider,
    ) -> anyhow::Result<ScanSummary> {
        let (adapter, ctx) = self.provider_context(provider)?;
        let mut summary = ScanSummary::default();

        // Phase 1 — collect ALL series: walk every catalogue page and register each series
        // immediately (metadata-light, no chapters).
        let mut paths: Vec<String> = Vec::new();
        let mut seen = std::collections::HashSet::new();
        let mut truncated_by_page_cap = true;
        for page in 1..=self.max_catalog_pages {
            let catalog = match adapter.list_catalog(&ctx, page).await {
                Ok(c) => c,
                Err(e) => {
                    tracing::warn!(
                        provider = %provider.slug,
                        page,
                        pages_walked = page.saturating_sub(1),
                        series_collected = paths.len(),
                        error = %e,
                        next = "catalogue walk stops here; the series already collected are \
                                still enriched, later pages are not seen this run",
                        "catalog page failed"
                    );
                    truncated_by_page_cap = false;
                    break;
                }
            };
            if catalog.items.is_empty() {
                truncated_by_page_cap = false;
                break;
            }
            // Skip cross-page duplicates, then register the rest in one batch — a
            // 20k-entry sitemap page costs one existence query, not 20k.
            let fresh: Vec<(&str, &str)> = catalog
                .items
                .iter()
                .filter(|item| seen.insert(item.path.clone()))
                .map(|item| (item.path.as_str(), item.title.as_str()))
                .collect();
            summary.series_seen += fresh.len();
            match tankovault_db::repo::catalog::register_source_stubs(
                &self.pool,
                provider.id,
                &fresh,
                &self.matching,
            )
            .await
            {
                Ok(registered) => tracing::info!(
                    provider = %provider.slug,
                    page,
                    items = catalog.items.len(),
                    fresh = fresh.len(),
                    registered,
                    "catalog page walked"
                ),
                Err(e) => tracing::warn!(
                    provider = %provider.slug,
                    page,
                    items = fresh.len(),
                    error = %e,
                    next = "the page's series are still enriched below, which creates their \
                            source rows",
                    "catalog page registration failed"
                ),
            }
            paths.extend(fresh.iter().map(|(path, _)| (*path).to_owned()));
            if !catalog.has_next {
                truncated_by_page_cap = false;
                break;
            }
        }
        if truncated_by_page_cap {
            tracing::warn!(
                provider = %provider.slug,
                max_catalog_pages = self.max_catalog_pages,
                "catalog walk stopped at the page safety cap while the catalogue still had more \
                 pages; increase worker.max_catalog_pages if this is a legitimately large site"
            );
            catalog_truncated(&provider.slug);
        }

        // Phase 2 — enrich: fetch chapters + full metadata for every collected series.
        //
        // Detached: this path has no task row to report a stage to. The reporter still travels
        // because `process_series` is shared with the broker path, where it does.
        let stage = StageReporter::detached(self.pool.clone());
        for path in &paths {
            match self
                .process_series(provider, adapter.as_ref(), &ctx, path, &stage)
                .await
            {
                Ok(new) => summary.new_chapters += new,
                Err(e) => {
                    summary.series_failed += 1;
                    tracing::warn!(
                        provider = %provider.slug,
                        target = %path,
                        error = %e,
                        next = "series skipped; the rest of the scan continues and the next run \
                                retries it",
                        "series ingest failed"
                    );
                }
            }
        }
        Ok(summary)
    }

    /// One-shot fast scan: read the latest feed and re-ingest each updated series.
    pub(crate) async fn run_fast_scan_inline(
        &self,
        provider: &Provider,
    ) -> anyhow::Result<ScanSummary> {
        let (adapter, ctx) = self.provider_context(provider)?;
        let mut summary = ScanSummary::default();

        let updates = adapter.list_latest(&ctx).await?;
        let stage = StageReporter::detached(self.pool.clone());
        for update in &updates {
            summary.series_seen += 1;
            // Ingest is idempotent and reports only genuinely new chapters (via
            // `xmax = 0`), so re-ingesting an unchanged series emits no false-new events.
            match self
                .process_series(provider, adapter.as_ref(), &ctx, &update.path, &stage)
                .await
            {
                Ok(new) => summary.new_chapters += new,
                Err(e) => {
                    summary.series_failed += 1;
                    tracing::warn!(
                        provider = %provider.slug,
                        target = %update.path,
                        error = %e,
                        next = "series skipped; the rest of the feed continues and the next fast \
                                scan retries it",
                        "fast-scan series failed"
                    );
                }
            }
        }
        Ok(summary)
    }
}
