//! Targeted single-series push: the fast path when a user asserts state directly (e.g. marking
//! a chapter read), as opposed to bulk reconciliation in [`super::reconcile`]. Local state wins
//! outright — no remote fetch, no three-way merge.

use std::sync::Arc;

use anyhow::anyhow;
use serde_json::json;
use time::OffsetDateTime;

use tankovault_db::PgPool;
use tankovault_db::repo::{sync, tracking};
use tankovault_domain::{SeriesId, UserId, WatchStatus};

use super::linked::{LinkedSeries, plan_mirror};
use super::registry::ProviderRegistry;
use super::resolve::SeriesResolver;
use super::tokens::TokenVault;
use crate::provider::ExternalProvider;

use tankovault_contracts::sync::ProviderPushOutcome;

/// Fans one series' local state out to every provider a user has linked.
pub(crate) struct TargetedPush {
    pool: PgPool,
    registry: Arc<ProviderRegistry>,
    tokens: Arc<TokenVault>,
    resolver: Arc<SeriesResolver>,
    linked: Arc<LinkedSeries>,
}

impl TargetedPush {
    pub(crate) const fn new(
        pool: PgPool,
        registry: Arc<ProviderRegistry>,
        tokens: Arc<TokenVault>,
        resolver: Arc<SeriesResolver>,
        linked: Arc<LinkedSeries>,
    ) -> Self {
        Self {
            pool,
            registry,
            tokens,
            resolver,
            linked,
        }
    }

    /// Push one series to every linked provider. Never fails the caller; every outcome
    /// (including failures) is best-effort logged and recorded via `record_sync_error`.
    pub(crate) async fn push_series(
        &self,
        user_id: UserId,
        series_id: SeriesId,
    ) -> Vec<ProviderPushOutcome> {
        let linked = match sync::list_linked_providers(&self.pool, user_id).await {
            Ok(l) => l,
            Err(e) => {
                tracing::warn!(
                    error = %e,
                    %user_id,
                    "could not list linked providers for targeted push"
                );
                return Vec::new();
            }
        };
        let mut outcomes = Vec::with_capacity(linked.len());
        for slug in linked {
            let Some(provider) = self.registry.try_get(&slug) else {
                continue;
            };
            outcomes.push(self.push_one(provider, &slug, user_id, series_id).await);
        }
        outcomes
    }

    async fn push_one(
        &self,
        provider: &dyn ExternalProvider,
        slug: &str,
        user_id: UserId,
        series_id: SeriesId,
    ) -> ProviderPushOutcome {
        match self.push_inner(provider, slug, user_id, series_id).await {
            Ok(()) => {
                let _ =
                    sync::mark_synced(&self.pool, user_id, slug, None, OffsetDateTime::now_utc())
                        .await;
                ProviderPushOutcome {
                    provider: slug.to_owned(),
                    ok: true,
                    error: None,
                }
            }
            Err(e) => {
                tracing::warn!(
                    error = %e,
                    provider = slug,
                    %user_id,
                    %series_id,
                    "targeted sync push failed"
                );
                let _ = sync::record_sync_error(&self.pool, user_id, slug, &e.to_string()).await;
                ProviderPushOutcome {
                    provider: slug.to_owned(),
                    ok: false,
                    error: Some(e.to_string()),
                }
            }
        }
    }

    async fn push_inner(
        &self,
        provider: &dyn ExternalProvider,
        slug: &str,
        user_id: UserId,
        series_id: SeriesId,
    ) -> anyhow::Result<()> {
        // Gated on the same switches every sync path respects: auto-sync enabled for the
        // account, and the series not excluded.
        match sync::get_account(&self.pool, user_id, slug).await? {
            Some(a) if a.auto_sync_enabled => {}
            _ => return Ok(()),
        }
        if tracking::is_sync_excluded(&self.pool, user_id, series_id, slug).await? {
            return Ok(());
        }

        let access = self.tokens.access(slug, provider, user_id).await?;
        // The same blocklist the scheduled reconciliation reads. A targeted push is the one path
        // that could otherwise re-create a mapping an operator has just rejected, because it runs
        // the moment a reader marks a chapter read rather than on the next sweep.
        let blocked = self.resolver.blocklist(slug).await?;
        let external_id = self
            .resolver
            .media_id_for_series(provider, slug, &access, series_id, &blocked)
            .await?
            .ok_or_else(|| {
                anyhow!(
                    "no resolvable {} entry for this series",
                    provider.display_name()
                )
            })?;
        let status = tracking::watchlist_status_get(&self.pool, user_id, series_id)
            .await?
            .unwrap_or(WatchStatus::Reading);
        let progress = tracking::progress_state(&self.pool, user_id, series_id)
            .await?
            .map_or(0.0, |(p, _)| p);
        provider
            .save_entry(&access, &external_id, status, progress)
            .await?;
        sync::upsert_mapping(&self.pool, series_id, slug, &external_id).await?;
        self.settle(user_id, slug, series_id, &external_id, progress, status)
            .await
    }

    /// Record what this push settled, for the pushed series and for every other local series
    /// mapped to the same remote entry.
    ///
    /// The reader asserted a frontier for a *work*, and the provider keeps one entry per work:
    /// leaving a catalogue duplicate on its old number would show the same chapter read on one
    /// copy and unread on the other, and nothing would ever correct it — a reconciliation
    /// reconciles the remote entry once and skips the rest of the group as already handled.
    ///
    /// The ancestor snapshot is refreshed here too, for the same reason the reconciliation
    /// refreshes it: both sides now hold this value, so the next run must read it as "neither
    /// side changed" rather than re-deciding a remote that has moved since against an ancestor
    /// from before this push.
    ///
    /// Only the mirrored series are journalled in `sync_history`. The pushed series' own change
    /// is the reader's own action, already visible where they made it; a write to a series they
    /// never touched is not, and that is what the log is for.
    async fn settle(
        &self,
        user_id: UserId,
        slug: &str,
        series_id: SeriesId,
        external_id: &str,
        progress: f64,
        status: WatchStatus,
    ) -> anyhow::Result<()> {
        let members = self.linked.members(user_id, slug, external_id).await?;
        let writes = plan_mirror(&members, progress, status);
        // A series with no local rows at all is absent from the writes, so its snapshot still
        // has to be recorded by hand.
        if !writes.iter().any(|w| w.series_id == series_id) {
            sync::record_snapshot(
                &self.pool,
                &sync::AgreedSnapshot {
                    series_id,
                    provider: slug,
                    local_progress: progress,
                    remote_progress: progress,
                    local_status: status.as_str(),
                    remote_status: status.as_str(),
                },
            )
            .await?;
        }
        self.linked
            .apply(user_id, slug, &writes, (progress, status))
            .await?;
        for write in &writes {
            if write.series_id == series_id || (write.progress.is_none() && write.status.is_none())
            {
                continue; // the reader's own series, or one already in step
            }
            let _ = sync::append_history(
                &self.pool,
                user_id,
                write.series_id,
                slug,
                "pull",
                &json!({
                    "field": "progress", "to": progress, "status": status.as_str(),
                    "reason": "linked_to_the_same_remote_entry",
                    "linked_to": series_id, "external_id": external_id
                }),
            )
            .await;
        }
        Ok(())
    }
}
