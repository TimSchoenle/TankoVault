//! Scan execution engine — shared by the inline (one-shot) full/fast scans and the
//! `JetStream` task consumer. Every write goes through the idempotent
//! [`tankovault_db::repo::catalog::ingest_series`], so replays are safe.

use sha2::{Digest, Sha256};
use std::sync::Arc;
use std::time::Duration;
use tankovault_adapters::{ChapterMeta, Ctx, SeriesMeta, SourceAdapter, build_adapter};
use tankovault_bus::Bus;
use tankovault_contracts::{ChapterDiscovered, ScanTaskMessage, TaskKind};
use tankovault_db::PgPool;
use tankovault_db::repo::catalog::{ChapterUpsert, ScannedSeries, SeriesUpsert};
use tankovault_domain::{Provider, normalize_title};
use tankovault_fetch::{ProviderFetchConfig, RobotsRules, SessionStore, build_provider_fetcher};
use tankovault_solver::ChallengeSolver;
use time::OffsetDateTime;

/// Shared dependencies handed to the engine.
#[derive(Clone)]
pub(crate) struct Engine {
    pub(crate) pool: PgPool,
    pub(crate) bus: Option<Bus>,
    pub(crate) solver: Arc<dyn ChallengeSolver>,
    pub(crate) session_store: Arc<dyn SessionStore>,
    pub(crate) worker_id: String,
    /// Safety cap on catalogue pages walked per full scan.
    pub(crate) max_catalog_pages: u32,
}

impl Engine {
    /// Build the per-provider adapter + injected fetch stack + context.
    fn provider_context(
        &self,
        provider: &Provider,
    ) -> anyhow::Result<(Box<dyn SourceAdapter>, Ctx)> {
        let adapter = build_adapter(provider.adapter, &provider.slug, &provider.config)?;

        let robots = provider
            .robots_txt
            .as_deref()
            .map(|txt| RobotsRules::parse(txt, &provider.politeness.user_agent));

        let mut fetch_cfg = ProviderFetchConfig::new(
            provider.politeness.user_agent.clone(),
            self.solver.clone(),
            self.session_store.clone(),
        );
        fetch_cfg.rps = provider.politeness.rps;
        fetch_cfg.concurrency = provider.politeness.concurrency;
        let robots_delay_ms = robots
            .as_ref()
            .and_then(|r| r.crawl_delay)
            .filter(|d| d.is_finite() && *d > 0.0)
            .map(|d| Duration::from_secs_f64(d).as_millis())
            .and_then(|ms| u64::try_from(ms).ok())
            .unwrap_or(0);
        fetch_cfg.crawl_delay_ms = provider.politeness.crawl_delay_ms.max(robots_delay_ms);
        fetch_cfg.robots = robots;
        fetch_cfg.connect_timeout = Duration::from_secs(10);
        fetch_cfg.request_timeout = Duration::from_secs(30);

        let fetcher = build_provider_fetcher(fetch_cfg)?;
        let ctx = Ctx {
            base_url: provider.base_url.clone(),
            provider_slug: provider.slug.clone(),
            fetcher,
        };
        Ok((adapter, ctx))
    }

    /// Fetch, parse, and idempotently ingest one series; emit `chapter.discovered` for
    /// genuinely new chapters. Returns the count of new chapters.
    pub(crate) async fn process_series(
        &self,
        provider: &Provider,
        adapter: &dyn SourceAdapter,
        ctx: &Ctx,
        path: &str,
    ) -> anyhow::Result<usize> {
        let meta = adapter.fetch_series(ctx, path).await?;
        let chapters = adapter.fetch_chapters(ctx, path).await?;
        let hash = content_hash(&meta, &chapters);

        let scanned = ScannedSeries {
            provider_id: provider.id,
            source_path: path.to_owned(),
            provider_title: Some(meta.title.clone()),
            meta: SeriesUpsert {
                canonical_title: meta.title.clone(),
                normalized_title: normalize_title(&meta.title),
                description: meta.description.clone(),
                cover_url: meta.cover_url.clone(),
                content_type: meta.content_type,
                status: meta.status,
                release_year: meta.release_year,
            },
            alt_titles: meta
                .alt_titles
                .iter()
                .map(|t| (t.clone(), normalize_title(t)))
                .collect(),
            tags: meta.tags.clone(),
            authors: meta.authors.clone(),
            chapters: chapters
                .iter()
                .map(|c| ChapterUpsert {
                    number: c.number,
                    volume: None,
                    title: c.title.clone(),
                    path: c.path.clone(),
                    published_at: c.published_at,
                })
                .collect(),
            content_hash: hash,
        };

        let outcome = tankovault_db::repo::catalog::ingest_series(&self.pool, scanned).await?;

        if let Some(bus) = &self.bus {
            for number in &outcome.new_chapters {
                if let Some(ch) = chapters
                    .iter()
                    .find(|c| (c.number - number).abs() < f64::EPSILON)
                {
                    let event = ChapterDiscovered {
                        series_id: outcome.series_id,
                        series_source_id: outcome.source_id,
                        provider_id: provider.id,
                        provider_slug: provider.slug.clone(),
                        chapter_number: ch.number,
                        chapter_title: ch.title.clone(),
                        chapter_path: ch.path.clone(),
                        published_at: ch.published_at,
                        discovered_at: OffsetDateTime::now_utc(),
                    };
                    if let Err(e) = bus.publish_chapter(&event).await {
                        tracing::warn!(error = %e, "failed to publish chapter.discovered");
                    }
                }
            }
        }

        Ok(outcome.new_chapters.len())
    }

    /// One-shot full scan of a provider without the broker (the CLI `worker scan` path).
    ///
    /// Two phases, matching the broker fan-out (design §12, §20): **first** walk the entire
    /// catalogue, registering every series from its listing so the complete series list
    /// materialises up front; **then** fetch chapters + full metadata per series. A single
    /// series' malformed markup fails only that series, never the whole run.
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
                    tracing::warn!(page, error = %e, "catalog page failed; stopping walk");
                    truncated_by_page_cap = false;
                    break;
                }
            };
            if catalog.items.is_empty() {
                truncated_by_page_cap = false;
                break;
            }
            for item in &catalog.items {
                // Skip duplicates a paginator may repeat across pages.
                if !seen.insert(item.path.clone()) {
                    continue;
                }
                summary.series_seen += 1;
                if let Err(e) = tankovault_db::repo::catalog::register_source_stub(
                    &self.pool,
                    provider.id,
                    &item.path,
                    &item.title,
                )
                .await
                {
                    tracing::warn!(path = %item.path, error = %e, "series registration failed");
                }
                paths.push(item.path.clone());
            }
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
        }

        // Phase 2 — enrich: fetch chapters + full metadata for every collected series.
        for path in &paths {
            match self
                .process_series(provider, adapter.as_ref(), &ctx, path)
                .await
            {
                Ok(new) => summary.new_chapters += new,
                Err(e) => {
                    summary.series_failed += 1;
                    tracing::warn!(path = %path, error = %e, "series ingest failed");
                }
            }
        }
        Ok(summary)
    }

    /// One-shot fast scan: read the latest feed and ingest only series whose newest
    /// chapter exceeds what we have stored.
    pub(crate) async fn run_fast_scan_inline(
        &self,
        provider: &Provider,
    ) -> anyhow::Result<ScanSummary> {
        let (adapter, ctx) = self.provider_context(provider)?;
        let mut summary = ScanSummary::default();

        let updates = adapter.list_latest(&ctx).await?;
        for update in &updates {
            summary.series_seen += 1;
            // Ingest is idempotent and only reports genuinely new chapters (via the
            // `xmax = 0` predicate), so re-ingesting an unchanged series is cheap and
            // emits no false-new events. A stored-max/content-hash pre-gate is a
            // documented optimisation.
            match self
                .process_series(provider, adapter.as_ref(), &ctx, &update.path)
                .await
            {
                Ok(new) => summary.new_chapters += new,
                Err(e) => {
                    summary.series_failed += 1;
                    tracing::warn!(path = %update.path, error = %e, "fast-scan series failed");
                }
            }
        }
        Ok(summary)
    }

    /// Dispatch a single task received from `JetStream` (design §12).
    ///
    /// - `CatalogPage` **fans out**: it registers every series on the page immediately
    ///   (breadth-first "collect all series first"), enqueues a `Series` task per series,
    ///   and chains the next `CatalogPage` while the catalogue has more pages. This is what
    ///   makes a full scan walk the *whole* catalogue instead of only page 1.
    /// - `Series` fetches metadata + chapters and upserts (idempotent).
    /// - `LatestFeed` (fast scan) ingests each updated series inline.
    pub(crate) async fn dispatch_task(
        &self,
        provider: &Provider,
        task: &ScanTaskMessage,
    ) -> anyhow::Result<()> {
        let (adapter, ctx) = self.provider_context(provider)?;
        match task.kind {
            TaskKind::Series => {
                let path = task
                    .target
                    .get("path")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| anyhow::anyhow!("series task missing path"))?;
                self.process_series(provider, adapter.as_ref(), &ctx, path)
                    .await?;
            }
            TaskKind::CatalogPage => {
                let page = task
                    .target
                    .get("page")
                    .and_then(serde_json::Value::as_u64)
                    .and_then(|p| u32::try_from(p).ok())
                    .unwrap_or(1);
                let catalog = adapter.list_catalog(&ctx, page).await?;
                for item in &catalog.items {
                    // Breadth-first: register the series now so the full list is available
                    // before any chapters are fetched; a registration failure must not lose
                    // the series, so still enqueue its enrichment task.
                    if let Err(e) = tankovault_db::repo::catalog::register_source_stub(
                        &self.pool,
                        provider.id,
                        &item.path,
                        &item.title,
                    )
                    .await
                    {
                        tracing::warn!(path = %item.path, error = %e, "series registration failed");
                    }
                    self.enqueue_child(
                        task,
                        "series",
                        TaskKind::Series,
                        serde_json::json!({ "path": item.path }),
                    )
                    .await?;
                }
                // Chain the next page while the catalogue has more, bounded by the same
                // safety cap the inline walk uses so an unbounded paginator cannot loop.
                if catalog.has_next {
                    if page < self.max_catalog_pages {
                        self.enqueue_child(
                            task,
                            "catalog_page",
                            TaskKind::CatalogPage,
                            serde_json::json!({ "page": page + 1 }),
                        )
                        .await?;
                    } else {
                        tracing::warn!(
                            provider = %provider.slug,
                            page,
                            max_catalog_pages = self.max_catalog_pages,
                            "catalog fan-out stopped at the page safety cap while the catalogue \
                             still had more pages; increase worker.max_catalog_pages if this is \
                             a legitimately large site"
                        );
                    }
                }
            }
            TaskKind::LatestFeed => {
                let updates = adapter.list_latest(&ctx).await?;
                for update in &updates {
                    if let Err(e) = self
                        .process_series(provider, adapter.as_ref(), &ctx, &update.path)
                        .await
                    {
                        tracing::warn!(path = %update.path, error = %e, "latest series failed");
                    }
                }
            }
        }
        Ok(())
    }

    /// Fan out one child task of `parent`'s run: persist it, bump the run's task total, and
    /// publish it to the provider's `JetStream` subject.
    ///
    /// Task creation is idempotent on `(run_id, kind, target)`: if an identical child already
    /// exists — e.g. this `catalog_page` was redelivered and re-processed — `create_task`
    /// returns `None` and this is a no-op, so a redelivered page does not re-enqueue every
    /// series. The total is incremented **before** the parent completes (the caller completes
    /// it only after `dispatch_task` returns), so `done + failed` can never reach `total`
    /// while fan-out is in flight — the run cannot finalise early.
    async fn enqueue_child(
        &self,
        parent: &ScanTaskMessage,
        kind_str: &str,
        kind: TaskKind,
        target: serde_json::Value,
    ) -> anyhow::Result<()> {
        let Some(bus) = &self.bus else {
            anyhow::bail!("cannot fan out scan tasks without a broker");
        };
        let Some(task_id) =
            tankovault_db::repo::scans::create_task(&self.pool, parent.run_id, kind_str, &target)
                .await?
        else {
            // Identical child already enqueued (idempotent redelivery); nothing to do.
            return Ok(());
        };
        tankovault_db::repo::scans::add_total_tasks(&self.pool, parent.run_id, 1).await?;
        bus.publish_task(&ScanTaskMessage {
            task_id,
            run_id: parent.run_id,
            provider_id: parent.provider_id,
            provider_slug: parent.provider_slug.clone(),
            mode: parent.mode,
            kind,
            target,
            traceparent: None,
        })
        .await?;
        Ok(())
    }

    /// Publish a compact [`tankovault_contracts::ProgressEvent`] for `run_id` by reading the
    /// authoritative counters from `scan_runs`. Called after each task settles so the
    /// control-plane aggregator can finalise the run and the console SSE can relay live
    /// progress over NATS instead of DB-polling (design §12). Best-effort: a broker or DB
    /// hiccup is logged, never fatal to task processing.
    pub(crate) async fn report_progress(&self, run_id: tankovault_domain::ScanRunId) {
        let Some(bus) = &self.bus else { return };
        let run = match tankovault_db::repo::scans::get_run(&self.pool, run_id).await {
            Ok(run) => run,
            Err(e) => {
                tracing::warn!(%run_id, error = %e, "progress: failed to read run");
                return;
            }
        };
        let event = tankovault_contracts::ProgressEvent {
            run_id: run.id,
            provider_id: run.provider_id,
            mode: run.mode,
            state: run.state,
            total_tasks: run.total_tasks,
            done_tasks: run.done_tasks,
            failed_tasks: run.failed_tasks,
            at: OffsetDateTime::now_utc(),
        };
        if let Err(e) = bus.publish_progress(&event).await {
            tracing::warn!(%run_id, error = %e, "progress: failed to publish event");
        }
    }
}

/// Summary of an inline scan.
#[derive(Debug, Default)]
pub(crate) struct ScanSummary {
    pub(crate) series_seen: usize,
    pub(crate) series_failed: usize,
    pub(crate) new_chapters: usize,
}

/// Content hash over title + chapter (number, path) pairs, for cheap change detection.
fn content_hash(meta: &SeriesMeta, chapters: &[ChapterMeta]) -> Vec<u8> {
    let mut h = Sha256::new();
    h.update(meta.title.as_bytes());
    if let Some(desc) = &meta.description {
        h.update(desc.as_bytes());
    }
    for c in chapters {
        h.update(c.number.to_le_bytes());
        h.update(b"|");
        h.update(c.path.as_bytes());
        h.update(b"\n");
    }
    h.finalize().to_vec()
}
