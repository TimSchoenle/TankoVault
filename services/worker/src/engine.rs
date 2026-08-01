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
use tankovault_config::MatchingConfig;
use tankovault_contracts::{ChapterDiscovered, ScanTaskMessage, TaskKind};
use tankovault_db::PgPool;
use tankovault_db::repo::catalog::{ChapterUpsert, ScannedSeries, SeriesUpsert};
use tankovault_domain::{Provider, ProviderId, normalize_title};
use tankovault_fetch::{Fetcher, ProviderFetchConfig, SessionStore, build_provider_fetcher};
use tankovault_solver::ChallengeSolver;
use time::OffsetDateTime;

/// Children enqueued per statement when fanning out a catalogue page.
///
/// A catalogue "page" can be up to 20k entries (a sitemap-driven adapter like kunmanga).
/// Chunking bounds any single INSERT and lets `total_tasks` advance incrementally.
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
    /// The confidence policy for canonicalising a scanned series onto an existing one.
    ///
    /// Held here, not defaulted in the repository, so this path and external sync answer
    /// "is this the same series?" the same way.
    pub(crate) matching: MatchingConfig,
    /// One fetch stack per provider, keyed by the politeness settings it was built from.
    ///
    /// Load-bearing for correctness, not just speed: the rate limiter and adaptive 429
    /// penalty live on the fetcher, so building one per task turns `rps`/`concurrency`
    /// into a per-task budget instead of per-provider, and N concurrent tasks offer N ×
    /// rps to the provider.
    fetchers: Arc<Mutex<HashMap<ProviderId, CachedFetcher>>>,
}

/// Hash the provider settings a fetch stack is built from.
///
/// Only the inputs to `build_provider_fetcher`, so an unrelated column change doesn't
/// discard a warm connection pool, while lowering `rps` takes effect on the next task.
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

/// The lookup key for a chapter number, pairing a newly-inserted number back to the parsed
/// chapter it came from.
///
/// Keyed on the bit pattern, not a tolerance comparison, matching the old
/// `(a - b).abs() < f64::EPSILON` test for every value a chapter number can take. The one
/// disagreement is `-0.0`, normalised here since chapter 0 (prologues) is real. `NaN`
/// cannot arrive: `parse_number` rejects non-finite values.
fn chapter_key(number: f64) -> u64 {
    if number == 0.0 { 0.0_f64 } else { number }.to_bits()
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
    /// A constructor, not a struct literal, so [`Engine::fetchers`] stays an
    /// implementation detail callers can't construct pre-populated.
    pub(crate) fn new(
        pool: PgPool,
        bus: Option<Bus>,
        solver: Arc<dyn ChallengeSolver>,
        session_store: Arc<dyn SessionStore>,
        worker_id: String,
        max_catalog_pages: u32,
        matching: MatchingConfig,
    ) -> Self {
        Self {
            pool,
            bus,
            solver,
            session_store,
            worker_id,
            max_catalog_pages,
            matching,
            fetchers: Arc::default(),
        }
    }

    /// Build the fetch stack for `provider`, reusing the cached one when its settings are
    /// unchanged.
    fn fetcher_for(&self, provider: &Provider) -> anyhow::Result<Arc<dyn Fetcher>> {
        let fingerprint = politeness_fingerprint(provider);

        // No await point held here, so a `std` mutex is right; `tokio::sync::RwLock`
        // would make `provider_context` async for no benefit.
        {
            let cache = self.fetchers.lock().expect("fetcher cache mutex poisoned");
            if let Some(entry) = cache.get(&provider.id)
                && entry.fingerprint == fingerprint
            {
                return Ok(Arc::clone(&entry.fetcher));
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
        // Another task may have built one concurrently. Either is correct, but the stored
        // entry must be shared, so only insert if it's still absent or stale.
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
    /// The adapter is rebuilt per call (cheap, stateless); the fetch stack is not — it
    /// carries the rate limiter and throttle penalty, which must be shared across a
    /// provider's tasks to mean anything.
    pub(crate) fn provider_context(
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

        // `meta`/`chapters` move into `scanned`, not copy: the fan-out below reads them
        // back out of `scanned`, so a 2,000-chapter series doesn't allocate a second copy
        // of every title and path.
        let normalized_title = normalize_title(&meta.title);
        let scanned = ScannedSeries {
            provider_id: provider.id,
            source_path: path.to_owned(),
            // The one surviving clone: the title is both the provider's label for this source
            // and the canonical series title, and the two are independent thereafter.
            provider_title: Some(meta.title.clone()),
            meta: SeriesUpsert {
                canonical_title: meta.title,
                normalized_title,
                description: meta.description,
                cover_url: meta.cover_url,
                content_type: meta.content_type,
                status: meta.status,
                release_year: meta.release_year,
            },
            alt_titles: meta
                .alt_titles
                .into_iter()
                .map(|t| {
                    let normalized = normalize_title(&t);
                    (t, normalized)
                })
                .collect(),
            tags: meta.tags,
            authors: meta.authors,
            chapters: chapters
                .into_iter()
                .map(|c| ChapterUpsert {
                    number: c.number,
                    volume: None,
                    title: c.title,
                    path: c.path,
                    published_at: c.published_at,
                })
                .collect(),
            content_hash: hash,
        };

        let outcome =
            tankovault_db::repo::catalog::ingest_series(&self.pool, &scanned, &self.matching)
                .await?;

        if let Some(bus) = &self.bus {
            // One indexing pass, then a lookup per new chapter — an O(n) scan per new
            // number would cost a 2,000-chapter series with 50 new chapters 100,000
            // comparisons.
            let by_number: HashMap<u64, &ChapterUpsert> = scanned
                .chapters
                .iter()
                .map(|c| (chapter_key(c.number), c))
                .collect();

            for number in &outcome.new_chapters {
                if let Some(ch) = by_number.get(&chapter_key(*number)) {
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

    /// One-shot fast scan: read the latest feed and re-ingest each updated series.
    pub(crate) async fn run_fast_scan_inline(
        &self,
        provider: &Provider,
    ) -> anyhow::Result<ScanSummary> {
        let (adapter, ctx) = self.provider_context(provider)?;
        let mut summary = ScanSummary::default();

        let updates = adapter.list_latest(&ctx).await?;
        for update in &updates {
            summary.series_seen += 1;
            // Ingest is idempotent and reports only genuinely new chapters (via
            // `xmax = 0`), so re-ingesting an unchanged series emits no false-new events.
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

    /// Dispatch a single task received from `JetStream`: `CatalogPage` registers every
    /// series on the page, enqueues a `Series` task per series, and chains the next page;
    /// `Series` fetches and upserts one series; `LatestFeed` ingests updates inline.
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
                // Breadth-first: register every series before fetching chapters. Batched —
                // a per-entry round trip on a 20k-entry sitemap page would outrun the
                // consumer's ack deadline.
                let entries: Vec<(&str, &str)> = catalog
                    .items
                    .iter()
                    .map(|i| (i.path.as_str(), i.title.as_str()))
                    .collect();
                match tankovault_db::repo::catalog::register_source_stubs(
                    &self.pool,
                    provider.id,
                    &entries,
                    &self.matching,
                )
                .await
                {
                    // Logged per page so a looping paginator is visible immediately as
                    // page after page of `registered = 0`.
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
    /// Idempotent on `(run_id, kind, target)`: a redelivered parent re-enqueues nothing.
    /// The total is incremented before the parent completes, so `done + failed` can never
    /// reach `total` mid-fan-out — the run cannot finalise early.
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

    /// Batched counterpart to [`enqueue_child`](Self::enqueue_child), for catalogue pages
    /// carrying thousands of entries.
    ///
    /// Preserves both fan-out invariants: idempotent creation on `(run_id, kind, target)`,
    /// and `total_tasks` bumped before any child is published and before the parent
    /// completes, so the run cannot finalise mid-fan-out. Chunked so one page never
    /// becomes a single multi-megabyte statement.
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

    /// Publish a compact [`tankovault_contracts::ProgressEvent`] for `run_id`, read from
    /// `scan_runs`, so the control-plane aggregator can finalise the run and the console
    /// SSE gets live progress over NATS. Best-effort: a broker or DB hiccup is logged,
    /// never fatal.
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

    fn meta(title: &str, description: Option<&str>) -> SeriesMeta {
        SeriesMeta {
            title: title.to_owned(),
            alt_titles: Vec::new(),
            description: description.map(str::to_owned),
            cover_url: None,
            tags: Vec::new(),
            authors: Vec::new(),
            status: tankovault_domain::SeriesStatus::Ongoing,
            content_type: tankovault_domain::ContentType::Manga,
            release_year: Some(2020),
        }
    }

    fn chapter(number: f64, title: Option<&str>, path: &str) -> ChapterMeta {
        ChapterMeta {
            number,
            title: title.map(str::to_owned),
            path: path.to_owned(),
            published_at: None,
        }
    }

    /// Determinism is the entire contract: a hash that varies for identical input makes
    /// every scan look changed (wasteful); one that's stable across a real change stops
    /// updates for that series silently — the failure nobody notices.
    #[test]
    fn the_content_hash_is_deterministic_for_identical_input() {
        let chapters = vec![
            chapter(1.0, Some("Awakening"), "/manga/x/1/"),
            chapter(2.0, None, "/manga/x/2/"),
        ];
        assert_eq!(
            content_hash(&meta("Solo Leveling", Some("blurb")), &chapters),
            content_hash(&meta("Solo Leveling", Some("blurb")), &chapters),
        );
    }

    /// Every field the hash is documented to cover must actually change it.
    #[test]
    fn the_content_hash_changes_when_a_covered_field_changes() {
        let base_meta = meta("Solo Leveling", Some("blurb"));
        let base = vec![chapter(1.0, None, "/manga/x/1/")];
        let baseline = content_hash(&base_meta, &base);

        assert_ne!(
            baseline,
            content_hash(&meta("Solo Levelling", Some("blurb")), &base),
            "a retitled series must be seen as changed"
        );
        assert_ne!(
            baseline,
            content_hash(&meta("Solo Leveling", Some("rewritten")), &base),
            "a rewritten description must be seen as changed"
        );
        assert_ne!(
            baseline,
            content_hash(&base_meta, &[chapter(1.5, None, "/manga/x/1/")]),
            "a renumbered chapter must be seen as changed"
        );
        assert_ne!(
            baseline,
            content_hash(&base_meta, &[chapter(1.0, None, "/manga/x/1-v2/")]),
            "a relinked chapter must be seen as changed"
        );
        assert_ne!(
            baseline,
            content_hash(
                &base_meta,
                &[
                    chapter(1.0, None, "/manga/x/1/"),
                    chapter(2.0, None, "/manga/x/2/"),
                ]
            ),
            "a new chapter must be seen as changed — this is the case the whole scan exists for"
        );
    }

    /// Two things the hash deliberately does *not* cover.
    ///
    /// 1. Chapter titles aren't hashed — a chapter retitled in place reports "no change",
    ///    intentional since scanlation sites edit labels constantly.
    /// 2. Chapter order is significant — a reordered listing reports a change, costing
    ///    work but never wrong; an order-insensitive hash would be a behaviour change.
    #[test]
    fn the_content_hash_ignores_chapter_titles_and_respects_chapter_order() {
        let series = meta("Solo Leveling", None);
        let untitled = vec![chapter(1.0, None, "/manga/x/1/")];
        let titled = vec![chapter(1.0, Some("Awakening"), "/manga/x/1/")];
        assert_eq!(
            content_hash(&series, &untitled),
            content_hash(&series, &titled),
            "chapter titles are outside the hash; if that changes, change this test on purpose"
        );

        let ascending = vec![
            chapter(1.0, None, "/manga/x/1/"),
            chapter(2.0, None, "/manga/x/2/"),
        ];
        let descending = vec![
            chapter(2.0, None, "/manga/x/2/"),
            chapter(1.0, None, "/manga/x/1/"),
        ];
        assert_ne!(
            content_hash(&series, &ascending),
            content_hash(&series, &descending),
            "the hash is order-sensitive today; making it order-insensitive is a behaviour \
             change, not a cleanup"
        );
    }

    /// A chapter path can't be made to look like a different chapter list by embedding the
    /// framing bytes (`number | path \n`) the hash uses to separate entries — a classic
    /// collision providers could otherwise force.
    #[test]
    fn a_chapter_path_carrying_the_separator_bytes_does_not_forge_another_chapter() {
        let series = meta("X", None);
        let smuggled = vec![chapter(1.0, None, "/a/\n\u{0}|/b/")];
        let genuine = vec![chapter(1.0, None, "/a/"), chapter(1.0, None, "/b/")];
        assert_ne!(
            content_hash(&series, &smuggled),
            content_hash(&series, &genuine),
            "a provider-supplied path must not be able to impersonate a second chapter"
        );
    }

    /// The pairing key must agree with the tolerance comparison it replaced, on every
    /// number a chapter can carry — disagreement loses notifications silently.
    #[test]
    fn the_pairing_key_agrees_with_the_tolerance_it_replaced() {
        let numbers = [
            0.0, 1.0, 1.5, 2.0, 3.5, 152.0, 152.1, 152.5, 152.6, 153.0, 9999.0, 0.001, 1e9,
        ];
        for &a in &numbers {
            for &b in &numbers {
                let by_key = chapter_key(a) == chapter_key(b);
                let by_tolerance = (a - b).abs() < f64::EPSILON;
                assert_eq!(
                    by_key, by_tolerance,
                    "`{a}` vs `{b}`: the hash key and the old comparison must decide the same                      way, or a chapter is announced twice or not at all"
                );
            }
        }
    }

    /// Chapter 0 is real — a prologue — and it is the one value where a bit pattern and a
    /// tolerance would disagree, because `-0.0` and `0.0` are equal numbers with different
    /// bits. Normalising it is what keeps a notification from depending on a sign bit.
    #[test]
    fn negative_zero_is_the_same_chapter_as_zero() {
        assert_eq!(chapter_key(-0.0), chapter_key(0.0));
    }
}
