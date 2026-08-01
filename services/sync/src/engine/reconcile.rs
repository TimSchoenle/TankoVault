//! Full three-way reconciliation of a linked account. Owns the *I/O* half — fetching, resolving,
//! persisting, applying — while every merge rule itself lives in [`super::plan`].

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use secrecy::SecretString;
use serde::Serialize;
use time::OffsetDateTime;

use tankovault_db::PgPool;
use tankovault_db::repo::{sync, tracking};
use tankovault_domain::{SeriesId, UserId, WatchStatus};

use super::accounts::AccountService;
use super::metadata::MetadataWriter;
use super::plan::{Ancestor, LocalSide, MergePlan, SeriesPlan, plan_merge, plan_series};
use super::registry::ProviderRegistry;
use super::resolve::SeriesResolver;
use super::tokens::TokenVault;
use crate::mapping::{ConflictPolicy, MergeAction};
use crate::provider::{ExternalProvider, RemoteEntry};

/// Outcome of a pull (provider → local).
#[derive(Debug, Default, Serialize)]
pub(crate) struct PullReport {
    /// Entries returned by the provider.
    pub(crate) fetched: usize,
    /// Entries resolved to a canonical local series.
    pub(crate) matched: usize,
    /// Local progress rows written.
    pub(crate) updated: usize,
    /// Entries with no confident local match (skipped).
    pub(crate) unmatched: usize,
}

/// Outcome of a push (local → provider).
#[derive(Debug, Default, Serialize)]
pub(crate) struct PushReport {
    /// Local watchlist entries examined.
    pub(crate) considered: usize,
    /// Remote entries created or updated.
    pub(crate) pushed: usize,
    /// Watchlist entries with no resolvable remote media (skipped).
    pub(crate) unmapped: usize,
}

/// Aggregate counters accumulated over one full account reconciliation. Both the manual
/// `PullReport`/`PushReport` and the scheduled loop's logging are derived from these.
#[derive(Debug, Default)]
pub(crate) struct ReconcileCounts {
    pub(crate) fetched: usize,
    pub(crate) matched: usize,
    pub(crate) unmatched: usize,
    /// Local watchlist entries examined in the local-driven pass.
    pub(crate) considered: usize,
    /// Local writes applied (progress/status pulled from the remote).
    pub(crate) pulled: usize,
    /// Remote writes applied (progress/status pushed to the provider).
    pub(crate) pushed: usize,
    /// Watchlist entries with no resolvable remote media (skipped).
    pub(crate) unmapped: usize,
    /// Genuine conflicts queued for the user under `AskMe`.
    pub(crate) conflicts: usize,
    /// Series skipped because they are excluded from sync.
    pub(crate) skipped: usize,
    /// Series whose catalogue metadata was refreshed from the entries just fetched.
    pub(crate) enriched: usize,
}

/// How long a series' catalogue metadata is left alone after an enrichment attempt before a
/// list reconciliation refreshes it again. The tokenless sweep, not this pass, keeps metadata
/// current in between.
const METADATA_REFRESH_INTERVAL: time::Duration = time::Duration::WEEK;

/// One user's local sync-relevant state for one provider, read once per reconciliation run
/// instead of per series. Sound only because a run reconciles each series **at most once**
/// (`handled_series`/`handled_ids` in [`Reconciler::reconcile_account`] guarantee it), so no
/// series is read here after that same run has written to it.
struct LocalState {
    /// Series excluded from syncing with this provider.
    excluded: HashSet<SeriesId>,
    /// Whole-chapter frontier and when it last changed.
    progress: HashMap<SeriesId, (f64, OffsetDateTime)>,
    /// Watchlist status, absent when the series is not on the watchlist.
    status: HashMap<SeriesId, WatchStatus>,
}

impl LocalState {
    async fn load(pool: &PgPool, user_id: UserId, slug: &str) -> anyhow::Result<Self> {
        Ok(Self {
            excluded: tracking::sync_excluded_series(pool, user_id, slug).await?,
            progress: tracking::progress_states_for_user(pool, user_id).await?,
            status: tracking::watchlist_statuses_for_user(pool, user_id).await?,
        })
    }

    /// This run's view of one series, in the shape the pure planner takes.
    fn side(&self, series_id: SeriesId) -> LocalSide {
        let state = self.progress.get(&series_id).copied();
        LocalSide {
            progress: state.map_or(0.0, |(p, _)| p),
            updated_at: state.map_or(OffsetDateTime::UNIX_EPOCH, |(_, u)| u),
            status: self.status.get(&series_id).copied(),
            excluded: self.excluded.contains(&series_id),
        }
    }
}

/// What every series in one reconciliation run shares, bundled so per-series steps take one
/// argument instead of five.
struct RunContext<'a> {
    provider: &'a dyn ExternalProvider,
    slug: &'a str,
    access: &'a SecretString,
    user_id: UserId,
    policy: ConflictPolicy,
}

/// Collapse a fetched remote list to at most one entry per `external_id`, keeping the most
/// recently updated occurrence. A provider list can legitimately carry the same remote work
/// twice with divergent progress; reconciling both would flip-flop the series every run.
fn dedupe_latest_by_external_id(entries: Vec<RemoteEntry>) -> Vec<RemoteEntry> {
    let mut by_id: HashMap<String, RemoteEntry> = HashMap::with_capacity(entries.len());
    for entry in entries {
        match by_id.get(entry.external_id()) {
            Some(existing) if existing.updated_at >= entry.updated_at => {}
            _ => {
                by_id.insert(entry.external_id().to_owned(), entry);
            }
        }
    }
    by_id.into_values().collect()
}

/// Runs the account-wide merge between a provider and the local catalogue.
pub(crate) struct Reconciler {
    pool: PgPool,
    registry: Arc<ProviderRegistry>,
    tokens: Arc<TokenVault>,
    accounts: Arc<AccountService>,
    resolver: Arc<SeriesResolver>,
    metadata: Arc<MetadataWriter>,
}

impl Reconciler {
    pub(crate) const fn new(
        pool: PgPool,
        registry: Arc<ProviderRegistry>,
        tokens: Arc<TokenVault>,
        accounts: Arc<AccountService>,
        resolver: Arc<SeriesResolver>,
        metadata: Arc<MetadataWriter>,
    ) -> Self {
        Self {
            pool,
            registry,
            tokens,
            accounts,
            resolver,
            metadata,
        }
    }

    /// Manual "pull": runs the full three-way reconciliation and reports it in the historical
    /// `PullReport` shape (`pull` and `push` now do the same reconcile).
    pub(crate) async fn pull(
        &self,
        slug: &str,
        user_id: UserId,
        policy: Option<ConflictPolicy>,
    ) -> anyhow::Result<PullReport> {
        let c = self
            .reconcile_account_guarded(slug, user_id, policy)
            .await?;
        Ok(PullReport {
            fetched: c.fetched,
            matched: c.matched,
            updated: c.pulled,
            unmatched: c.unmatched,
        })
    }

    /// Manual "push": identical full reconciliation, reported in the historical `PushReport` shape.
    pub(crate) async fn push(
        &self,
        slug: &str,
        user_id: UserId,
        policy: Option<ConflictPolicy>,
    ) -> anyhow::Result<PushReport> {
        let c = self
            .reconcile_account_guarded(slug, user_id, policy)
            .await?;
        Ok(PushReport {
            considered: c.considered,
            pushed: c.pushed,
            unmapped: c.unmapped,
        })
    }

    /// Scheduled reconciliation of every account with automatic sync enabled. Best-effort: a
    /// failure on one account is logged and does not abort the tick.
    pub(crate) async fn reconcile_all_accounts(&self) {
        let accounts = match sync::list_auto_sync_accounts(&self.pool).await {
            Ok(a) => a,
            Err(e) => {
                tracing::warn!(error = %e, "scheduled reconciliation could not list accounts");
                return;
            }
        };
        for (user_id, slug) in accounts {
            if self.registry.try_get(&slug).is_none() {
                continue;
            }
            if let Err(e) = self.reconcile_account_guarded(&slug, user_id, None).await {
                tracing::warn!(error = %e, provider = %slug, %user_id,
                    "scheduled reconciliation failed for account");
            }
        }
    }

    async fn reconcile_account_guarded(
        &self,
        slug: &str,
        user_id: UserId,
        policy: Option<ConflictPolicy>,
    ) -> anyhow::Result<ReconcileCounts> {
        match self.reconcile_account(slug, user_id, policy).await {
            Ok(c) => Ok(c),
            Err(e) => {
                let _ = sync::record_sync_error(&self.pool, user_id, slug, &e.to_string()).await;
                Err(e)
            }
        }
    }

    /// Full three-way reconciliation of a linked account: every remote entry is matched +
    /// reconciled, then every mapped local watchlist entry not seen on the remote is created
    /// there. Excluded series are skipped; `AskMe` conflicts are queued.
    ///
    /// Runs in phases so writes are set-based: mappings must be persisted before the merge
    /// phase, since `record_snapshot` writes into the `sync_mappings` row.
    async fn reconcile_account(
        &self,
        slug: &str,
        user_id: UserId,
        override_policy: Option<ConflictPolicy>,
    ) -> anyhow::Result<ReconcileCounts> {
        let provider = self.registry.get(slug)?;
        let policy = self
            .accounts
            .effective_policy(slug, user_id, override_policy)
            .await;
        let access = self.tokens.access(slug, provider, user_id).await?;
        let viewer = provider.viewer(&access).await?;
        let entries = dedupe_latest_by_external_id(provider.fetch_list(&access, &viewer).await?);

        let run = RunContext {
            provider,
            slug,
            access: &access,
            user_id,
            policy,
        };

        let mut counts = ReconcileCounts {
            fetched: entries.len(),
            ..Default::default()
        };
        let local = LocalState::load(&self.pool, user_id, slug).await?;

        // Phase 1: resolve every entry to a canonical series (or to nothing).
        let mut resolved: Vec<(&RemoteEntry, Option<SeriesId>)> = Vec::with_capacity(entries.len());
        for entry in &entries {
            let matched = self.resolver.series_for_entry(slug, entry).await?;
            resolved.push((entry, matched));
        }

        self.persist_fetched(&run, &resolved).await?;
        self.enrich_matched(&run, &resolved, &mut counts).await;
        let handled_ids = self
            .reconcile_fetched(&run, &resolved, &local, &mut counts)
            .await?;
        self.reconcile_watchlist(&run, &handled_ids, &local, &mut counts)
            .await?;

        sync::mark_synced(
            &self.pool,
            user_id,
            slug,
            Some(&viewer.name),
            OffsetDateTime::now_utc(),
        )
        .await?;
        Ok(counts)
    }

    /// Persist every fetched snapshot and every resolved mapping in two set-based statements.
    async fn persist_fetched(
        &self,
        run: &RunContext<'_>,
        resolved: &[(&RemoteEntry, Option<SeriesId>)],
    ) -> anyhow::Result<()> {
        let snapshots: Vec<sync::FetchedRemoteEntry> = resolved
            .iter()
            .map(|(entry, matched)| sync::FetchedRemoteEntry {
                external_id: entry.external_id().to_owned(),
                title: entry.metadata.titles.first().cloned().unwrap_or_default(),
                status: entry.status.as_str().to_owned(),
                progress: entry.progress,
                content_type: entry.metadata.content_type.as_str().to_owned(),
                start_year: entry.metadata.start_year,
                updated_at: entry.updated_at,
                series_id: *matched,
            })
            .collect();
        sync::upsert_remote_entries(&self.pool, run.user_id, run.slug, &snapshots).await?;

        let mappings: Vec<(SeriesId, String)> = resolved
            .iter()
            .filter_map(|(entry, matched)| matched.map(|id| (id, entry.external_id().to_owned())))
            .collect();
        sync::upsert_mappings(&self.pool, run.slug, &mappings).await?;
        Ok(())
    }

    /// Fold each matched entry's upstream metadata into its local series, reusing the metadata
    /// already returned by the list fetch — no extra provider call. Only series due per
    /// [`METADATA_REFRESH_INTERVAL`] are written, so a settled catalogue costs one query.
    /// Best-effort: a failure is logged and the progress/status merge proceeds regardless.
    async fn enrich_matched(
        &self,
        run: &RunContext<'_>,
        resolved: &[(&RemoteEntry, Option<SeriesId>)],
        counts: &mut ReconcileCounts,
    ) {
        let by_series: HashMap<SeriesId, &RemoteEntry> = resolved
            .iter()
            .filter_map(|(entry, matched)| matched.map(|id| (id, *entry)))
            .collect();
        if by_series.is_empty() {
            return;
        }
        let ids: Vec<SeriesId> = by_series.keys().copied().collect();
        let stale_before = OffsetDateTime::now_utc() - METADATA_REFRESH_INTERVAL;
        let due = match self.metadata.needing_metadata(&ids, stale_before).await {
            Ok(rows) => rows,
            Err(e) => {
                tracing::warn!(error = %e, provider = %run.slug,
                    "could not select series for list-sync enrichment");
                return;
            }
        };
        for row in &due {
            let Some(entry) = by_series.get(&row.id) else {
                continue;
            };
            match self.metadata.apply(row, run.slug, &entry.metadata).await {
                Ok(()) => counts.enriched += 1,
                Err(e) => tracing::warn!(error = %e, series_id = %row.id,
                    "could not apply list-sync metadata"),
            }
        }
    }

    /// Phase 3, remote-driven: reconcile every matched entry against its local state. Returns
    /// the external ids handled, which the local-driven pass uses to avoid doing them twice.
    async fn reconcile_fetched(
        &self,
        run: &RunContext<'_>,
        resolved: &[(&RemoteEntry, Option<SeriesId>)],
        local: &LocalState,
        counts: &mut ReconcileCounts,
    ) -> anyhow::Result<HashSet<String>> {
        let mut handled_ids: HashSet<String> = HashSet::new();
        // Two distinct remote ids can resolve to one local series; each series is reconciled
        // at most once per run or the clobbering flip-flop from dupes would repeat here too.
        let mut handled_series: HashSet<SeriesId> = HashSet::new();

        for (entry, matched) in resolved {
            let Some(series_id) = *matched else {
                counts.unmatched += 1;
                continue;
            };
            counts.matched += 1;
            handled_ids.insert(entry.external_id().to_owned());
            if !handled_series.insert(series_id) {
                continue; // this series was already reconciled against a duplicate remote row
            }
            self.reconcile_series(
                run,
                series_id,
                entry.external_id(),
                Some(entry),
                local,
                counts,
            )
            .await?;
        }
        Ok(handled_ids)
    }

    /// Phase 4, local-driven: watchlist entries that map to a remote id not present in the
    /// fetched list need creating on the remote side.
    async fn reconcile_watchlist(
        &self,
        run: &RunContext<'_>,
        handled_ids: &HashSet<String>,
        local: &LocalState,
        counts: &mut ReconcileCounts,
    ) -> anyhow::Result<()> {
        let watchlist = tracking::watchlist_list(&self.pool, run.user_id).await?;
        for wl in &watchlist {
            counts.considered += 1;
            let Some(external_id) = self
                .resolver
                .media_id_for_series(run.provider, run.slug, run.access, wl.series_id)
                .await?
            else {
                counts.unmapped += 1;
                continue;
            };
            if handled_ids.contains(&external_id) {
                continue; // already reconciled in the remote-driven pass
            }
            self.reconcile_series(run, wl.series_id, &external_id, None, local, counts)
                .await?;
        }
        Ok(())
    }

    /// Reconcile one mapped series against the remote. `remote` is `None` when the series is not
    /// present on the remote yet (it must be created there). Every decision is [`super::plan`]'s;
    /// this method only performs them and counts what it did.
    async fn reconcile_series(
        &self,
        run: &RunContext<'_>,
        series_id: SeriesId,
        external_id: &str,
        remote: Option<&RemoteEntry>,
        local: &LocalState,
        counts: &mut ReconcileCounts,
    ) -> anyhow::Result<()> {
        let side = local.side(series_id);
        match plan_series(&side, remote) {
            SeriesPlan::Skip => {
                counts.skipped += 1;
                Ok(())
            }
            SeriesPlan::CreateRemote { status, progress } => {
                self.apply_create_remote(run, series_id, external_id, status, progress, counts)
                    .await
            }
            SeriesPlan::Merge => {
                let remote = remote.expect("plan_series only asks for a merge when remote is set");
                let ancestor = self.load_ancestor(series_id, run.slug).await?;
                let plan = plan_merge(&side, remote, &ancestor, run.policy);
                self.apply_merge(run, series_id, external_id, &plan, counts)
                    .await
            }
        }
    }

    /// The common ancestor recorded at the last successful reconciliation.
    async fn load_ancestor(&self, series_id: SeriesId, slug: &str) -> anyhow::Result<Ancestor> {
        let Some(s) = sync::get_snapshot(&self.pool, series_id, slug).await? else {
            return Ok(Ancestor::default());
        };
        Ok(Ancestor {
            local_progress: s.last_synced_local_progress,
            remote_progress: s.last_synced_remote_progress,
            local_status: s
                .last_synced_local_status
                .as_deref()
                .and_then(|t| t.parse::<WatchStatus>().ok()),
            remote_status: s
                .last_synced_remote_status
                .as_deref()
                .and_then(|t| t.parse::<WatchStatus>().ok()),
        })
    }

    /// First push of a series the remote does not have yet: create it, then record the state
    /// both sides now agree on.
    async fn apply_create_remote(
        &self,
        run: &RunContext<'_>,
        series_id: SeriesId,
        external_id: &str,
        status: WatchStatus,
        progress: f64,
        counts: &mut ReconcileCounts,
    ) -> anyhow::Result<()> {
        run.provider
            .save_entry(run.access, external_id, status, progress)
            .await?;
        self.record_snapshot(run, series_id, progress, status)
            .await?;
        counts.pushed += 1;
        self.append_history(
            run,
            series_id,
            "push",
            &serde_json::json!({ "created": true, "progress": progress }),
        )
        .await;
        Ok(())
    }

    /// Perform a decided merge: the optional watchlist import, then each field, then the single
    /// remote write, then the refreshed snapshot.
    async fn apply_merge(
        &self,
        run: &RunContext<'_>,
        series_id: SeriesId,
        external_id: &str,
        plan: &MergePlan,
        counts: &mut ReconcileCounts,
    ) -> anyhow::Result<()> {
        if let Some(status) = plan.import_status {
            tracking::watchlist_set_status(&self.pool, run.user_id, series_id, status).await?;
        }

        match plan.progress.action {
            MergeAction::PullRemote => {
                tracking::progress_set(&self.pool, run.user_id, series_id, plan.progress.remote)
                    .await?;
                counts.pulled += 1;
                self.append_history(
                    run,
                    series_id,
                    "pull",
                    &serde_json::json!({
                        "field": "progress", "from": plan.progress.local,
                        "to": plan.progress.remote, "policy": run.policy.as_str()
                    }),
                )
                .await;
            }
            MergeAction::Conflict => {
                sync::insert_conflict(
                    &self.pool,
                    &sync::NewConflict {
                        user_id: run.user_id,
                        series_id,
                        provider: run.slug,
                        field: "progress",
                        local_value: &plan.progress.local.to_string(),
                        remote_value: &plan.progress.remote.to_string(),
                    },
                )
                .await?;
                counts.conflicts += 1;
                self.append_history(
                    run,
                    series_id,
                    "conflict_auto",
                    &serde_json::json!({ "field": "progress",
                        "local": plan.progress.local, "remote": plan.progress.remote }),
                )
                .await;
            }
            MergeAction::PushLocal | MergeAction::Noop => {}
        }

        match plan.status.action {
            // An imported series was just written above; pulling it again would double-count.
            MergeAction::PullRemote if plan.import_status.is_none() => {
                tracking::watchlist_set_status(
                    &self.pool,
                    run.user_id,
                    series_id,
                    plan.status.remote,
                )
                .await?;
                counts.pulled += 1;
            }
            MergeAction::Conflict => {
                sync::insert_conflict(
                    &self.pool,
                    &sync::NewConflict {
                        user_id: run.user_id,
                        series_id,
                        provider: run.slug,
                        field: "status",
                        local_value: plan.status.local.as_str(),
                        remote_value: plan.status.remote.as_str(),
                    },
                )
                .await?;
                counts.conflicts += 1;
            }
            _ => {}
        }

        if let Some((status, progress)) = plan.remote_write {
            run.provider
                .save_entry(run.access, external_id, status, progress)
                .await?;
            counts.pushed += 1;
            self.append_history(
                run,
                series_id,
                "push",
                &serde_json::json!({ "progress": progress,
                    "status": status.as_str(), "policy": run.policy.as_str() }),
            )
            .await;
        }

        if let Some((progress, status)) = plan.snapshot {
            self.record_snapshot(run, series_id, progress, status)
                .await?;
        }
        Ok(())
    }

    /// Record the state both sides are known to agree on. Local and remote are written with the
    /// same value because that is what agreement means at this point.
    async fn record_snapshot(
        &self,
        run: &RunContext<'_>,
        series_id: SeriesId,
        progress: f64,
        status: WatchStatus,
    ) -> anyhow::Result<()> {
        sync::record_snapshot(
            &self.pool,
            &sync::AgreedSnapshot {
                series_id,
                provider: run.slug,
                local_progress: progress,
                remote_progress: progress,
                local_status: status.as_str(),
                remote_status: status.as_str(),
            },
        )
        .await?;
        Ok(())
    }

    /// History is an audit trail, not a control path: a failed append is dropped rather than
    /// failing the reconciliation that produced it.
    async fn append_history(
        &self,
        run: &RunContext<'_>,
        series_id: SeriesId,
        action: &str,
        detail: &serde_json::Value,
    ) {
        let _ = sync::append_history(&self.pool, run.user_id, series_id, run.slug, action, detail)
            .await;
    }
}

#[cfg(test)]
mod tests {
    // Progress values under test are small, exactly-representable integers, so exact float
    // comparison is correct here.
    #![expect(
        clippy::float_cmp,
        reason = "reconciliation decides by exact equality of progress values"
    )]

    use super::dedupe_latest_by_external_id;
    use crate::provider::{RemoteEntry, RemoteMetadata};
    use tankovault_domain::{ContentType, SeriesStatus, WatchStatus};
    use time::OffsetDateTime;

    fn entry(external_id: &str, progress: f64, updated_unix: i64) -> RemoteEntry {
        RemoteEntry {
            status: WatchStatus::Reading,
            progress,
            updated_at: OffsetDateTime::from_unix_timestamp(updated_unix).unwrap(),
            metadata: RemoteMetadata {
                external_id: external_id.to_owned(),
                titles: vec![format!("title-{external_id}")],
                description: None,
                cover_url: None,
                start_year: None,
                content_type: ContentType::Unknown,
                series_status: SeriesStatus::Unknown,
                tags: Vec::new(),
                authors: Vec::new(),
            },
        }
    }

    /// Pins an AniList anomaly: the same media returned twice, a stale row and a fresh one.
    #[test]
    fn dedupe_keeps_freshest_occurrence_per_external_id() {
        let stale = entry("143056", 23.0, 1_654_724_356); // 2022-06-08
        let fresh = entry("143056", 182.0, 1_784_801_432); // 2026-...
        let other = entry("129918", 182.0, 1_750_314_982);

        // Order must not matter: the newest `updated_at` wins regardless of input position.
        for input in [
            vec![fresh.clone(), stale.clone(), other.clone()],
            vec![stale.clone(), fresh.clone(), other.clone()],
        ] {
            let mut out = dedupe_latest_by_external_id(input);
            out.sort_by(|a, b| a.external_id().cmp(b.external_id()));

            assert_eq!(out.len(), 2, "one row per distinct external id");
            let dup = out.iter().find(|e| e.external_id() == "143056").unwrap();
            assert_eq!(dup.progress, 182.0, "freshest occurrence must win");
            let single = out.iter().find(|e| e.external_id() == "129918").unwrap();
            assert_eq!(
                single.progress, 182.0,
                "non-duplicated entries pass through"
            );
        }
    }

    #[test]
    fn dedupe_passes_through_a_list_without_duplicates() {
        let input = vec![
            entry("1", 5.0, 100),
            entry("2", 6.0, 200),
            entry("3", 7.0, 300),
        ];
        let out = dedupe_latest_by_external_id(input);
        assert_eq!(out.len(), 3);
    }
}
