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
mod tokens;

use std::collections::HashMap;
use std::sync::Arc;

use tankovault_auth::Sealer;
use tankovault_config::MatchingConfig;
use tankovault_contracts::sync::{AccountSettings, AccountStatus, ProviderInfo};
use tankovault_db::PgPool;
use tankovault_db::repo::sync;
use tankovault_domain::{MetadataPriority, SeriesId, UserId};

use crate::mapping::ConflictPolicy;
use crate::provider::ExternalProvider;

pub(crate) use enrich::EnrichReport;
pub(crate) use push::ProviderPushOutcome;
pub(crate) use reconcile::{PullReport, PushReport};

use accounts::AccountService;
use conflicts::ConflictService;
use enrich::Enricher;
use metadata::MetadataWriter;
use push::TargetedPush;
use reconcile::Reconciler;
use registry::ProviderRegistry;
use resolve::SeriesResolver;
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
}

impl SyncEngine {
    pub(crate) fn new(
        pool: PgPool,
        secret: Sealer,
        default_policy: ConflictPolicy,
        metadata_priority: MetadataPriority,
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
        let metadata = Arc::new(MetadataWriter::new(pool.clone(), metadata_priority));

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
