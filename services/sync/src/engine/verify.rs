//! Checking a provider's record against the series it would describe, before enrichment trusts
//! a title search and when an operator re-verifies the mappings already stored.

use std::sync::Arc;

use tankovault_contracts::sync::{MappingReverifyFailure, MappingReverifyReason};
use tankovault_db::PgPool;
use tankovault_db::repo::{catalog, sync};
use tankovault_domain::{SeriesId, UserId};
use tankovault_matcher::titles_identify;
use uuid::Uuid;

use super::registry::ProviderRegistry;
use crate::provider::RemoteMetadata;

/// Mappings read per page; also how often progress is published and the claim renewed, so it
/// stays small enough that a page of rate-limited lookups finishes well inside the claim's
/// abandonment window.
const PAGE: i64 = 20;

/// Failures kept in the state row. The count is exact; the list is for review, and an operator
/// reading thousands of rows would sample them anyway.
const MAX_LISTED_FAILURES: usize = 1_000;

/// Whether one of `meta`'s own titles (not a synonym) names `series_id` by its title or one of
/// its sources' listing titles. `None` when the series no longer exists.
///
/// # Errors
/// Database failures.
pub(super) async fn record_identifies(
    pool: &PgPool,
    series_id: SeriesId,
    meta: &RemoteMetadata,
) -> anyhow::Result<Option<bool>> {
    Ok(catalog::series_identity_titles(pool, series_id)
        .await?
        .map(|identity| {
            titles_identify(
                &identity.normalized_title,
                &identity.listing_titles,
                &meta.titles[..meta.own_titles.min(meta.titles.len())],
            )
        }))
}

/// Re-fetches every stored mapping by id and refuses the ones that no longer verify.
pub(crate) struct MappingVerifier {
    pool: PgPool,
    registry: Arc<ProviderRegistry>,
}

impl MappingVerifier {
    pub(crate) const fn new(pool: PgPool, registry: Arc<ProviderRegistry>) -> Self {
        Self { pool, registry }
    }

    /// Claim the run and return its token. `None` when another run holds the claim.
    ///
    /// # Errors
    /// Database failures.
    pub(crate) async fn claim(&self, dry_run: bool) -> anyhow::Result<Option<Uuid>> {
        Ok(sync::begin_reverify(&self.pool, dry_run).await?)
    }

    /// The claimed run `run`, start to finish. Always releases the claim, unless another run
    /// has taken it over.
    pub(crate) async fn run(&self, run: Uuid, dry_run: bool, actor: Option<UserId>) {
        let mut counts = sync::ReverifyCounts::default();
        let mut failures = Vec::new();
        let outcome = self
            .walk(run, dry_run, actor, &mut counts, &mut failures)
            .await;
        let error = outcome.as_ref().err().map(ToString::to_string);
        let listed = serde_json::to_value(&failures).unwrap_or_default();
        match sync::finish_reverify(&self.pool, run, counts, &listed, error.as_deref()).await {
            Ok(true) => {}
            Ok(false) => {
                tracing::warn!("mapping re-verification claim was taken over; result dropped");
            }
            Err(e) => {
                tracing::error!(error = %e, "could not release the mapping re-verification claim");
            }
        }
        match outcome {
            Ok(()) => tracing::info!(
                checked = counts.checked,
                failed = counts.failed,
                removed = counts.removed,
                unreachable = counts.unreachable,
                dry_run,
                "mapping re-verification complete"
            ),
            Err(e) => tracing::warn!(error = %e, "mapping re-verification failed"),
        }
    }

    async fn walk(
        &self,
        run: Uuid,
        dry_run: bool,
        actor: Option<UserId>,
        counts: &mut sync::ReverifyCounts,
        failures: &mut Vec<MappingReverifyFailure>,
    ) -> anyhow::Result<()> {
        for (slug, provider) in self.registry.iter() {
            if !provider.supports_public_metadata() {
                continue;
            }
            let mut after = None;
            loop {
                let page = sync::list_mappings_after(&self.pool, slug, after, PAGE).await?;
                let Some((last, _)) = page.last() else {
                    break;
                };
                after = Some(*last);
                for (series_id, external_id) in page {
                    counts.checked = counts.checked.saturating_add(1);
                    let reason = match provider.fetch_public_metadata_by_id(&external_id).await {
                        // Left alone: a transport error or an id the provider no longer knows
                        // says nothing about whether the mapping is right.
                        Err(e) => {
                            counts.unreachable = counts.unreachable.saturating_add(1);
                            tracing::debug!(error = %e, %series_id, %external_id, "re-verification lookup failed");
                            continue;
                        }
                        Ok(None) => MappingReverifyReason::NotALibraryRecord,
                        Ok(Some(meta)) => {
                            match record_identifies(&self.pool, series_id, &meta).await? {
                                // Verified, or the series was deleted since the page was read
                                // and its mapping with it.
                                Some(true) | None => continue,
                                Some(false) => MappingReverifyReason::TitlesDiffer,
                            }
                        }
                    };
                    counts.failed = counts.failed.saturating_add(1);
                    if !dry_run {
                        // A block, not a bare delete: the cached remote entry would otherwise
                        // re-seed the mapping on the next reconciliation.
                        anyhow::ensure!(
                            sync::block_sync_match_for_reverify(
                                &self.pool,
                                run,
                                slug,
                                &external_id,
                                series_id,
                                actor,
                                reason_text(reason),
                            )
                            .await?,
                            "the claim was taken over by another run"
                        );
                        counts.removed = counts.removed.saturating_add(1);
                    }
                    if failures.len() < MAX_LISTED_FAILURES {
                        failures.push(MappingReverifyFailure {
                            series_id,
                            provider: (*slug).to_owned(),
                            external_id,
                            reason,
                        });
                    }
                }
                // A stalled run whose claim was retaken stops here rather than spend the rate
                // budget beside its successor.
                anyhow::ensure!(
                    sync::record_reverify_progress(&self.pool, run, *counts).await?,
                    "the claim was taken over by another run"
                );
            }
        }
        Ok(())
    }
}

/// The `sync_match_blocks.reason` a removal records.
const fn reason_text(reason: MappingReverifyReason) -> &'static str {
    match reason {
        MappingReverifyReason::NotALibraryRecord => {
            "re-verification: the record is not a library format"
        }
        MappingReverifyReason::TitlesDiffer => {
            "re-verification: the record's titles do not identify the series"
        }
    }
}
