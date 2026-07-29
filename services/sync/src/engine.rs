//! Sync engine: OAuth linking plus provider ⇆ local pull/push, a targeted single-series push,
//! and the multi-provider registry (design: generalized multi-provider sync).
//!
//! Tokens are sealed with [`SecretBox`] before persistence and only ever decrypted here.
//! Series are mapped to canonical works by reusing [`tankovault_matcher`] over trigram
//! candidates, then cached in `sync_mappings` so later syncs skip re-matching. Reconciling
//! progress across the two sides is delegated to the pure [`crate::mapping`] logic. Status
//! crosses the provider boundary as [`WatchStatus`] — provider-specific vocabularies (e.g.
//! `AniListStatus`) never leave their own provider module.

use std::collections::HashMap;

use anyhow::{Context, anyhow};
use serde::Serialize;
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;

use tankovault_auth::SecretBox;
use tankovault_config::{MetadataPriorityConfig, SOURCE_ADAPTER, SOURCE_ANILIST};
use tankovault_db::PgPool;
use tankovault_db::repo::catalog::{MetadataEnrichment, SeriesEnrichmentRow};
use tankovault_db::repo::{catalog, matching, sync, tracking};
use tankovault_domain::{SeriesId, UserId, WatchStatus, normalize_title};
use tankovault_matcher::{Candidate, Query, Thresholds, best_match};

use crate::mapping::{ConflictPolicy, MergeAction, Side, three_way};
use crate::provider::{ExternalProvider, OAuthTokens, RemoteEntry, RemoteMetadata};
use tankovault_contracts::sync::{AccountSettings, AccountStatus, ProviderInfo};

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

/// Aggregate counters accumulated over one full account reconciliation (design v2 §B.3/§B.4).
/// Both the manual `PullReport`/`PushReport` and the scheduled loop's logging are derived from
/// these.
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
    /// Series skipped because they are excluded from sync (§A.5).
    pub(crate) skipped: usize,
}

/// Outcome of a tokenless metadata-enrichment sweep (public API, no user token).
#[derive(Debug, Default, Serialize)]
pub(crate) struct EnrichReport {
    /// Series examined this sweep.
    pub(crate) scanned: usize,
    /// Series that received metadata from at least one provider.
    pub(crate) enriched: usize,
    /// Series no public provider could resolve.
    pub(crate) unresolved: usize,
}

// `AccountStatus`, `AccountSettings` and `ProviderInfo` used to be declared here, private to
// this service. They now live in `tankovault_contracts::sync` because `services/api` proxies
// these routes verbatim and needs the same types to describe them in its OpenAPI document —
// which is what gives the frontend a *generated* client for the sync surface instead of
// hand-written mirror structs that drift (see the module docs over there).

/// One provider's outcome from a targeted single-series push (design: immediate targeted push).
#[derive(Debug, Clone, Serialize)]
pub(crate) struct ProviderPushOutcome {
    pub(crate) provider: String,
    pub(crate) ok: bool,
    pub(crate) error: Option<String>,
}

/// Collapse a fetched remote list to at most one entry per `external_id`, keeping the most
/// recently updated occurrence.
///
/// A provider's list can legitimately contain the *same* remote work more than once — `AniList`,
/// for instance, occasionally returns duplicate `MediaList` rows for one media (a fresh entry
/// and a stale leftover) that carry divergent `progress`/`updatedAt`. Reconciling every
/// occurrence would let the older duplicate clobber the newer one, so the same series flip-flops
/// between two values on every run. Deduplicating here — freshest `updated_at` wins — makes each
/// remote work reconcile exactly once, against its latest known state.
fn dedupe_latest_by_external_id(entries: Vec<RemoteEntry>) -> Vec<RemoteEntry> {
    let mut by_id: HashMap<String, RemoteEntry> = HashMap::with_capacity(entries.len());
    for entry in entries {
        match by_id.get(&entry.external_id) {
            Some(existing) if existing.updated_at >= entry.updated_at => {}
            _ => {
                by_id.insert(entry.external_id.clone(), entry);
            }
        }
    }
    by_id.into_values().collect()
}

/// The stateful sync engine, shared behind an `Arc` in service state. Holds every registered
/// provider (`AniList` today; a second provider is a drop-in registry entry).
pub(crate) struct SyncEngine {
    pool: PgPool,
    providers: HashMap<&'static str, Box<dyn ExternalProvider>>,
    secret: SecretBox,
    default_policy: ConflictPolicy,
    /// Which source has the final say per metadata field (default: `AniList` over adapters).
    metadata_priority: MetadataPriorityConfig,
    thresholds: Thresholds,
    candidate_limit: i64,
}

impl SyncEngine {
    pub(crate) fn new(
        pool: PgPool,
        secret: SecretBox,
        default_policy: ConflictPolicy,
        metadata_priority: MetadataPriorityConfig,
        providers: HashMap<&'static str, Box<dyn ExternalProvider>>,
    ) -> Self {
        Self {
            pool,
            providers,
            secret,
            default_policy,
            metadata_priority,
            thresholds: Thresholds::default(),
            candidate_limit: 10,
        }
    }

    fn provider(&self, slug: &str) -> anyhow::Result<&dyn ExternalProvider> {
        self.providers.get(slug).map(Box::as_ref).ok_or_else(|| {
            anyhow::Error::new(crate::error::SyncError::UnknownProvider(slug.to_owned()))
        })
    }

    /// The registered providers, for `GET /v1/sync/providers`.
    #[must_use]
    pub(crate) fn registry(&self) -> Vec<ProviderInfo> {
        let mut list: Vec<_> = self
            .providers
            .values()
            .map(|p| ProviderInfo {
                slug: p.slug().to_owned(),
                name: p.display_name().to_owned(),
            })
            .collect();
        list.sort_by(|a, b| a.slug.cmp(&b.slug));
        list
    }

    /// The `provider`'s consent URL to redirect a user to.
    pub(crate) fn authorize_url(&self, slug: &str) -> anyhow::Result<String> {
        Ok(self.provider(slug)?.authorize_url())
    }

    /// Exchange an OAuth `code` and persist the (encrypted) tokens for `user_id`.
    pub(crate) async fn link(&self, slug: &str, user_id: UserId, code: &str) -> anyhow::Result<()> {
        let provider = self.provider(slug)?;
        let tokens = provider.exchange_code(code).await?;
        self.store_tokens(slug, user_id, &tokens).await?;
        // Best-effort: capture the display name for the status card. A lookup failure must
        // not fail the link itself — the tokens are already safely stored.
        let username = provider
            .viewer(&tokens.access_token)
            .await
            .ok()
            .map(|v| v.name);
        sync::mark_synced(
            &self.pool,
            user_id,
            slug,
            username.as_deref(),
            OffsetDateTime::now_utc(),
        )
        .await?;
        // Seed the per-account conflict policy from the service default the first time (design
        // v2 §B.1: the env default is only the seed, never a live control thereafter).
        sync::seed_account_policy(&self.pool, user_id, slug, self.default_policy.as_str()).await?;
        Ok(())
    }

    /// The account's automatic-sync settings plus its pending-conflict count (design v2 §B.6).
    pub(crate) async fn settings(
        &self,
        slug: &str,
        user_id: UserId,
    ) -> anyhow::Result<AccountSettings> {
        self.provider(slug)?;
        let account = sync::get_account(&self.pool, user_id, slug).await?;
        let pending = sync::count_pending_conflicts(&self.pool, user_id).await?;
        Ok(match account {
            Some(a) => AccountSettings {
                linked: true,
                auto_sync_enabled: a.auto_sync_enabled,
                conflict_policy: a.conflict_policy,
                pending_conflicts: pending,
            },
            None => AccountSettings {
                linked: false,
                auto_sync_enabled: false,
                conflict_policy: self.default_policy.as_str().to_owned(),
                pending_conflicts: pending,
            },
        })
    }

    /// Update the account's automatic-sync settings (design v2 §B.6). An unknown policy token
    /// is rejected so a bad value can never be persisted.
    pub(crate) async fn update_settings(
        &self,
        slug: &str,
        user_id: UserId,
        auto_sync_enabled: Option<bool>,
        conflict_policy: Option<&str>,
    ) -> anyhow::Result<()> {
        self.provider(slug)?;
        if let Some(p) = conflict_policy {
            if ConflictPolicy::parse(p).as_str() != p {
                return Err(anyhow!("unknown conflict policy: {p}"));
            }
        }
        sync::update_account_settings(
            &self.pool,
            user_id,
            slug,
            auto_sync_enabled,
            conflict_policy,
        )
        .await?;
        Ok(())
    }

    /// A user's pending conflicts across all providers (design v2 §B.6).
    pub(crate) async fn list_conflicts(
        &self,
        user_id: UserId,
    ) -> anyhow::Result<Vec<sync::ConflictRow>> {
        Ok(sync::list_pending_conflicts(&self.pool, user_id).await?)
    }

    /// A page of a user's sync history (design v2 §B.6).
    pub(crate) async fn history(
        &self,
        user_id: UserId,
        series_id: Option<SeriesId>,
        provider: Option<&str>,
        page: i64,
    ) -> anyhow::Result<Vec<sync::HistoryRow>> {
        let limit = 50;
        let offset = page.max(0) * limit;
        Ok(sync::list_history(&self.pool, user_id, series_id, provider, limit, offset).await?)
    }

    /// Apply a user's manual conflict resolution (design v2 §B.6): write the chosen side, then
    /// mark the conflict resolved and refresh that field's snapshot so it is not re-detected.
    /// Returns `false` if the conflict does not exist / is already resolved.
    pub(crate) async fn resolve_conflict(
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
        let provider = self.provider(slug)?;
        let external_id = sync::mapping_external_for_series(&self.pool, series_id, slug)
            .await?
            .ok_or_else(|| anyhow!("series is no longer mapped for {slug}"))?;
        let access = self.access_token(slug, provider, user_id).await?;

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

    /// Remove a user's link to `provider`. Returns `true` if an account was removed.
    pub(crate) async fn unlink(&self, slug: &str, user_id: UserId) -> anyhow::Result<bool> {
        self.provider(slug)?;
        Ok(sync::delete_account(&self.pool, user_id, slug).await?)
    }

    /// Whether `user_id` has a linked `provider` account, plus its display name and most
    /// recent sync time — read straight from storage, never from the live API, so a page
    /// load never spends the provider's rate-limit budget.
    pub(crate) async fn status(
        &self,
        slug: &str,
        user_id: UserId,
    ) -> anyhow::Result<AccountStatus> {
        self.provider(slug)?;
        let account = sync::get_account(&self.pool, user_id, slug).await?;
        Ok(match account {
            Some(a) => AccountStatus {
                linked: true,
                username: a.external_username,
                // The contract carries RFC-3339 text rather than a `time` type: it is shared
                // with a wasm frontend that deliberately pulls in no date crate.
                last_synced_at: a.last_synced_at.and_then(|ts| ts.format(&Rfc3339).ok()),
            },
            None => AccountStatus::default(),
        })
    }

    async fn store_tokens(
        &self,
        slug: &str,
        user_id: UserId,
        tokens: &OAuthTokens,
    ) -> anyhow::Result<()> {
        let access_ct = self.secret.seal(tokens.access_token.as_bytes())?;
        let refresh_ct = tokens
            .refresh_token
            .as_ref()
            .map(|r| self.secret.seal(r.as_bytes()))
            .transpose()?;
        sync::upsert_account(
            &self.pool,
            user_id,
            slug,
            &access_ct,
            refresh_ct.as_deref(),
            tokens.expires_at,
        )
        .await?;
        Ok(())
    }

    /// Decrypt a usable access token for `user_id` at `provider`, refreshing it first if
    /// expired and a refresh token is available.
    async fn access_token(
        &self,
        slug: &str,
        provider: &dyn ExternalProvider,
        user_id: UserId,
    ) -> anyhow::Result<String> {
        let account = sync::get_account(&self.pool, user_id, slug)
            .await?
            .ok_or_else(|| {
                crate::error::SyncError::NotLinked(provider.display_name().to_owned())
            })?;

        if let (Some(expiry), Some(refresh_ct)) =
            (account.expires_at, account.refresh_token.as_ref())
        {
            if expiry <= OffsetDateTime::now_utc() {
                let refresh = String::from_utf8(self.secret.open(refresh_ct)?)
                    .context("decoded refresh token was not valid UTF-8")?;
                if let Ok(tokens) = provider.refresh(&refresh).await {
                    self.store_tokens(slug, user_id, &tokens).await?;
                    return Ok(tokens.access_token);
                }
            }
        }

        String::from_utf8(self.secret.open(&account.access_token)?)
            .context("decoded access token was not valid UTF-8")
    }

    /// Manual "pull" (design v2 §B.6): runs the full three-way reconciliation and reports it
    /// in the historical `PullReport` shape (`pull` and `push` now do the same reconcile).
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

    /// Manual "push" (design v2 §B.6): identical full reconciliation, reported in the
    /// historical `PushReport` shape.
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

    /// The effective conflict policy for an account: an explicit override wins; otherwise the
    /// account's persisted policy; otherwise the service seed default (design v2 §B.1/§B.3).
    async fn effective_policy(
        &self,
        slug: &str,
        user_id: UserId,
        override_policy: Option<ConflictPolicy>,
    ) -> ConflictPolicy {
        if let Some(p) = override_policy {
            return p;
        }
        match sync::get_account(&self.pool, user_id, slug).await {
            Ok(Some(a)) => ConflictPolicy::parse(&a.conflict_policy),
            _ => self.default_policy,
        }
    }

    /// Full three-way reconciliation of a linked account (design v2 §B.3/§B.4): every remote
    /// entry is matched + reconciled, then every mapped local watchlist entry not seen on the
    /// remote is created there. Excluded series are skipped; `AskMe` conflicts are queued.
    async fn reconcile_account(
        &self,
        slug: &str,
        user_id: UserId,
        override_policy: Option<ConflictPolicy>,
    ) -> anyhow::Result<ReconcileCounts> {
        let provider = self.provider(slug)?;
        let policy = self.effective_policy(slug, user_id, override_policy).await;
        let access = self.access_token(slug, provider, user_id).await?;
        let viewer = provider.viewer(&access).await?;
        // Collapse duplicate remote rows (same `external_id`) to their freshest occurrence
        // before reconciling — a provider list can carry the same work twice with divergent
        // progress, and processing both would let a stale duplicate clobber the fresh one.
        let entries = dedupe_latest_by_external_id(provider.fetch_list(&access, &viewer).await?);

        let mut counts = ReconcileCounts {
            fetched: entries.len(),
            ..Default::default()
        };
        let mut handled_ids: std::collections::HashSet<String> = std::collections::HashSet::new();
        // Series already reconciled this run. Two *distinct* remote ids can still resolve to one
        // local series (an ambiguous title match, or genuine duplicate remote works); reconciling
        // it more than once would replay the same clobbering flip-flop, so each series is
        // reconciled at most once per run.
        let mut handled_series: std::collections::HashSet<SeriesId> =
            std::collections::HashSet::new();

        // Remote-driven pass: reconcile every remote entry against its local match.
        for entry in &entries {
            let matched = self.resolve_series(slug, entry).await?;
            let title = entry.titles.first().map_or("", String::as_str);
            sync::upsert_remote_entry(
                &self.pool,
                user_id,
                slug,
                &entry.external_id,
                title,
                entry.status.as_str(),
                entry.progress,
                entry.content_type.as_str(),
                entry.start_year,
                entry.updated_at,
                matched,
            )
            .await?;

            let Some(series_id) = matched else {
                counts.unmatched += 1;
                continue;
            };
            counts.matched += 1;
            sync::upsert_mapping(&self.pool, series_id, slug, &entry.external_id).await?;
            handled_ids.insert(entry.external_id.clone());
            if !handled_series.insert(series_id) {
                continue; // this series was already reconciled against a duplicate remote row
            }
            self.reconcile_series(
                provider,
                slug,
                &access,
                user_id,
                series_id,
                &entry.external_id,
                Some(entry),
                policy,
                &mut counts,
            )
            .await?;
        }

        // Local-driven pass: watchlist entries that map to a remote id not present in the
        // fetched list need creating on the remote side.
        let watchlist = tracking::watchlist_list(&self.pool, user_id).await?;
        for wl in &watchlist {
            counts.considered += 1;
            let Some(external_id) = self
                .resolve_media_id(provider, slug, &access, wl.series_id)
                .await?
            else {
                counts.unmapped += 1;
                continue;
            };
            if handled_ids.contains(&external_id) {
                continue; // already reconciled in the remote-driven pass
            }
            self.reconcile_series(
                provider,
                slug,
                &access,
                user_id,
                wl.series_id,
                &external_id,
                None,
                policy,
                &mut counts,
            )
            .await?;
        }

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

    /// Reconcile one mapped series against the remote, per the three-way merge (design v2
    /// §B.3). `remote` is `None` when the series is not present on the remote yet (it must be
    /// created there). Excluded series (§A.5) are skipped entirely.
    // The parallel `snap_lp`/`snap_rp`/`snap_ls`/`snap_rs` bindings and the length are
    // inherent to the three-way merge; splitting it would obscure the §B.3 logic.
    #[allow(
        clippy::too_many_arguments,
        clippy::too_many_lines,
        clippy::similar_names
    )]
    async fn reconcile_series(
        &self,
        provider: &dyn ExternalProvider,
        slug: &str,
        access: &str,
        user_id: UserId,
        series_id: SeriesId,
        external_id: &str,
        remote: Option<&RemoteEntry>,
        policy: ConflictPolicy,
        counts: &mut ReconcileCounts,
    ) -> anyhow::Result<()> {
        if tracking::is_sync_excluded(&self.pool, user_id, series_id, slug).await? {
            counts.skipped += 1;
            return Ok(());
        }

        let local_state = tracking::progress_state(&self.pool, user_id, series_id).await?;
        let local_progress = local_state.map_or(0.0, |(p, _)| p);
        let local_updated = local_state.map_or(OffsetDateTime::UNIX_EPOCH, |(_, u)| u);
        let local_status_opt =
            tracking::watchlist_status_get(&self.pool, user_id, series_id).await?;

        // No remote entry yet: create it outright (local is authoritative for a first push).
        let Some(remote) = remote else {
            let status = local_status_opt.unwrap_or(WatchStatus::Reading);
            provider
                .save_entry(access, external_id, status, local_progress)
                .await?;
            sync::record_snapshot(
                &self.pool,
                series_id,
                slug,
                local_progress,
                local_progress,
                status.as_str(),
                status.as_str(),
            )
            .await?;
            counts.pushed += 1;
            let _ = sync::append_history(
                &self.pool,
                user_id,
                series_id,
                slug,
                "push",
                &serde_json::json!({ "created": true, "progress": local_progress }),
            )
            .await;
            return Ok(());
        };

        let snap = sync::get_snapshot(&self.pool, series_id, slug).await?;
        let (snap_lp, snap_rp, snap_ls, snap_rs) = match &snap {
            Some(s) => (
                s.last_synced_local_progress,
                s.last_synced_remote_progress,
                s.last_synced_local_status
                    .as_deref()
                    .and_then(|t| t.parse::<WatchStatus>().ok()),
                s.last_synced_remote_status
                    .as_deref()
                    .and_then(|t| t.parse::<WatchStatus>().ok()),
            ),
            None => (None, None, None, None),
        };

        // The side whose own last-modified time is later, for `NewestWins`.
        let newer = if local_updated >= remote.updated_at {
            Side::Local
        } else {
            Side::Remote
        };

        // An absent local watchlist entry is imported from the remote first, so status merges
        // are meaningful; treat its current status as agreeing with the remote.
        let imported = local_status_opt.is_none();
        if imported {
            tracking::watchlist_set_status(&self.pool, user_id, series_id, remote.status).await?;
        }
        let local_status = local_status_opt.unwrap_or(remote.status);

        let pd = three_way(
            local_progress,
            remote.progress,
            snap_lp,
            snap_rp,
            policy,
            newer,
        );
        let sd = three_way(local_status, remote.status, snap_ls, snap_rs, policy, newer);

        let mut conflict = false;
        // --- progress ---
        match pd.action {
            MergeAction::PullRemote => {
                tracking::progress_set(&self.pool, user_id, series_id, remote.progress).await?;
                counts.pulled += 1;
                let _ = sync::append_history(
                    &self.pool,
                    user_id,
                    series_id,
                    slug,
                    "pull",
                    &serde_json::json!({
                        "field": "progress", "from": local_progress, "to": remote.progress,
                        "policy": policy.as_str()
                    }),
                )
                .await;
            }
            MergeAction::Conflict => {
                conflict = true;
                sync::insert_conflict(
                    &self.pool,
                    user_id,
                    series_id,
                    slug,
                    "progress",
                    &local_progress.to_string(),
                    &remote.progress.to_string(),
                )
                .await?;
                counts.conflicts += 1;
                let _ = sync::append_history(
                    &self.pool,
                    user_id,
                    series_id,
                    slug,
                    "conflict_auto",
                    &serde_json::json!({ "field": "progress",
                        "local": local_progress, "remote": remote.progress }),
                )
                .await;
            }
            MergeAction::PushLocal | MergeAction::Noop => {}
        }
        // --- status ---
        match sd.action {
            MergeAction::PullRemote if !imported => {
                tracking::watchlist_set_status(&self.pool, user_id, series_id, remote.status)
                    .await?;
                counts.pulled += 1;
            }
            MergeAction::Conflict => {
                conflict = true;
                sync::insert_conflict(
                    &self.pool,
                    user_id,
                    series_id,
                    slug,
                    "status",
                    local_status.as_str(),
                    remote.status.as_str(),
                )
                .await?;
                counts.conflicts += 1;
            }
            _ => {}
        }

        // A single remote write covers both fields if either wants to push local.
        let push_needed =
            pd.action == MergeAction::PushLocal || sd.action == MergeAction::PushLocal;
        if push_needed {
            let status_for_remote = match sd.action {
                MergeAction::PushLocal | MergeAction::Noop => local_status,
                _ => remote.status,
            };
            let progress_for_remote = match pd.action {
                MergeAction::PushLocal | MergeAction::Noop => local_progress,
                _ => remote.progress,
            };
            provider
                .save_entry(access, external_id, status_for_remote, progress_for_remote)
                .await?;
            counts.pushed += 1;
            let _ = sync::append_history(
                &self.pool,
                user_id,
                series_id,
                slug,
                "push",
                &serde_json::json!({ "progress": progress_for_remote,
                    "status": status_for_remote.as_str(), "policy": policy.as_str() }),
            )
            .await;
        }

        // Refresh the common-ancestor snapshot only when nothing was left in conflict, so a
        // pending `AskMe` conflict is re-detected on the next run until resolved.
        if !conflict {
            let agreed_progress = match pd.action {
                MergeAction::PullRemote => remote.progress,
                _ => local_progress,
            };
            let agreed_status = match sd.action {
                MergeAction::PullRemote => remote.status,
                _ => local_status,
            };
            sync::record_snapshot(
                &self.pool,
                series_id,
                slug,
                agreed_progress,
                agreed_progress,
                agreed_status.as_str(),
                agreed_status.as_str(),
            )
            .await?;
        }
        Ok(())
    }

    /// Scheduled reconciliation of every account with automatic sync enabled (design v2 §B.4).
    /// Best-effort: a failure on one account is logged and does not abort the tick.
    pub(crate) async fn reconcile_all_accounts(&self) {
        let accounts = match sync::list_auto_sync_accounts(&self.pool).await {
            Ok(a) => a,
            Err(e) => {
                tracing::warn!(error = %e, "scheduled reconciliation could not list accounts");
                return;
            }
        };
        for (user_id, slug) in accounts {
            if !self.providers.contains_key(slug.as_str()) {
                continue;
            }
            if let Err(e) = self.reconcile_account_guarded(&slug, user_id, None).await {
                tracing::warn!(error = %e, provider = %slug, %user_id,
                    "scheduled reconciliation failed for account");
            }
        }
    }

    /// Targeted single-series push (design: immediate targeted push): fans out to every
    /// provider `user_id` has linked. Fast path — no full remote-list fetch/reconciliation;
    /// local state wins outright since this is a direct, deliberate user action (e.g. marking
    /// a chapter read), not a bulk reconciliation. Never fails the caller; every outcome
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
            let Some(provider) = self.providers.get(slug.as_str()).map(Box::as_ref) else {
                continue;
            };
            outcomes.push(
                self.push_series_one(provider, &slug, user_id, series_id)
                    .await,
            );
        }
        outcomes
    }

    async fn push_series_one(
        &self,
        provider: &dyn ExternalProvider,
        slug: &str,
        user_id: UserId,
        series_id: SeriesId,
    ) -> ProviderPushOutcome {
        match self
            .push_series_inner(provider, slug, user_id, series_id)
            .await
        {
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

    async fn push_series_inner(
        &self,
        provider: &dyn ExternalProvider,
        slug: &str,
        user_id: UserId,
        series_id: SeriesId,
    ) -> anyhow::Result<()> {
        // Reactive push is now gated on the same two switches every sync path respects
        // (design v2 §B.4): automatic sync must be enabled for the account, and the series
        // must not be excluded (§A.5). Neither check existed before.
        match sync::get_account(&self.pool, user_id, slug).await? {
            Some(a) if a.auto_sync_enabled => {}
            _ => return Ok(()),
        }
        if tracking::is_sync_excluded(&self.pool, user_id, series_id, slug).await? {
            return Ok(());
        }

        let access = self.access_token(slug, provider, user_id).await?;
        let external_id = self
            .resolve_media_id(provider, slug, &access, series_id)
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

    /// Resolve a remote entry to a canonical series: first via an existing mapping, then by
    /// the best confident title match against the local catalogue.
    ///
    /// Every candidate title (romaji/english/native, plus every `AniList` synonym) is scored
    /// against its own trigram candidates and the **global** best is taken, so an entry
    /// attaches when *any* of its titles matches confidently — not just the first one tried.
    /// Synonym lists routinely duplicate the official titles or each other once normalized
    /// (case, punctuation, a "manga"/"webtoon" suffix), so titles are deduplicated by their
    /// normalized form first — one DB round trip per distinct key, not per raw string.
    async fn resolve_series(
        &self,
        slug: &str,
        entry: &RemoteEntry,
    ) -> anyhow::Result<Option<SeriesId>> {
        if let Some(id) =
            sync::mapping_series_for_external(&self.pool, slug, &entry.external_id).await?
        {
            return Ok(Some(id));
        }

        let mut seen = std::collections::HashSet::with_capacity(entry.titles.len());
        let normalized_titles: Vec<String> = entry
            .titles
            .iter()
            .map(|title| normalize_title(title))
            .filter(|normalized| !normalized.is_empty() && seen.insert(normalized.clone()))
            .collect();

        let mut best: Option<(SeriesId, f32)> = None;
        for normalized in normalized_titles {
            let candidates: Vec<Candidate> =
                matching::find_candidates(&self.pool, &normalized, self.candidate_limit)
                    .await?
                    .into_iter()
                    .map(|c| Candidate {
                        series_id: c.series_id,
                        normalized_title: c.normalized_title,
                        similarity: c.similarity,
                        content_type: c.content_type,
                        release_year: c.release_year,
                        tags: c.tags,
                        authors: c.authors,
                    })
                    .collect();
            // AniList's own genres/staff, matched against each candidate's locally-scraped
            // tags/authors — the extra signal that makes ambiguous title matches confident.
            let query = Query {
                normalized_title: normalized,
                content_type: entry.content_type,
                release_year: entry.start_year,
                tags: entry.tags.clone(),
                authors: entry.authors.clone(),
            };
            if let Some((id, score)) = best_match(&query, &candidates) {
                if best.is_none_or(|(_, b)| score > b) {
                    best = Some((id, score));
                }
            }
        }

        Ok(best
            .filter(|(_, score)| *score >= self.thresholds.high)
            .map(|(id, _)| id))
    }

    /// Resolve a local series to a `provider` external id: via an existing mapping, else by a
    /// title search (whose result is cached as a mapping).
    async fn resolve_media_id(
        &self,
        provider: &dyn ExternalProvider,
        slug: &str,
        access: &str,
        series_id: SeriesId,
    ) -> anyhow::Result<Option<String>> {
        if let Some(ext) = sync::mapping_external_for_series(&self.pool, series_id, slug).await? {
            return Ok(Some(ext));
        }
        let series = catalog::get_series(&self.pool, series_id).await?;
        if let Some(id) = provider.search(access, &series.canonical_title).await? {
            sync::upsert_mapping(&self.pool, series_id, slug, &id).await?;
            return Ok(Some(id));
        }
        Ok(None)
    }

    /// Tokenless metadata-enrichment sweep (design: worker queue syncing every existing
    /// entry to `AniList` **without** a stored user token).
    ///
    /// Walks the catalogue in batches of `batch_size`, up to `max_series` series, and for
    /// each one asks every provider that exposes a public API (`AniList`'s unauthenticated
    /// GraphQL) for its catalogue metadata — by an already-cached external id where one
    /// exists, else by canonical title. Resolved metadata is folded in under the
    /// configured per-field priority (default: `AniList` wins over the scraped adapter data),
    /// and every alternative title/synonym is persisted for merge detection and search.
    /// Never fails the whole sweep on a single series' error — those are logged and skipped.
    pub(crate) async fn enrich_all(
        &self,
        batch_size: i64,
        max_series: usize,
    ) -> anyhow::Result<EnrichReport> {
        let mut report = EnrichReport::default();
        if !self
            .providers
            .values()
            .any(|p| p.supports_public_metadata())
        {
            return Ok(report);
        }
        // A keyset walk, not `OFFSET`. Enrichment writes `updated_at = now()`, which is the
        // very column the sweep ordered by — so with `OFFSET` every enriched row jumped to
        // the end of the ordering, the rows behind it shifted forward, and the next page's
        // offset skipped exactly those. The sweep silently missed series.
        //
        // `started_at` fences the run: a row this sweep has already touched now sorts after
        // it *and* fails `updated_at < started_at`, so it cannot come back around.
        let started_at = OffsetDateTime::now_utc();
        let mut cursor: Option<(OffsetDateTime, uuid::Uuid)> = None;
        while report.scanned < max_series {
            let rows =
                catalog::list_series_for_enrichment(&self.pool, batch_size, cursor, started_at)
                    .await?;
            if rows.is_empty() {
                break;
            }
            let fetched = rows.len();
            for row in rows {
                if report.scanned >= max_series {
                    break;
                }
                // Advanced before the work, not after: an enrichment that fails must still
                // move the cursor, or a permanently-failing row stalls the sweep forever.
                cursor = Some((row.updated_at, row.id.as_uuid()));
                report.scanned += 1;
                match self.enrich_series(&row).await {
                    Ok(true) => report.enriched += 1,
                    Ok(false) => report.unresolved += 1,
                    Err(e) => {
                        report.unresolved += 1;
                        tracing::warn!(error = %e, series_id = %row.id, "series enrichment failed");
                    }
                }
            }
            if i64::try_from(fetched).unwrap_or(0) < batch_size {
                break;
            }
        }
        tracing::info!(
            scanned = report.scanned,
            enriched = report.enriched,
            unresolved = report.unresolved,
            "tokenless metadata enrichment sweep complete"
        );
        Ok(report)
    }

    /// Enrich one series from the first public provider that resolves it. Returns whether any
    /// provider supplied metadata.
    async fn enrich_series(&self, row: &SeriesEnrichmentRow) -> anyhow::Result<bool> {
        for (slug, provider) in &self.providers {
            if !provider.supports_public_metadata() {
                continue;
            }
            let existing = sync::mapping_external_for_series(&self.pool, row.id, slug).await?;
            let meta = match existing {
                Some(ext) => provider.fetch_public_metadata_by_id(&ext).await?,
                None => {
                    provider
                        .fetch_public_metadata_by_title(&row.canonical_title)
                        .await?
                }
            };
            let Some(meta) = meta else {
                continue;
            };
            self.apply_metadata(row, slug, &meta).await?;
            return Ok(true);
        }
        Ok(false)
    }

    /// Persist resolved public metadata for `row`: cache the external-id mapping, fold in the
    /// priority-resolved description/cover, and record every alternative title/synonym, genre
    /// and credit.
    async fn apply_metadata(
        &self,
        row: &SeriesEnrichmentRow,
        slug: &str,
        meta: &RemoteMetadata,
    ) -> anyhow::Result<()> {
        sync::upsert_mapping(&self.pool, row.id, slug, &meta.external_id).await?;

        // Description/cover follow the configured priority: the AniList value versus the
        // value the scraping adapters already stored on the row.
        let description = self.metadata_priority.resolve(
            "description",
            &[
                (SOURCE_ANILIST, meta.description.clone()),
                (SOURCE_ADAPTER, row.description.clone()),
            ],
        );
        let cover = self.metadata_priority.resolve(
            "cover",
            &[
                (SOURCE_ANILIST, meta.cover_url.clone()),
                (SOURCE_ADAPTER, row.cover_url.clone()),
            ],
        );

        // Every alternative title AniList tracks (english/native/synonyms), normalized for
        // the trigram/merge/search indexes; blanks and duplicates are dropped downstream.
        let alt_titles: Vec<(String, String)> = meta
            .titles
            .iter()
            .map(|t| (t.clone(), normalize_title(t)))
            .filter(|(_, n)| !n.is_empty())
            .collect();

        // Content-type and release year are additive gap-fills (never overwrite a value the
        // adapters already determined), so the AniList value only lands where local data is
        // missing — no priority resolution needed.
        let content_type = match meta.content_type {
            tankovault_domain::ContentType::Unknown => None,
            other => Some(other.as_str()),
        };

        catalog::apply_enrichment(
            &self.pool,
            row.id,
            &MetadataEnrichment {
                description: description.as_deref(),
                cover_url: cover.as_deref(),
                content_type,
                release_year: meta.start_year,
                alt_titles: &alt_titles,
                tags: &meta.tags,
                authors: &meta.authors,
            },
        )
        .await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    // Progress values under test are small, exactly-representable integers, so exact float
    // comparison is correct here.
    #![allow(clippy::float_cmp)]

    use super::dedupe_latest_by_external_id;
    use crate::provider::RemoteEntry;
    use tankovault_domain::{ContentType, WatchStatus};
    use time::OffsetDateTime;

    fn entry(external_id: &str, progress: f64, updated_unix: i64) -> RemoteEntry {
        RemoteEntry {
            external_id: external_id.to_owned(),
            titles: vec![format!("title-{external_id}")],
            status: WatchStatus::Reading,
            progress,
            updated_at: OffsetDateTime::from_unix_timestamp(updated_unix).unwrap(),
            start_year: None,
            content_type: ContentType::Unknown,
            tags: Vec::new(),
            authors: Vec::new(),
        }
    }

    #[test]
    fn dedupe_keeps_freshest_occurrence_per_external_id() {
        // The AniList-observed anomaly: media 143056 returned twice, a stale 2022 row at
        // progress 23 and a fresh 2026 row at progress 182. Only the fresh one must survive.
        let stale = entry("143056", 23.0, 1_654_724_356); // 2022-06-08
        let fresh = entry("143056", 182.0, 1_784_801_432); // 2026-...
        let other = entry("129918", 182.0, 1_750_314_982);

        // Order must not matter: the newest `updated_at` wins regardless of input position.
        for input in [
            vec![fresh.clone(), stale.clone(), other.clone()],
            vec![stale.clone(), fresh.clone(), other.clone()],
        ] {
            let mut out = dedupe_latest_by_external_id(input);
            out.sort_by(|a, b| a.external_id.cmp(&b.external_id));

            assert_eq!(out.len(), 2, "one row per distinct external id");
            let dup = out.iter().find(|e| e.external_id == "143056").unwrap();
            assert_eq!(dup.progress, 182.0, "freshest occurrence must win");
            let single = out.iter().find(|e| e.external_id == "129918").unwrap();
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
