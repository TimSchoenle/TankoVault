//! Scan execution engine — shared by the inline (one-shot) full/fast scans and the
//! `JetStream` task consumer. Every write goes through the idempotent
//! [`tankovault_db::repo::catalog::ingest_series`], so replays are safe.

use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::hash::{Hash as _, Hasher as _};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tankovault_adapters::{ChapterMeta, Ctx, SeriesMeta, SourceAdapter, build_adapter};
use tankovault_bus::Bus;
use tankovault_contracts::{ChapterDiscovered, ScanTaskMessage, TaskKind};
use tankovault_db::PgPool;
use tankovault_db::repo::catalog::{ChapterUpsert, ScannedSeries, SeriesUpsert};
use tankovault_domain::{Provider, ProviderId, normalize_title};
use tankovault_fetch::{Fetcher, ProviderFetchConfig, SessionStore, build_provider_fetcher};
use tankovault_solver::ChallengeSolver;
use time::OffsetDateTime;

/// Children enqueued per statement when fanning out a catalogue page.
///
/// A catalogue "page" is whatever the adapter chooses to return: for most providers that is
/// a dozen entries, but a sitemap-driven adapter (kunmanga) yields up to 20k in one page.
/// Chunking keeps any single INSERT bounded and lets `total_tasks` advance while a large
/// page is still fanning out, instead of in one jump at the end.
const FANOUT_CHUNK: usize = 1_000;

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
    /// One fetch stack per provider, keyed by the politeness settings it was built from.
    ///
    /// This cache is load-bearing for **correctness**, not only speed. `RateLimitedFetcher`
    /// owns the governor cell and the semaphore, and `Throttle` owns the adaptive 429
    /// penalty — so a fetcher built per task made the configured `rps` and `concurrency` a
    /// *per-task* budget. N concurrent tasks therefore offered N × rps to the provider, which
    /// is what produced the 429 storms the backoff layer then spent wall-clock absorbing, and
    /// the accumulated penalty was thrown away every task. The comment at
    /// `crates/fetch/src/ratelimit.rs` claiming a per-provider limiter was simply false.
    ///
    /// The speed half: each rebuild also meant a fresh `wreq::Client` with its own connection
    /// pool, so every task paid a TCP + TLS 1.3 handshake before its first byte — roughly
    /// 500k handshakes on a full scan that should have needed about `concurrency` of them.
    fetchers: Arc<Mutex<HashMap<ProviderId, CachedFetcher>>>,
}

/// Hash the provider settings a fetch stack is built from.
///
/// Only the inputs to `build_provider_fetcher`, so a change to an unrelated column (a display
/// name, an adapter config key) does not throw away a warm connection pool and a rate limiter
/// mid-run — while an operator lowering `rps` or switching emulation profile does take effect
/// on the next task rather than at the next restart.
fn politeness_fingerprint(provider: &Provider) -> u64 {
    let p = &provider.politeness;
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    // `f64` is not `Hash`; its bit pattern is, and equality of bit patterns is exactly the
    // "unchanged" test wanted here.
    p.rps.to_bits().hash(&mut hasher);
    p.concurrency.hash(&mut hasher);
    p.crawl_delay_ms.hash(&mut hasher);
    p.user_agent.hash(&mut hasher);
    format!("{:?}", p.emulation).hash(&mut hasher);
    hasher.finish()
}

/// A built fetch stack plus the fingerprint of the settings that produced it.
struct CachedFetcher {
    /// Politeness + base URL as the provider row had them when this stack was built. An
    /// operator lowering `rps` mid-run must take effect, so the entry is rebuilt on change
    /// rather than pinned for the process lifetime.
    fingerprint: u64,
    fetcher: Arc<dyn Fetcher>,
}

impl Engine {
    /// Assemble the engine with an empty fetcher cache.
    ///
    /// A constructor rather than a struct literal so [`Engine::fetchers`] stays an
    /// implementation detail — callers have no reason to know the cache exists, and one that
    /// could be constructed pre-populated would be a way to get the sharing wrong.
    pub(crate) fn new(
        pool: PgPool,
        bus: Option<Bus>,
        solver: Arc<dyn ChallengeSolver>,
        session_store: Arc<dyn SessionStore>,
        worker_id: String,
        max_catalog_pages: u32,
    ) -> Self {
        Self {
            pool,
            bus,
            solver,
            session_store,
            worker_id,
            max_catalog_pages,
            fetchers: Arc::default(),
        }
    }

    /// Build the fetch stack for `provider`, reusing the cached one when its settings are
    /// unchanged.
    fn fetcher_for(&self, provider: &Provider) -> anyhow::Result<Arc<dyn Fetcher>> {
        let fingerprint = politeness_fingerprint(provider);

        // A short critical section around a `HashMap`, held across no await point: a `std`
        // mutex is the right primitive, and using `tokio::sync::RwLock` here would make
        // `provider_context` async for no benefit.
        {
            let cache = self.fetchers.lock().expect("fetcher cache mutex poisoned");
            if let Some(entry) = cache.get(&provider.id) {
                if entry.fingerprint == fingerprint {
                    return Ok(Arc::clone(&entry.fetcher));
                }
            }
        }

        let mut fetch_cfg = ProviderFetchConfig::new(
            provider.politeness.user_agent.clone(),
            self.solver.clone(),
            self.session_store.clone(),
        );
        fetch_cfg.emulation = provider.politeness.emulation;
        fetch_cfg.rps = provider.politeness.rps;
        fetch_cfg.concurrency = provider.politeness.concurrency;
        fetch_cfg.crawl_delay_ms = provider.politeness.crawl_delay_ms;
        fetch_cfg.connect_timeout = Duration::from_secs(10);
        fetch_cfg.request_timeout = Duration::from_secs(30);
        let fetcher = build_provider_fetcher(fetch_cfg)?;

        let mut cache = self.fetchers.lock().expect("fetcher cache mutex poisoned");
        // Another task may have built one while this one was constructing. Either is
        // correct, but keeping the stored entry means both callers share one limiter, which
        // is the entire point — so only insert if it is still absent or stale.
        let entry = cache
            .entry(provider.id)
            .and_modify(|existing| {
                if existing.fingerprint != fingerprint {
                    *existing = CachedFetcher {
                        fingerprint,
                        fetcher: Arc::clone(&fetcher),
                    };
                }
            })
            .or_insert_with(|| CachedFetcher {
                fingerprint,
                fetcher: Arc::clone(&fetcher),
            });
        Ok(Arc::clone(&entry.fetcher))
    }

    /// Build the per-provider adapter + injected fetch stack + context.
    ///
    /// The adapter is rebuilt per call and the fetch stack is not: `build_adapter` is cheap
    /// and stateless, while the fetch stack carries the rate limiter, the connection pool and
    /// the accumulated throttle penalty, all of which must be shared across a provider's
    /// tasks to mean anything.
    fn provider_context(
        &self,
        provider: &Provider,
    ) -> anyhow::Result<(Box<dyn SourceAdapter>, Ctx)> {
        let adapter = build_adapter(provider.adapter, &provider.slug, &provider.config)?;
        let ctx = Ctx {
            base_url: provider.base_url.clone(),
            provider_slug: provider.slug.clone(),
            fetcher: self.fetcher_for(provider)?,
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
            // Skip duplicates a paginator may repeat across pages, then register what is
            // left in one batch — the same batched path the broker fan-out uses, so a
            // sitemap-shard page of 20k entries costs one existence query, not 20k.
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
                // Breadth-first: register every series on the page now so the full list is
                // available before any chapters are fetched. Both halves are batched — a
                // sitemap-shard page can carry 20k entries, and a per-entry round-trip there
                // would outrun the consumer's ack deadline.
                let entries: Vec<(&str, &str)> = catalog
                    .items
                    .iter()
                    .map(|i| (i.path.as_str(), i.title.as_str()))
                    .collect();
                match tankovault_db::repo::catalog::register_source_stubs(
                    &self.pool,
                    provider.id,
                    &entries,
                )
                .await
                {
                    // Logged per page so a catalogue that walks but yields nothing new is
                    // visible immediately: a clamped or looping paginator shows up here as
                    // page after page of `registered = 0`, which is otherwise
                    // indistinguishable from a healthy re-scan until the run totals land.
                    Ok(registered) => tracing::info!(
                        provider = %provider.slug,
                        page,
                        items = entries.len(),
                        registered,
                        "catalog page walked"
                    ),
                    // A registration failure must not lose the series: the enrichment tasks
                    // below are enqueued regardless and will create the source themselves.
                    Err(e) => tracing::warn!(
                        provider = %provider.slug,
                        page,
                        items = entries.len(),
                        error = %e,
                        next = "series are still enqueued and will create their own source rows",
                        "catalog page registration failed"
                    ),
                }
                let targets: Vec<serde_json::Value> = catalog
                    .items
                    .iter()
                    .map(|i| serde_json::json!({ "path": i.path }))
                    .collect();
                self.enqueue_children(task, "series", TaskKind::Series, targets)
                    .await?;
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
                        tracing::warn!(
                            provider = %provider.slug,
                            target = %update.path,
                            error = %e,
                            next = "series skipped; the rest of the feed continues and the next \
                                    fast scan retries it",
                            "latest series failed"
                        );
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

    /// Fan out many children of the same `kind` at once — the batched counterpart to
    /// [`enqueue_child`](Self::enqueue_child), used for catalogue pages that carry thousands
    /// of entries (a sitemap-shard page can hold 20k).
    ///
    /// Preserves both fan-out invariants exactly: creation stays idempotent on
    /// `(run_id, kind, target)` (only rows actually inserted come back, so a redelivered
    /// parent republishes nothing), and `total_tasks` is bumped **before** any child is
    /// published — and before the parent completes — so `done + failed` cannot reach `total`
    /// mid-fan-out and finalise the run early.
    ///
    /// Targets are processed in chunks so one page never becomes a single multi-megabyte
    /// statement, and so progress is visible while a large page is still fanning out.
    async fn enqueue_children(
        &self,
        parent: &ScanTaskMessage,
        kind_str: &str,
        kind: TaskKind,
        targets: Vec<serde_json::Value>,
    ) -> anyhow::Result<()> {
        let Some(bus) = &self.bus else {
            anyhow::bail!("cannot fan out scan tasks without a broker");
        };
        for chunk in targets.chunks(FANOUT_CHUNK) {
            let created = tankovault_db::repo::scans::create_tasks(
                &self.pool,
                parent.run_id,
                kind_str,
                chunk,
            )
            .await?;
            if created.is_empty() {
                continue;
            }
            let delta = i32::try_from(created.len()).unwrap_or(i32::MAX);
            tankovault_db::repo::scans::add_total_tasks(&self.pool, parent.run_id, delta).await?;
            for (task_id, target) in created {
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
            }
        }
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

#[cfg(test)]
mod tests {
    use super::*;
    use tankovault_domain::{AdapterKind, Politeness, ProviderState};

    fn provider(rps: f64, ua: &str) -> Provider {
        Provider {
            id: ProviderId::from_uuid(uuid::Uuid::nil()),
            slug: "demo".to_owned(),
            name: "Demo".to_owned(),
            base_url: "https://demo.test".to_owned(),
            adapter: AdapterKind::Madara,
            config: serde_json::json!({}),
            state: ProviderState::Active,
            politeness: Politeness {
                rps,
                concurrency: 2,
                crawl_delay_ms: 0,
                user_agent: ua.to_owned(),
                emulation: None,
            },
            last_full_scan_at: None,
            created_at: OffsetDateTime::UNIX_EPOCH,
            updated_at: OffsetDateTime::UNIX_EPOCH,
        }
    }

    /// The cache key must change when — and only when — a setting the fetch stack is built
    /// from changes. Too eager and every task rebuilds again (the bug this replaced); too
    /// lazy and an operator lowering `rps` mid-run is ignored until the process restarts.
    #[test]
    fn the_fingerprint_tracks_exactly_the_settings_the_stack_is_built_from() {
        let base = provider(1.0, "tankovault/1.0");
        assert_eq!(
            politeness_fingerprint(&base),
            politeness_fingerprint(&provider(1.0, "tankovault/1.0")),
            "identical politeness must reuse the stack, or the limiter is per-task again"
        );

        assert_ne!(
            politeness_fingerprint(&base),
            politeness_fingerprint(&provider(0.5, "tankovault/1.0")),
            "lowering rps must take effect without a restart"
        );
        assert_ne!(
            politeness_fingerprint(&base),
            politeness_fingerprint(&provider(1.0, "other/1.0"))
        );

        // Fields the fetch stack does not read must NOT invalidate a warm connection pool,
        // a rate limiter and an accumulated throttle penalty.
        let mut renamed = provider(1.0, "tankovault/1.0");
        renamed.name = "Renamed".to_owned();
        renamed.config = serde_json::json!({ "unrelated": true });
        renamed.state = ProviderState::Degraded;
        assert_eq!(
            politeness_fingerprint(&base),
            politeness_fingerprint(&renamed),
            "an unrelated column change must not throw away the fetch stack"
        );
    }

    /// `rps` is an `f64`; hashing it at all requires going through the bit pattern.
    #[test]
    fn fractional_rates_are_distinguished() {
        assert_ne!(
            politeness_fingerprint(&provider(0.5, "ua")),
            politeness_fingerprint(&provider(0.25, "ua"))
        );
    }
}
