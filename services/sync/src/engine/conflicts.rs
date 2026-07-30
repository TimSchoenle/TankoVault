//! Pending conflicts and sync history: the read models the console renders, plus the manual
//! resolution that closes a conflict (design v2 §B.6).

use anyhow::anyhow;
use std::sync::Arc;

use tankovault_db::PgPool;
use tankovault_db::repo::{sync, tracking};
use tankovault_domain::{SeriesId, UserId, WatchStatus};

use super::registry::ProviderRegistry;
use super::tokens::TokenVault;

/// One page of history is fixed-size; the caller pages by index.
const HISTORY_PAGE: i64 = 50;

/// Serves a user's conflict queue and history, and applies their manual resolutions.
pub(crate) struct ConflictService {
    pool: PgPool,
    registry: Arc<ProviderRegistry>,
    tokens: Arc<TokenVault>,
}

impl ConflictService {
    pub(crate) const fn new(
        pool: PgPool,
        registry: Arc<ProviderRegistry>,
        tokens: Arc<TokenVault>,
    ) -> Self {
        Self {
            pool,
            registry,
            tokens,
        }
    }

    /// A user's pending conflicts across all providers.
    pub(crate) async fn list(&self, user_id: UserId) -> anyhow::Result<Vec<sync::ConflictRow>> {
        Ok(sync::list_pending_conflicts(&self.pool, user_id).await?)
    }

    /// A page of a user's sync history.
    pub(crate) async fn history(
        &self,
        user_id: UserId,
        series_id: Option<SeriesId>,
        provider: Option<&str>,
        page: i64,
    ) -> anyhow::Result<Vec<sync::HistoryRow>> {
        let offset = page.max(0) * HISTORY_PAGE;
        Ok(sync::list_history(
            &self.pool,
            user_id,
            series_id,
            provider,
            HISTORY_PAGE,
            offset,
        )
        .await?)
    }

    /// Apply a user's manual conflict resolution: write the chosen side, then mark the conflict
    /// resolved and refresh that field's snapshot so it is not re-detected. Returns `false` if
    /// the conflict does not exist / is already resolved.
    pub(crate) async fn resolve(
        &self,
        user_id: UserId,
        conflict_id: uuid::Uuid,
        resolution: &str,
    ) -> anyhow::Result<bool> {
        if resolution != "local" && resolution != "remote" {
            return Err(anyhow!("resolution must be 'local' or 'remote'"));
        }
        let Some(c) = sync::get_pending_conflict(&self.pool, user_id, conflict_id).await? else {
            return Ok(false);
        };
        let series_id = SeriesId::from_uuid(c.series_id);
        let slug = c.provider.as_str();
        let provider = self.registry.get(slug)?;
        let external_id = sync::mapping_external_for_series(&self.pool, series_id, slug)
            .await?
            .ok_or_else(|| anyhow!("series is no longer mapped for {slug}"))?;
        let access = self.tokens.access(slug, provider, user_id).await?;

        match (c.field.as_str(), resolution) {
            ("progress", "remote") => {
                let v: f64 = c.remote_value.parse().unwrap_or(0.0);
                tracking::progress_set(&self.pool, user_id, series_id, v).await?;
            }
            ("progress", _ /* local */) => {
                let v: f64 = c.local_value.parse().unwrap_or(0.0);
                let status = tracking::watchlist_status_get(&self.pool, user_id, series_id)
                    .await?
                    .unwrap_or(WatchStatus::Reading);
                provider
                    .save_entry(&access, &external_id, status, v)
                    .await?;
            }
            ("status", "remote") => {
                let s = c
                    .remote_value
                    .parse::<WatchStatus>()
                    .unwrap_or(WatchStatus::Reading);
                tracking::watchlist_set_status(&self.pool, user_id, series_id, s).await?;
            }
            ("status", _ /* local */) => {
                let s = c
                    .local_value
                    .parse::<WatchStatus>()
                    .unwrap_or(WatchStatus::Reading);
                let progress = tracking::progress_state(&self.pool, user_id, series_id)
                    .await?
                    .map_or(0.0, |(p, _)| p);
                provider
                    .save_entry(&access, &external_id, s, progress)
                    .await?;
            }
            _ => {}
        }

        let ok = sync::resolve_conflict(&self.pool, user_id, conflict_id, resolution).await?;
        let _ = sync::append_history(
            &self.pool,
            user_id,
            series_id,
            slug,
            "conflict_manual",
            &serde_json::json!({ "field": c.field, "resolution": resolution }),
        )
        .await;
        Ok(ok)
    }
}
