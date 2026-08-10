//! Undoing one journalled sync decision, and refusing a title match permanently.
//!
//! This lives in the sync service rather than the API because half of it is a *provider* write:
//! taking back a push means putting the previous values on the remote, and the token that can do
//! that is sealed in this service's vault. The API forwards.

use std::sync::Arc;

use tankovault_db::PgPool;
use tankovault_db::repo::sync::{self, SyncDecisionRow};
use tankovault_db::repo::tracking;
use tankovault_domain::{SeriesId, UserId, WatchStatus};
use uuid::Uuid;

use super::registry::ProviderRegistry;
use super::tokens::TokenVault;

use tankovault_contracts::sync::{RestoredSide, RevertReport};

/// Reverts journalled decisions and records the refusal.
pub(crate) struct RevertService {
    pool: PgPool,
    registry: Arc<ProviderRegistry>,
    tokens: Arc<TokenVault>,
}

impl RevertService {
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

    /// A page of the journal.
    ///
    /// # Errors
    /// Database failures.
    pub(crate) async fn list(
        &self,
        filter: &sync::SyncDecisionFilter,
        limit: i64,
        offset: i64,
    ) -> anyhow::Result<Vec<SyncDecisionRow>> {
        Ok(sync::list_sync_decisions(&self.pool, filter, limit, offset).await?)
    }

    /// Undo one decision.
    ///
    /// # What each action's inverse is
    ///
    /// - **`pull`** — put the local value back. The common-ancestor snapshot is deliberately left
    ///   alone: with local moved away from it and the remote unchanged, the next reconciliation
    ///   sees "only local changed" and *pushes* the restored value, which is the operator's
    ///   intent. Resetting the snapshot instead would make the next run re-pull the value that
    ///   was just rejected.
    /// - **`import_status`** — the series was not on the watchlist before the sync put it there,
    ///   so the inverse is to take it off again.
    /// - **`push`** — write the remote's previous status and progress back through the provider.
    ///   This is the half that needs a token, and the reason this service owns the operation.
    /// - **`matched`** — a mapping is not a value to restore but a claim to withdraw, so the
    ///   inverse is the permanent refusal in [`Self::block_match`].
    ///
    /// # Errors
    ///
    /// A decision that changed nothing, was already reverted, or has no inverse — a
    /// `create_remote`, because no provider in this system can delete a remote entry — is an
    /// error naming which of those it was. Otherwise database and provider failures.
    pub(crate) async fn revert(
        &self,
        id: Uuid,
        actor: Option<UserId>,
        reason: &str,
    ) -> anyhow::Result<RevertReport> {
        let decision = sync::get_sync_decision(&self.pool, id).await?;
        if decision.reverted_at.is_some() {
            anyhow::bail!("this decision has already been reverted");
        }
        if !decision.applied {
            anyhow::bail!("this decision changed nothing, so there is nothing to undo");
        }

        let report = match decision.action.as_str() {
            "pull" => self.revert_pull(&decision).await?,
            "import_status" => self.revert_import(&decision).await?,
            "push" => self.revert_push(&decision).await?,
            "matched" => {
                let (external_id, series_id) = Self::match_target(&decision)?;
                sync::block_sync_match(
                    &self.pool,
                    &decision.provider,
                    &external_id,
                    series_id,
                    actor,
                    reason,
                )
                .await?;
                RevertReport {
                    decision_id: id,
                    restored: RestoredSide::Match,
                    value: None,
                    blocked_match: true,
                }
            }
            "create_remote" => anyhow::bail!(
                "a series created on the remote cannot be removed from here: no provider in this \
                 system exposes a delete. Remove it on {} and exclude the series from sync.",
                decision.provider,
            ),
            other => anyhow::bail!("a {other} decision has no inverse"),
        };

        sync::mark_sync_decision_reverted(&self.pool, id, actor, reason).await?;
        Ok(report)
    }

    /// Mark a decision wrong without undoing it, optionally refusing the match it made.
    ///
    /// Kept separate from [`Self::revert`] because the two answer different questions. A revert
    /// says "put it back"; a flag says "this was the wrong call", which is worth recording even
    /// when the value has since moved on and putting it back would be its own mistake.
    ///
    /// # Errors
    /// Database failures, or a `block_match` request against a decision that named no match.
    pub(crate) async fn flag(
        &self,
        id: Uuid,
        actor: Option<UserId>,
        reason: &str,
        block_match: bool,
    ) -> anyhow::Result<bool> {
        let decision = sync::get_sync_decision(&self.pool, id).await?;
        let flagged = sync::flag_sync_decision(&self.pool, id, actor, reason).await?;
        if block_match {
            let (external_id, series_id) = Self::match_target(&decision)?;
            sync::block_sync_match(
                &self.pool,
                &decision.provider,
                &external_id,
                series_id,
                actor,
                reason,
            )
            .await?;
        }
        Ok(flagged)
    }

    /// Refuse one (external id, series) correspondence permanently, whatever it scored.
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
        sync::block_sync_match(&self.pool, provider, external_id, series_id, actor, reason).await?;
        Ok(())
    }

    /// The (external id, series) pair a decision named, or an error saying it named none.
    fn match_target(decision: &SyncDecisionRow) -> anyhow::Result<(String, SeriesId)> {
        let external_id = decision
            .external_id
            .clone()
            .ok_or_else(|| anyhow::anyhow!("this decision names no remote entry to block"))?;
        let series_id = decision.series_id.ok_or_else(|| {
            anyhow::anyhow!("this decision matched no series, so nothing to block")
        })?;
        Ok((external_id, series_id))
    }

    async fn revert_pull(&self, decision: &SyncDecisionRow) -> anyhow::Result<RevertReport> {
        let series_id = decision
            .series_id
            .ok_or_else(|| anyhow::anyhow!("this decision names no series"))?;
        let before = decision
            .local_before
            .as_deref()
            .ok_or_else(|| anyhow::anyhow!("this decision recorded no previous local value"))?;
        let user_id = UserId::from_uuid(decision.user_id);

        match decision.scope.as_str() {
            "progress" => {
                let value: f64 = before.parse()?;
                tracking::progress_set(&self.pool, user_id, series_id, value).await?;
                Ok(RevertReport {
                    decision_id: decision.id,
                    restored: RestoredSide::LocalProgress,
                    value: Some(value.to_string()),
                    blocked_match: false,
                })
            }
            "status" => {
                let value: WatchStatus = before.parse()?;
                tracking::watchlist_set_status(&self.pool, user_id, series_id, value).await?;
                Ok(RevertReport {
                    decision_id: decision.id,
                    restored: RestoredSide::LocalStatus,
                    value: Some(value.as_str().to_owned()),
                    blocked_match: false,
                })
            }
            other => anyhow::bail!("a pull of {other} has no inverse"),
        }
    }

    async fn revert_import(&self, decision: &SyncDecisionRow) -> anyhow::Result<RevertReport> {
        let series_id = decision
            .series_id
            .ok_or_else(|| anyhow::anyhow!("this decision names no series"))?;
        let user_id = UserId::from_uuid(decision.user_id);
        // An import only ever happens for a series that was *not* on the watchlist, so removing
        // it is exact rather than approximate. The reader's progress is left alone: it is not
        // what the import wrote, and deleting a read frontier to undo a status import would be a
        // larger change than the one being reverted.
        tracking::watchlist_remove(&self.pool, user_id, series_id).await?;
        Ok(RevertReport {
            decision_id: decision.id,
            restored: RestoredSide::WatchlistEntry,
            value: None,
            blocked_match: false,
        })
    }

    async fn revert_push(&self, decision: &SyncDecisionRow) -> anyhow::Result<RevertReport> {
        let external_id = decision
            .external_id
            .clone()
            .ok_or_else(|| anyhow::anyhow!("this decision names no remote entry"))?;
        let progress = decision
            .evidence
            .get("remote_progress_before")
            .and_then(serde_json::Value::as_f64)
            .ok_or_else(|| {
                anyhow::anyhow!("this decision recorded no previous remote progress to restore")
            })?;
        let status: WatchStatus = decision
            .evidence
            .get("remote_status_before")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| {
                anyhow::anyhow!("this decision recorded no previous remote status to restore")
            })?
            .parse()?;

        let user_id = UserId::from_uuid(decision.user_id);
        let provider = self.registry.get(&decision.provider)?;
        let access = self
            .tokens
            .access(&decision.provider, provider, user_id)
            .await?;
        provider
            .save_entry(&access, &external_id, status, progress)
            .await?;

        // The snapshot said both sides agreed on the pushed value. They no longer do, and leaving
        // it would make the next run read the restored remote as a *remote* change and pull it
        // back onto the reader. Recording the restored pair as the agreement is what makes the
        // revert stick.
        if let Some(series_id) = decision.series_id {
            sync::record_snapshot(
                &self.pool,
                &sync::AgreedSnapshot {
                    series_id,
                    provider: &decision.provider,
                    local_progress: progress,
                    remote_progress: progress,
                    local_status: status.as_str(),
                    remote_status: status.as_str(),
                },
            )
            .await?;
        }

        Ok(RevertReport {
            decision_id: decision.id,
            restored: RestoredSide::RemoteEntry,
            value: Some(progress.to_string()),
            blocked_match: false,
        })
    }
}
