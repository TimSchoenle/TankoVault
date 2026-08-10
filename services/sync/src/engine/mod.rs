//! Sync engine: OAuth linking plus provider ⇆ local pull/push, a targeted single-series push,
//! metadata enrichment, and the multi-provider registry. [`SyncEngine`] is a facade delegating
//! to the collaborator modules below; it holds no logic of its own.

mod accounts;
mod conflicts;
mod enrich;
mod metadata;
mod plan;
mod push;
mod reconcile;
mod registry;
mod resolve;
mod revert;
mod tokens;

use std::collections::HashMap;
use std::sync::Arc;

use tankovault_auth::Sealer;
use tankovault_config::MatchingConfig;
use tankovault_contracts::sync::{AccountSettings, AccountStatus, ProviderInfo};
use tankovault_db::PgPool;
use tankovault_db::repo::sync;
use tankovault_domain::{MetadataPriority, SeriesId, TagBlocklist, UserId};

use crate::mapping::ConflictPolicy;
use crate::provider::ExternalProvider;

// The reports these routes answer with are published API types, not engine-private ones:
// `services/api` re-publishes each verbatim, so they live in `tankovault_contracts` and are
// re-exported here only so the collaborators below keep their unqualified names.
pub(crate) use tankovault_contracts::sync::{
    EnrichReport, ProviderPushOutcome, PullReport, PushReport, RevertReport,
};

use accounts::AccountService;
use conflicts::ConflictService;
use enrich::Enricher;
use metadata::MetadataWriter;
use push::TargetedPush;
use reconcile::Reconciler;
use registry::ProviderRegistry;
use resolve::SeriesResolver;
use revert::RevertService;
use tokens::TokenVault;

/// The stateful sync engine, shared behind an `Arc` in service state. A façade over the
/// collaborators listed in the module docs; it delegates and holds no logic itself.
pub(crate) struct SyncEngine {
    registry: Arc<ProviderRegistry>,
    accounts: Arc<AccountService>,
    conflicts: ConflictService,
    reconciler: Reconciler,
    targeted_push: TargetedPush,
    enricher: Enricher,
    /// Undoing a journalled decision, and the durable refusal that stops it recurring.
    reverts: RevertService,
}

impl SyncEngine {
    pub(crate) fn new(
        pool: PgPool,
        secret: Sealer,
        default_policy: ConflictPolicy,
        metadata_priority: MetadataPriority,
        tag_blocklist: TagBlocklist,
        matching: &MatchingConfig,
        providers: HashMap<&'static str, Box<dyn ExternalProvider>>,
    ) -> Self {
        let registry = Arc::new(ProviderRegistry::new(providers));
        let tokens = Arc::new(TokenVault::new(pool.clone(), secret));
        // Same `MatchingConfig` the worker's ingest canonicalisation reads, so the two paths
        // can't disagree on match thresholds.
        let resolver = Arc::new(SeriesResolver::new(
            pool.clone(),
            matching.thresholds(),
            matching.candidate_limit,
        ));
        let accounts = Arc::new(AccountService::new(
            pool.clone(),
            Arc::clone(&registry),
            Arc::clone(&tokens),
            default_policy,
        ));
        // One writer behind both metadata paths so they can't disagree on field priority.
        let metadata = Arc::new(MetadataWriter::new(
            pool.clone(),
            metadata_priority,
            tag_blocklist,
        ));

        Self {
            conflicts: ConflictService::new(
                pool.clone(),
                Arc::clone(&registry),
                Arc::clone(&tokens),
            ),
            reconciler: Reconciler::new(
                pool.clone(),
                Arc::clone(&registry),
                Arc::clone(&tokens),
                Arc::clone(&accounts),
                Arc::clone(&resolver),
                Arc::clone(&metadata),
            ),
            reverts: RevertService::new(pool.clone(), Arc::clone(&registry), Arc::clone(&tokens)),
            targeted_push: TargetedPush::new(pool.clone(), Arc::clone(&registry), tokens, resolver),
            enricher: Enricher::new(pool, Arc::clone(&registry), metadata),
            registry,
            accounts,
        }
    }

    /// The registered providers, for `GET /v1/sync/providers`.
    #[must_use]
    pub(crate) fn registry(&self) -> Vec<ProviderInfo> {
        self.registry.list()
    }

    /// The `provider`'s consent URL to redirect a user to.
    pub(crate) fn authorize_url(&self, slug: &str) -> anyhow::Result<String> {
        self.accounts.authorize_url(slug)
    }

    /// Exchange an OAuth `code` and persist the (encrypted) tokens for `user_id`.
    pub(crate) async fn link(&self, slug: &str, user_id: UserId, code: &str) -> anyhow::Result<()> {
        self.accounts.link(slug, user_id, code).await
    }

    /// Remove a user's link to `provider`. Returns `true` if an account was removed.
    pub(crate) async fn unlink(&self, slug: &str, user_id: UserId) -> anyhow::Result<bool> {
        self.accounts.unlink(slug, user_id).await
    }

    /// Whether `user_id` has a linked `provider` account, plus its display name and most recent
    /// sync time.
    pub(crate) async fn status(
        &self,
        slug: &str,
        user_id: UserId,
    ) -> anyhow::Result<AccountStatus> {
        self.accounts.status(slug, user_id).await
    }

    /// The account's automatic-sync settings plus its pending-conflict count.
    pub(crate) async fn settings(
        &self,
        slug: &str,
        user_id: UserId,
    ) -> anyhow::Result<AccountSettings> {
        self.accounts.settings(slug, user_id).await
    }

    /// Update the account's automatic-sync settings.
    pub(crate) async fn update_settings(
        &self,
        slug: &str,
        user_id: UserId,
        auto_sync_enabled: Option<bool>,
        conflict_policy: Option<ConflictPolicy>,
    ) -> anyhow::Result<()> {
        self.accounts
            .update_settings(slug, user_id, auto_sync_enabled, conflict_policy)
            .await
    }

    /// A user's pending conflicts across all providers.
    pub(crate) async fn list_conflicts(
        &self,
        user_id: UserId,
    ) -> anyhow::Result<Vec<sync::ConflictRow>> {
        self.conflicts.list(user_id).await
    }

    /// A page of a user's sync history.
    pub(crate) async fn history(
        &self,
        user_id: UserId,
        series_id: Option<SeriesId>,
        provider: Option<&str>,
        page: i64,
    ) -> anyhow::Result<Vec<sync::HistoryRow>> {
        self.conflicts
            .history(user_id, series_id, provider, page)
            .await
    }

    /// Apply a user's manual conflict resolution. `false` if it does not exist / is resolved.
    pub(crate) async fn resolve_conflict(
        &self,
        user_id: UserId,
        conflict_id: uuid::Uuid,
        resolution: &str,
    ) -> anyhow::Result<bool> {
        self.conflicts
            .resolve(user_id, conflict_id, resolution)
            .await
    }

    /// A page of the operator-facing decision journal.
    ///
    /// # Errors
    /// Database failures.
    pub(crate) async fn list_decisions(
        &self,
        filter: &sync::SyncDecisionFilter,
        limit: i64,
        offset: i64,
    ) -> anyhow::Result<Vec<sync::SyncDecisionRow>> {
        self.reverts.list(filter, limit, offset).await
    }

    /// Undo one journalled sync decision.
    ///
    /// # Errors
    /// A decision that changed nothing, was already reverted, or has no inverse; otherwise
    /// database and provider failures.
    pub(crate) async fn revert_decision(
        &self,
        id: uuid::Uuid,
        actor: Option<UserId>,
        reason: &str,
    ) -> anyhow::Result<RevertReport> {
        self.reverts.revert(id, actor, reason).await
    }

    /// Mark one journalled sync decision wrong, optionally refusing the match it made.
    ///
    /// # Errors
    /// Database failures, or a `block_match` request against a decision that named no match.
    pub(crate) async fn flag_decision(
        &self,
        id: uuid::Uuid,
        actor: Option<UserId>,
        reason: &str,
        block_match: bool,
    ) -> anyhow::Result<bool> {
        self.reverts.flag(id, actor, reason, block_match).await
    }

    /// Refuse one (external id, series) correspondence permanently.
    ///
    /// # Errors
    /// Database failures.
    pub(crate) async fn block_match(
        &self,
        provider: &str,
        external_id: &str,
        series_id: SeriesId,
        actor: Option<UserId>,
        reason: &str,
    ) -> anyhow::Result<()> {
        self.reverts
            .block_match(provider, external_id, series_id, actor, reason)
            .await
    }

    /// Manual "pull": the full three-way reconciliation, reported in the `PullReport` shape.
    pub(crate) async fn pull(
        &self,
        slug: &str,
        user_id: UserId,
        policy: Option<ConflictPolicy>,
    ) -> anyhow::Result<PullReport> {
        self.reconciler.pull(slug, user_id, policy).await
    }

    /// Manual "push": the same reconciliation, reported in the `PushReport` shape.
    pub(crate) async fn push(
        &self,
        slug: &str,
        user_id: UserId,
        policy: Option<ConflictPolicy>,
    ) -> anyhow::Result<PushReport> {
        self.reconciler.push(slug, user_id, policy).await
    }

    /// Scheduled reconciliation of every account with automatic sync enabled.
    pub(crate) async fn reconcile_all_accounts(&self) {
        self.reconciler.reconcile_all_accounts().await;
    }

    /// Targeted single-series push to every provider `user_id` has linked.
    pub(crate) async fn push_series(
        &self,
        user_id: UserId,
        series_id: SeriesId,
    ) -> Vec<ProviderPushOutcome> {
        self.targeted_push.push_series(user_id, series_id).await
    }

    /// Tokenless metadata-enrichment sweep over the catalogue.
    pub(crate) async fn enrich_all(
        &self,
        batch_size: i64,
        max_series: usize,
    ) -> anyhow::Result<EnrichReport> {
        self.enricher.enrich_all(batch_size, max_series).await
    }
}
