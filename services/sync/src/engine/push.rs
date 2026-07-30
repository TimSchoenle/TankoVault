//! Targeted single-series push (design: immediate targeted push).
//!
//! The fast path taken when a user does something deliberate — marking a chapter read — as
//! opposed to the bulk reconciliation in [`super::reconcile`]. No remote list is fetched and no
//! three-way merge runs: local state wins outright, because the user just asserted it.

use std::sync::Arc;

use anyhow::anyhow;
use serde::Serialize;
use time::OffsetDateTime;

use tankovault_db::PgPool;
use tankovault_db::repo::{sync, tracking};
use tankovault_domain::{SeriesId, UserId, WatchStatus};

use super::registry::ProviderRegistry;
use super::resolve::SeriesResolver;
use super::tokens::TokenVault;
use crate::provider::ExternalProvider;

/// One provider's outcome from a targeted single-series push.
#[derive(Debug, Clone, Serialize)]
pub(crate) struct ProviderPushOutcome {
    pub(crate) provider: String,
    pub(crate) ok: bool,
    pub(crate) error: Option<String>,
}

/// Fans one series' local state out to every provider a user has linked.
pub(crate) struct TargetedPush {
    pool: PgPool,
    registry: Arc<ProviderRegistry>,
    tokens: Arc<TokenVault>,
    resolver: Arc<SeriesResolver>,
}

impl TargetedPush {
    pub(crate) const fn new(
        pool: PgPool,
        registry: Arc<ProviderRegistry>,
        tokens: Arc<TokenVault>,
        resolver: Arc<SeriesResolver>,
    ) -> Self {
        Self {
            pool,
            registry,
            tokens,
            resolver,
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
        // Reactive push is gated on the same two switches every sync path respects (design v2
        // §B.4): automatic sync must be enabled for the account, and the series must not be
        // excluded (§A.5). Neither check existed before.
        match sync::get_account(&self.pool, user_id, slug).await? {
            Some(a) if a.auto_sync_enabled => {}
            _ => return Ok(()),
        }
        if tracking::is_sync_excluded(&self.pool, user_id, series_id, slug).await? {
            return Ok(());
        }

        let access = self.tokens.access(slug, provider, user_id).await?;
        let external_id = self
            .resolver
            .media_id_for_series(provider, slug, &access, series_id)
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
        Ok(())
    }
}
