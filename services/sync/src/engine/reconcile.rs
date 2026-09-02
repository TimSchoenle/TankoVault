//! Full three-way reconciliation of a linked account. Owns the *I/O* half — fetching, resolving,
//! persisting, applying — while every merge rule itself lives in [`super::plan`].
//!
//! Every per-series decision a run takes is journalled in `sync_decisions`, including the ones
//! that changed nothing: the entries that matched no local series, the series skipped as
//! excluded, and the fields both sides already agreed on. What a run *considered* is as much
//! part of explaining an automatic sync as what it wrote.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use secrecy::SecretString;
use serde_json::json;
use time::OffsetDateTime;
use uuid::Uuid;

use tankovault_db::PgPool;
use tankovault_db::repo::sync::NewSyncDecision;
use tankovault_db::repo::{sync, tracking};
use tankovault_domain::{SeriesId, UserId, WatchStatus};

use super::accounts::AccountService;
use super::linked::{GroupSide, LinkedMember, LinkedSeries, plan_group, plan_mirror};
use super::metadata::MetadataWriter;
use super::plan::{Ancestor, MergePlan, SeriesPlan, plan_merge, plan_series};
use super::registry::ProviderRegistry;
use super::resolve::{MatchOutcome, SeriesResolver};
use super::tokens::TokenVault;
use crate::mapping::{ConflictPolicy, MergeAction};
use crate::provider::{ExternalProvider, RemoteEntry};
use tankovault_contracts::sync::{PullReport, PushReport};

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

/// A run's counters and its decision journal, threaded together because every step that moves a
/// counter has a reason worth writing down and the two would otherwise drift apart.
#[derive(Debug, Default)]
struct RunState {
    counts: ReconcileCounts,
    decisions: Vec<NewSyncDecision>,
}

impl RunState {
    /// Start a decision and hand back a handle to fill in. The identifying half comes from the
    /// run, so a caller cannot journal a decision against the wrong user or provider.
    fn note(
        &mut self,
        run: &RunContext<'_>,
        scope: &str,
        action: &str,
        reason: &str,
    ) -> &mut NewSyncDecision {
        self.decisions.push(NewSyncDecision::new(
            run.user_id,
            run.slug,
            scope,
            action,
            reason,
        ));
        self.decisions
            .last_mut()
            .expect("a decision was just pushed")
    }
}

/// How long a series' catalogue metadata is left alone after an enrichment attempt before a
/// list reconciliation refreshes it again. The tokenless sweep, not this pass, keeps metadata
/// current in between.
const METADATA_REFRESH_INTERVAL: time::Duration = time::Duration::WEEK;

/// Why a series nobody asked about was written: it shares a remote entry with the one that was.
/// Stable, because it is persisted and rendered in the decision journal.
const LINKED_REASON: &str = "linked_to_the_same_remote_entry";

/// One user's local sync-relevant state for one provider, read once per reconciliation run
/// instead of per series. Sound only because a run reconciles each series **at most once**
/// (`handled_series`/`handled_ids` in [`Reconciler::reconcile_account`] guarantee it), so no
/// series is read here after that same run has written to it. A linked group counts as one
/// series for that purpose: every member is marked handled when the group is reconciled,
/// because the mirror has already written all of them.
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

    /// This run's view of one linked group, in the shape the pure planner takes.
    fn members(&self, series_ids: &[SeriesId]) -> Vec<LinkedMember> {
        series_ids
            .iter()
            .map(|&series_id| LinkedMember {
                series_id,
                progress: self.progress.get(&series_id).copied(),
                status: self.status.get(&series_id).copied(),
                excluded: self.excluded.contains(&series_id),
            })
            .collect()
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
    /// Groups this run's decisions, so the console can read one reconciliation as a unit.
    run_id: Uuid,
    /// The (external id, series) matches an operator has judged wrong for this provider.
    blocked: HashSet<(String, SeriesId)>,
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
    linked: Arc<LinkedSeries>,
}

impl Reconciler {
    pub(crate) const fn new(
        pool: PgPool,
        registry: Arc<ProviderRegistry>,
        tokens: Arc<TokenVault>,
        accounts: Arc<AccountService>,
        resolver: Arc<SeriesResolver>,
        metadata: Arc<MetadataWriter>,
        linked: Arc<LinkedSeries>,
    ) -> Self {
        Self {
            pool,
            registry,
            tokens,
            accounts,
            resolver,
            metadata,
            linked,
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
            run_id: Uuid::now_v7(),
            blocked: self.resolver.blocklist(slug).await?,
        };

        let mut state = RunState {
            counts: ReconcileCounts {
                fetched: entries.len(),
                ..Default::default()
            },
            decisions: Vec::new(),
        };
        let local = LocalState::load(&self.pool, user_id, slug).await?;

        // Phase 1: resolve every entry to a canonical series (or to nothing).
        let mut resolved: Vec<(&RemoteEntry, MatchOutcome)> = Vec::with_capacity(entries.len());
        for entry in &entries {
            let outcome = self
                .resolver
                .series_for_entry(slug, entry, &run.blocked)
                .await?;
            Self::note_match(&run, entry, &outcome, &mut state);
            resolved.push((entry, outcome));
        }

        // Flushed here as well as at the end: the match decisions are the most expensive half of
        // the journal to lose, and a provider write failing in a later phase would take them with
        // it if they were only written once.
        self.flush(&run, &mut state).await;

        self.persist_fetched(&run, &resolved).await?;
        self.enrich_matched(&run, &resolved, &mut state).await;
        let mut handled_ids = self
            .reconcile_fetched(&run, &resolved, &local, &mut state)
            .await?;
        self.reconcile_watchlist(&run, &mut handled_ids, &local, &mut state)
            .await?;

        sync::mark_synced(
            &self.pool,
            user_id,
            slug,
            Some(&viewer.name),
            OffsetDateTime::now_utc(),
        )
        .await?;
        self.flush(&run, &mut state).await;
        Ok(state.counts)
    }

    /// Write the journal so far and clear it.
    ///
    /// Best-effort, like `sync_history`: the record of a reconciliation must not be able to fail
    /// the reconciliation. Cleared whether or not the write succeeded, so a persistent failure
    /// costs the journal rather than growing the run's memory without bound.
    async fn flush(&self, run: &RunContext<'_>, state: &mut RunState) {
        if state.decisions.is_empty() {
            return;
        }
        let decisions = std::mem::take(&mut state.decisions);
        if let Err(e) = sync::record_sync_decisions(&self.pool, run.run_id, &decisions).await {
            tracing::warn!(error = %e, provider = %run.slug, user_id = %run.user_id,
                count = decisions.len(), "could not journal sync decisions");
        }
    }

    /// Journal how one remote entry resolved — including, and especially, when it resolved to
    /// nothing. An unmatched entry was previously a counter and no more, so the commonest sync
    /// complaint ("the tracker has this and it never syncs") had no evidence at all.
    fn note_match(
        run: &RunContext<'_>,
        entry: &RemoteEntry,
        outcome: &MatchOutcome,
        state: &mut RunState,
    ) {
        let action = if outcome.series_id.is_some() {
            "matched"
        } else {
            "unmatched"
        };
        let decision = state.note(run, "match", action, outcome.reason);
        decision.series_id = outcome.series_id;
        decision.external_id = Some(entry.external_id().to_owned());
        decision.evidence = outcome.evidence.clone();
        if let Some(a) = outcome.assessment {
            decision.match_score = Some(a.score);
            decision.match_signals = a.signals.labels().iter().map(|s| (*s).to_owned()).collect();
        }
        // A fresh mapping is a write; a cache hit and a non-match are not.
        decision.applied =
            outcome.series_id.is_some() && outcome.reason == "title_match_above_threshold";
    }

    /// Persist every fetched snapshot and every resolved mapping in two set-based statements.
    async fn persist_fetched(
        &self,
        run: &RunContext<'_>,
        resolved: &[(&RemoteEntry, MatchOutcome)],
    ) -> anyhow::Result<()> {
        let snapshots: Vec<sync::FetchedRemoteEntry> = resolved
            .iter()
            .map(|(entry, outcome)| sync::FetchedRemoteEntry {
                external_id: entry.external_id().to_owned(),
                title: entry.metadata.titles.first().cloned().unwrap_or_default(),
                status: entry.status.as_str().to_owned(),
                progress: entry.progress,
                content_type: entry.metadata.content_type.as_str().to_owned(),
                start_year: entry.metadata.start_year,
                updated_at: entry.updated_at,
                series_id: outcome.series_id,
            })
            .collect();
        sync::upsert_remote_entries(&self.pool, run.user_id, run.slug, &snapshots).await?;

        let mappings: Vec<(SeriesId, String)> = resolved
            .iter()
            .filter_map(|(entry, outcome)| {
                outcome
                    .series_id
                    .map(|id| (id, entry.external_id().to_owned()))
            })
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
        resolved: &[(&RemoteEntry, MatchOutcome)],
        state: &mut RunState,
    ) {
        let by_series: HashMap<SeriesId, &RemoteEntry> = resolved
            .iter()
            .filter_map(|(entry, outcome)| outcome.series_id.map(|id| (id, *entry)))
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
                Ok(()) => {
                    state.counts.enriched += 1;
                    let decision =
                        state.note(run, "metadata", "enriched", "stale_past_refresh_interval");
                    decision.series_id = Some(row.id);
                    decision.external_id = Some(entry.external_id().to_owned());
                    decision.applied = true;
                    decision.evidence = json!({
                        "titles": entry.metadata.titles,
                        "content_type": entry.metadata.content_type.as_str(),
                        "start_year": entry.metadata.start_year,
                    });
                }
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
        resolved: &[(&RemoteEntry, MatchOutcome)],
        local: &LocalState,
        state: &mut RunState,
    ) -> anyhow::Result<HashSet<String>> {
        let mut handled_ids: HashSet<String> = HashSet::new();
        // Two distinct remote ids can resolve to one local series; each series is reconciled
        // at most once per run or the clobbering flip-flop from dupes would repeat here too.
        // Every member of a reconciled group counts, since the mirror wrote them all.
        let mut handled_series: HashSet<SeriesId> = HashSet::new();

        for (entry, outcome) in resolved {
            let Some(series_id) = outcome.series_id else {
                state.counts.unmatched += 1;
                continue;
            };
            state.counts.matched += 1;
            handled_ids.insert(entry.external_id().to_owned());
            if !handled_series.insert(series_id) {
                // Journalled rather than silently skipped: a provider returning one work twice is
                // an upstream anomaly, and it used to be visible only as a count that did not add
                // up against the fetched total.
                let decision = state.note(
                    run,
                    "series",
                    "skipped",
                    "series_already_reconciled_this_run",
                );
                decision.series_id = Some(series_id);
                decision.external_id = Some(entry.external_id().to_owned());
                continue;
            }
            let group = self
                .reconcile_series(
                    run,
                    series_id,
                    entry.external_id(),
                    Some(entry),
                    local,
                    state,
                )
                .await?;
            handled_series.extend(group);
        }
        Ok(handled_ids)
    }

    /// Phase 4, local-driven: watchlist entries that map to a remote id not present in the
    /// fetched list need creating on the remote side.
    async fn reconcile_watchlist(
        &self,
        run: &RunContext<'_>,
        handled_ids: &mut HashSet<String>,
        local: &LocalState,
        state: &mut RunState,
    ) -> anyhow::Result<()> {
        let watchlist = tracking::watchlist_list(&self.pool, run.user_id).await?;
        for wl in &watchlist {
            state.counts.considered += 1;
            let Some(external_id) = self
                .resolver
                .media_id_for_series(
                    run.provider,
                    run.slug,
                    run.access,
                    wl.series_id,
                    &run.blocked,
                )
                .await?
            else {
                state.counts.unmapped += 1;
                let decision = state.note(run, "match", "unmapped", "no_remote_media_found");
                decision.series_id = Some(wl.series_id);
                continue;
            };
            // Already settled — either in the remote-driven pass, or by an earlier watchlist
            // entry linked to the same remote id. Both wrote every member of the group, and
            // creating the same entry twice from a second member's state would have the two
            // clobber each other on the remote.
            if !handled_ids.insert(external_id.clone()) {
                continue;
            }
            self.reconcile_series(run, wl.series_id, &external_id, None, local, state)
                .await?;
        }
        Ok(())
    }

    /// Reconcile the linked group behind one external id against the remote. `remote` is `None`
    /// when the group is not present on the remote yet (it must be created there). Every decision
    /// is [`super::plan`]'s or [`super::linked`]'s; this method only performs them and counts
    /// what it did.
    ///
    /// `series_id` is the member the caller arrived at, which [`plan_group`] takes as the group's
    /// driver unless it is excluded. Returns the whole group, so the caller can mark every member
    /// handled rather than only that one.
    async fn reconcile_series(
        &self,
        run: &RunContext<'_>,
        series_id: SeriesId,
        external_id: &str,
        remote: Option<&RemoteEntry>,
        local: &LocalState,
        state: &mut RunState,
    ) -> anyhow::Result<Vec<SeriesId>> {
        let mut group = sync::mapping_linked_series(&self.pool, run.slug, external_id).await?;
        // Both callers reach this having just written the mapping, so an empty group is a
        // concurrent deletion rather than an unmapped series; the caller's own series is still
        // the right thing to reconcile.
        if group.is_empty() {
            group.push(series_id);
        }
        let members = local.members(&group);
        let GroupSide { primary, side } = plan_group(&members, series_id);
        match plan_series(&side, remote) {
            SeriesPlan::Skip => {
                state.counts.skipped += 1;
                let decision = state.note(run, "series", "skipped", "excluded_from_sync");
                decision.series_id = Some(primary);
                decision.external_id = Some(external_id.to_owned());
            }
            SeriesPlan::CreateRemote { status, progress } => {
                self.apply_create_remote(
                    run,
                    primary,
                    external_id,
                    status,
                    progress,
                    &members,
                    state,
                )
                .await?;
            }
            SeriesPlan::Merge => {
                let remote = remote.expect("plan_series only asks for a merge when remote is set");
                let ancestor = self.load_ancestor(external_id, run.slug).await?;
                let plan = plan_merge(&side, remote, &ancestor, run.policy);
                self.apply_merge(run, primary, external_id, &plan, &members, state)
                    .await?;
            }
        }
        Ok(group)
    }

    /// The common ancestor the linked group recorded at its last successful reconciliation.
    async fn load_ancestor(&self, external_id: &str, slug: &str) -> anyhow::Result<Ancestor> {
        let Some(s) = sync::get_group_snapshot(&self.pool, slug, external_id).await? else {
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
    #[expect(
        clippy::too_many_arguments,
        reason = "the created values, the series they were created from and the group they \
                  settle for are each needed here; bundling them would only move the list"
    )]
    async fn apply_create_remote(
        &self,
        run: &RunContext<'_>,
        series_id: SeriesId,
        external_id: &str,
        status: WatchStatus,
        progress: f64,
        members: &[LinkedMember],
        state: &mut RunState,
    ) -> anyhow::Result<()> {
        run.provider
            .save_entry(run.access, external_id, status, progress)
            .await?;
        self.settle(
            run,
            series_id,
            external_id,
            members,
            progress,
            status,
            state,
        )
        .await?;
        state.counts.pushed += 1;
        let decision = state.note(
            run,
            "series",
            "create_remote",
            "absent_from_the_remote_library",
        );
        decision.series_id = Some(series_id);
        decision.external_id = Some(external_id.to_owned());
        decision.applied = true;
        decision.policy = Some(run.policy.as_str().to_owned());
        decision.local_after = Some(progress.to_string());
        decision.remote_after = Some(progress.to_string());
        decision.evidence = json!({ "status": status.as_str(), "created": true });

        self.append_history(
            run,
            series_id,
            "push",
            &json!({ "created": true, "progress": progress }),
        )
        .await;
        Ok(())
    }

    /// Perform a decided merge: the optional watchlist import, then each field, then the single
    /// remote write, then the refreshed snapshot.
    #[expect(
        clippy::too_many_lines,
        reason = "one arm per merge action per field, each journalling the values it acted on; \
                  splitting it would separate a write from the record of that write"
    )]
    async fn apply_merge(
        &self,
        run: &RunContext<'_>,
        series_id: SeriesId,
        external_id: &str,
        plan: &MergePlan,
        members: &[LinkedMember],
        state: &mut RunState,
    ) -> anyhow::Result<()> {
        if let Some(status) = plan.import_status {
            tracking::watchlist_set_status(&self.pool, run.user_id, series_id, status).await?;
            let decision = state.note(run, "status", "import_status", "absent_from_the_watchlist");
            decision.series_id = Some(series_id);
            decision.external_id = Some(external_id.to_owned());
            decision.applied = true;
            decision.remote_after = Some(status.as_str().to_owned());
            decision.local_after = Some(status.as_str().to_owned());
        }

        match plan.progress.action {
            MergeAction::PullRemote => {
                tracking::progress_set(&self.pool, run.user_id, series_id, plan.progress.remote)
                    .await?;
                state.counts.pulled += 1;
                Self::note_field(
                    run,
                    state,
                    series_id,
                    external_id,
                    "progress",
                    "pull",
                    plan.progress.reason,
                    (
                        Some(plan.progress.local.to_string()),
                        Some(plan.progress.remote.to_string()),
                    ),
                    (
                        plan.progress.ancestor.0.map(|v| v.to_string()),
                        plan.progress.ancestor.1.map(|v| v.to_string()),
                    ),
                    true,
                );
                self.append_history(
                    run,
                    series_id,
                    "pull",
                    &json!({
                        "field": "progress", "from": plan.progress.local,
                        "to": plan.progress.remote, "policy": run.policy.as_str(),
                        "reason": plan.progress.reason
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
                state.counts.conflicts += 1;
                Self::note_field(
                    run,
                    state,
                    series_id,
                    external_id,
                    "progress",
                    "conflict",
                    plan.progress.reason,
                    (
                        Some(plan.progress.local.to_string()),
                        Some(plan.progress.remote.to_string()),
                    ),
                    (
                        plan.progress.ancestor.0.map(|v| v.to_string()),
                        plan.progress.ancestor.1.map(|v| v.to_string()),
                    ),
                    false,
                );
                self.append_history(
                    run,
                    series_id,
                    "conflict_auto",
                    &json!({ "field": "progress", "reason": plan.progress.reason,
                        "local": plan.progress.local, "remote": plan.progress.remote }),
                )
                .await;
            }
            // A push is journalled once, with the status, where the single remote write happens;
            // a no-op is journalled here because "nothing changed, and here is why" is the answer
            // to the second commonest sync question.
            MergeAction::PushLocal => {}
            MergeAction::Noop => Self::note_field(
                run,
                state,
                series_id,
                external_id,
                "progress",
                "noop",
                plan.progress.reason,
                (
                    Some(plan.progress.local.to_string()),
                    Some(plan.progress.remote.to_string()),
                ),
                (
                    plan.progress.ancestor.0.map(|v| v.to_string()),
                    plan.progress.ancestor.1.map(|v| v.to_string()),
                ),
                false,
            ),
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
                state.counts.pulled += 1;
                Self::note_field(
                    run,
                    state,
                    series_id,
                    external_id,
                    "status",
                    "pull",
                    plan.status.reason,
                    (
                        Some(plan.status.local.as_str().to_owned()),
                        Some(plan.status.remote.as_str().to_owned()),
                    ),
                    (
                        plan.status.ancestor.0.map(|v| v.as_str().to_owned()),
                        plan.status.ancestor.1.map(|v| v.as_str().to_owned()),
                    ),
                    true,
                );
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
                state.counts.conflicts += 1;
                Self::note_field(
                    run,
                    state,
                    series_id,
                    external_id,
                    "status",
                    "conflict",
                    plan.status.reason,
                    (
                        Some(plan.status.local.as_str().to_owned()),
                        Some(plan.status.remote.as_str().to_owned()),
                    ),
                    (
                        plan.status.ancestor.0.map(|v| v.as_str().to_owned()),
                        plan.status.ancestor.1.map(|v| v.as_str().to_owned()),
                    ),
                    false,
                );
            }
            _ => {}
        }

        if let Some((status, progress)) = plan.remote_write {
            run.provider
                .save_entry(run.access, external_id, status, progress)
                .await?;
            state.counts.pushed += 1;
            // One decision for one write, naming whichever field asked for it. Two rows would
            // claim two provider calls were made, and the whole point of `remote_write` is that
            // there is exactly one.
            let reason = if plan.progress.action == MergeAction::PushLocal {
                plan.progress.reason
            } else {
                plan.status.reason
            };
            Self::note_field(
                run,
                state,
                series_id,
                external_id,
                "progress",
                "push",
                reason,
                (
                    Some(progress.to_string()),
                    Some(plan.progress.remote.to_string()),
                ),
                (
                    plan.progress.ancestor.0.map(|v| v.to_string()),
                    plan.progress.ancestor.1.map(|v| v.to_string()),
                ),
                true,
            );
            if let Some(decision) = state.decisions.last_mut() {
                decision.remote_after = Some(progress.to_string());
                decision.evidence = json!({
                    "status_written": status.as_str(),
                    "progress_written": progress,
                    // Both halves of what the remote held before this one call, because
                    // `save_entry` writes both and undoing it has to restore both. The
                    // `remote_before` column carries only the progress.
                    "remote_status_before": plan.status.remote.as_str(),
                    "remote_progress_before": plan.progress.remote,
                    "driven_by": if plan.progress.action == MergeAction::PushLocal {
                        "progress"
                    } else {
                        "status"
                    },
                });
            }
            self.append_history(
                run,
                series_id,
                "push",
                &json!({ "progress": progress, "status": status.as_str(),
                    "policy": run.policy.as_str(), "reason": reason }),
            )
            .await;
        }

        if let Some((progress, status)) = plan.snapshot {
            self.settle(
                run,
                series_id,
                external_id,
                members,
                progress,
                status,
                state,
            )
            .await?;
        }
        Ok(())
    }

    /// Bring the whole linked group into step with what it and the remote have settled on, and
    /// record that as each member's new common ancestor.
    ///
    /// This is what makes a duplicate stop drifting. The remote holds one entry for the whole
    /// group, so a member the merge did not drive is not "unchanged" — it describes the same
    /// work with a frontier the reader has already moved past, and nothing else in a run would
    /// ever revisit it (the local-driven pass skips it as an already-handled external id).
    ///
    /// The driving series is written here too, since the settled progress may have come from a
    /// different member, but it is not journalled here: the merge already recorded a decision
    /// for it. Nothing is settled while a field is in conflict, so nothing is fanned out either
    /// — [`MergePlan::snapshot`] is `None` and this is not reached.
    #[expect(
        clippy::too_many_arguments,
        reason = "the settled pair, the series it was settled on, its group and the journal are \
                  each needed here; bundling them would only move the list one line up"
    )]
    async fn settle(
        &self,
        run: &RunContext<'_>,
        series_id: SeriesId,
        external_id: &str,
        members: &[LinkedMember],
        progress: f64,
        status: WatchStatus,
        state: &mut RunState,
    ) -> anyhow::Result<()> {
        let writes = plan_mirror(members, progress, status);
        // A driving series with nothing local to keep in step is absent from the writes, so its
        // snapshot still has to be recorded by hand.
        if !writes.iter().any(|w| w.series_id == series_id) {
            self.record_snapshot(run, series_id, progress, status)
                .await?;
        }
        if writes.is_empty() {
            return Ok(());
        }
        self.linked
            .apply(run.user_id, run.slug, &writes, (progress, status))
            .await?;
        for write in &writes {
            if write.series_id == series_id {
                continue; // journalled and counted by the merge that drove it
            }
            let before = members.iter().find(|m| m.series_id == write.series_id);
            if write.progress.is_some() {
                state.counts.pulled += 1;
                Self::note_field(
                    run,
                    state,
                    write.series_id,
                    external_id,
                    "progress",
                    "pull",
                    LINKED_REASON,
                    (
                        before.and_then(|m| m.progress).map(|(p, _)| p.to_string()),
                        Some(progress.to_string()),
                    ),
                    (None, None),
                    true,
                );
                self.append_history(
                    run,
                    write.series_id,
                    "pull",
                    &json!({
                        "field": "progress", "to": progress, "reason": LINKED_REASON,
                        "linked_to": series_id, "external_id": external_id
                    }),
                )
                .await;
            }
            if write.status.is_some() {
                state.counts.pulled += 1;
                Self::note_field(
                    run,
                    state,
                    write.series_id,
                    external_id,
                    "status",
                    "pull",
                    LINKED_REASON,
                    (
                        before.and_then(|m| m.status).map(|s| s.as_str().to_owned()),
                        Some(status.as_str().to_owned()),
                    ),
                    (None, None),
                    true,
                );
            }
        }
        Ok(())
    }

    /// Journal one field's decision. Takes the before/after and ancestor pairs already stringified
    /// by the caller, because a progress is a number and a status is an enum and the journal
    /// stores both as text.
    #[expect(
        clippy::too_many_arguments,
        reason = "every argument is one column of the row being written; bundling them into a \
                  struct would only move the same list one line up"
    )]
    fn note_field(
        run: &RunContext<'_>,
        state: &mut RunState,
        series_id: SeriesId,
        external_id: &str,
        scope: &str,
        action: &str,
        reason: &str,
        values: (Option<String>, Option<String>),
        ancestor: (Option<String>, Option<String>),
        applied: bool,
    ) {
        let (local, remote) = values;
        let decision = state.note(run, scope, action, reason);
        decision.series_id = Some(series_id);
        decision.external_id = Some(external_id.to_owned());
        decision.policy = Some(run.policy.as_str().to_owned());
        decision.applied = applied;
        // The written side gets an `after` as well; the untouched side keeps only its `before`,
        // and is the only side that needs no clone.
        match action {
            "pull" => decision.local_after.clone_from(&remote),
            "push" => decision.remote_after.clone_from(&local),
            _ => {}
        }
        decision.local_before = local;
        decision.remote_before = remote;
        decision.ancestor_local = ancestor.0;
        decision.ancestor_remote = ancestor.1;
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
    use tankovault_domain::WatchStatus;
    use time::OffsetDateTime;

    fn entry(external_id: &str, progress: f64, updated_unix: i64) -> RemoteEntry {
        RemoteEntry {
            status: WatchStatus::Reading,
            progress,
            updated_at: OffsetDateTime::from_unix_timestamp(updated_unix).unwrap(),
            metadata: RemoteMetadata {
                external_id: external_id.to_owned(),
                titles: vec![format!("title-{external_id}")],
                ..RemoteMetadata::default()
            },
        }
    }

    /// Pins an `AniList` anomaly: the same media returned twice, a stale row and a fresh one.
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
