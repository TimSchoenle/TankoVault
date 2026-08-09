//! The `JetStream` task consumer: dispatch, catalogue fan-out and progress reporting.

use tankovault_contracts::{ScanTaskMessage, TaskKind};
use tankovault_domain::{Provider, ScanStage};
use time::OffsetDateTime;

use super::{Engine, FANOUT_CHUNK, StageReporter, catalog_truncated};

impl Engine {
    /// Dispatch a single task received from `JetStream`: `CatalogPage` registers every
    /// series on the page, enqueues a `Series` task per series, and chains the next page;
    /// `Series` fetches and upserts one series; `LatestFeed` enqueues a `Series` task per
    /// entry the feed named.
    ///
    /// Every arm reports its stage as it goes. That is what makes a task that legitimately runs
    /// for minutes legible from outside: without it, `claimed` is the only thing the console can
    /// say about a task, and it says the same thing about a wedged one.
    pub(crate) async fn dispatch_task(
        &self,
        provider: &Provider,
        task: &ScanTaskMessage,
        stage: &StageReporter,
    ) -> anyhow::Result<()> {
        let (adapter, ctx) = self.provider_context(provider)?;
        match task.kind {
            TaskKind::Series => {
                let path = task
                    .target
                    .get("path")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| anyhow::anyhow!("series task missing path"))?;
                self.process_series(provider, adapter.as_ref(), &ctx, path, stage)
                    .await?;
            }
            TaskKind::CatalogPage => {
                let page = task
                    .target
                    .get("page")
                    .and_then(serde_json::Value::as_u64)
                    .and_then(|p| u32::try_from(p).ok())
                    .unwrap_or(1);
                self.walk_catalog_page(provider, task, adapter.as_ref(), &ctx, page, stage)
                    .await?;
            }
            TaskKind::LatestFeed => {
                stage.enter(ScanStage::FeedFetch, None).await;
                let updates = adapter.list_latest(&ctx).await?;
                // Fanned out, not walked inline.
                //
                // Walking the feed inside this one task is why a fast scan sat at `0/1` for as
                // long as it ran: the run's single counter could not move until every series the
                // feed named had been fetched, and at a polite crawl rate that is tens of
                // minutes. A child per series reports the same work as `17/94`, gives each series
                // its own retry budget and its own failure row instead of one warning in the log,
                // and keeps every task short enough that a redelivery costs one series rather
                // than the whole feed. It costs one extra task per feed entry, which is what the
                // full scan has always done per catalogue entry.
                let targets: Vec<serde_json::Value> = updates
                    .iter()
                    .map(|u| serde_json::json!({ "path": u.path }))
                    .collect();
                tracing::info!(
                    provider = %provider.slug,
                    series = targets.len(),
                    "latest feed read"
                );
                stage.enter(ScanStage::FeedFanout, None).await;
                self.enqueue_children(task, "series", TaskKind::Series, targets, stage)
                    .await?;
            }
        }
        Ok(())
    }

    /// Walk one catalogue page: register everything on it, fan out a task per series, and chain
    /// the next page.
    ///
    /// Three stages, because they fail and stall for different reasons — a page that will not
    /// fetch is a provider problem, a registration that crawls is the trigram matcher, and a
    /// fan-out that crawls is the broker.
    async fn walk_catalog_page(
        &self,
        provider: &Provider,
        task: &ScanTaskMessage,
        adapter: &dyn tankovault_adapters::SourceAdapter,
        ctx: &tankovault_adapters::Ctx,
        page: u32,
        stage: &StageReporter,
    ) -> anyhow::Result<()> {
        let label = format!("page {page}");
        stage.enter(ScanStage::CatalogFetch, Some(&label)).await;
        let catalog = adapter.list_catalog(ctx, page).await?;
        // Breadth-first: register every series before fetching chapters. Batched — a per-entry
        // round trip on a 20k-entry sitemap page would outrun the consumer's ack deadline.
        let entries: Vec<(&str, &str)> = catalog
            .items
            .iter()
            .map(|i| (i.path.as_str(), i.title.as_str()))
            .collect();
        stage.enter(ScanStage::CatalogRegister, Some(&label)).await;
        match tankovault_db::repo::catalog::register_source_stubs(
            &self.pool,
            provider.id,
            &entries,
            &self.matching,
        )
        .await
        {
            // Logged per page so a looping paginator is visible immediately as page after page
            // of `registered = 0`.
            Ok(registered) => tracing::info!(
                provider = %provider.slug,
                page,
                items = entries.len(),
                registered,
                "catalog page walked"
            ),
            // A registration failure must not lose the series: the enrichment tasks below are
            // enqueued regardless and will create the source themselves.
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
        stage.enter(ScanStage::CatalogFanout, Some(&label)).await;
        self.enqueue_children(task, "series", TaskKind::Series, targets, stage)
            .await?;

        // Chain the next page while the catalogue has more, bounded by the same safety cap the
        // inline walk uses so an unbounded paginator cannot loop.
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
                    "catalog fan-out stopped at the page safety cap while the catalogue still \
                     had more pages; increase worker.max_catalog_pages if this is a legitimately \
                     large site"
                );
                catalog_truncated(&provider.slug);
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
        stage: &StageReporter,
    ) -> anyhow::Result<()> {
        let Some(bus) = &self.bus else {
            anyhow::bail!("cannot fan out scan tasks without a broker");
        };
        let total = targets.len();
        let mut published = 0usize;
        for chunk in targets.chunks(FANOUT_CHUNK) {
            let created = tankovault_db::repo::scans::create_tasks(
                &self.pool,
                parent.run_id,
                kind_str,
                chunk,
            )
            .await?;
            published += chunk.len();
            // Reported per chunk, not per task: a 20k-entry page otherwise finishes its fan-out
            // with the console still showing the page fetch that preceded it. The reporter
            // throttles the writes.
            stage.progress(published, total, None).await;
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
