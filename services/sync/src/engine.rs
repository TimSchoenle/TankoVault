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

use tankovault_auth::SecretBox;
use tankovault_db::PgPool;
use tankovault_db::repo::{catalog, matching, sync, tracking};
use tankovault_domain::{SeriesId, UserId, WatchStatus, normalize_title};
use tankovault_matcher::{Candidate, Query, Thresholds, best_match};

use crate::mapping::{ConflictPolicy, ProgressState, Side, reconcile_progress};
use crate::provider::{ExternalProvider, OAuthTokens, ProviderInfo, RemoteEntry};

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

/// Whether a user has linked a provider, and (when linked) the connected display name and the
/// most recent sync time — the shape the "Sync & integrations" panel and status pill render.
#[derive(Debug, Default, Serialize)]
pub(crate) struct AccountStatus {
    pub(crate) linked: bool,
    pub(crate) username: Option<String>,
    #[serde(with = "time::serde::rfc3339::option")]
    pub(crate) last_synced_at: Option<OffsetDateTime>,
}

/// One provider's outcome from a targeted single-series push (design: immediate targeted push).
#[derive(Debug, Clone, Serialize)]
pub(crate) struct ProviderPushOutcome {
    pub(crate) provider: String,
    pub(crate) ok: bool,
    pub(crate) error: Option<String>,
}

/// The stateful sync engine, shared behind an `Arc` in service state. Holds every registered
/// provider (`AniList` today; a second provider is a drop-in registry entry).
pub(crate) struct SyncEngine {
    pool: PgPool,
    providers: HashMap<&'static str, Box<dyn ExternalProvider>>,
    secret: SecretBox,
    default_policy: ConflictPolicy,
    thresholds: Thresholds,
    candidate_limit: i64,
}

impl SyncEngine {
    pub(crate) fn new(
        pool: PgPool,
        secret: SecretBox,
        default_policy: ConflictPolicy,
        providers: HashMap<&'static str, Box<dyn ExternalProvider>>,
    ) -> Self {
        Self {
            pool,
            providers,
            secret,
            default_policy,
            thresholds: Thresholds::default(),
            candidate_limit: 10,
        }
    }

    fn provider(&self, slug: &str) -> anyhow::Result<&dyn ExternalProvider> {
        self.providers
            .get(slug)
            .map(Box::as_ref)
            .ok_or_else(|| anyhow!("unknown sync provider: {slug}"))
    }

    /// The registered providers, for `GET /v1/sync/providers`.
    #[must_use]
    pub(crate) fn registry(&self) -> Vec<ProviderInfo> {
        let mut list: Vec<_> = self
            .providers
            .values()
            .map(|p| ProviderInfo {
                slug: p.slug(),
                name: p.display_name(),
            })
            .collect();
        list.sort_by_key(|p| p.slug);
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
        Ok(())
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
                last_synced_at: a.last_synced_at,
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
            .ok_or_else(|| anyhow!("no {} account linked for user", provider.display_name()))?;

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

    /// Pull the user's `provider` list into the local watchlist/progress.
    pub(crate) async fn pull(
        &self,
        slug: &str,
        user_id: UserId,
        policy: Option<ConflictPolicy>,
    ) -> anyhow::Result<PullReport> {
        match self.pull_inner(slug, user_id, policy).await {
            Ok(report) => Ok(report),
            Err(e) => {
                let _ = sync::record_sync_error(&self.pool, user_id, slug, &e.to_string()).await;
                Err(e)
            }
        }
    }

    async fn pull_inner(
        &self,
        slug: &str,
        user_id: UserId,
        policy: Option<ConflictPolicy>,
    ) -> anyhow::Result<PullReport> {
        let provider = self.provider(slug)?;
        let policy = policy.unwrap_or(self.default_policy);
        let access = self.access_token(slug, provider, user_id).await?;
        let viewer = provider.viewer(&access).await?;
        let entries = provider.fetch_list(&access, &viewer).await?;

        let mut report = PullReport {
            fetched: entries.len(),
            ..Default::default()
        };
        for entry in &entries {
            let matched = self.resolve_series(slug, entry).await?;

            // Snapshot every fetched entry (matched or not) so the admin console can review
            // and hand-assign the ones the auto-matcher missed — the whole list is reconciled,
            // not just the confident matches.
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
                report.unmatched += 1;
                continue;
            };
            report.matched += 1;
            sync::upsert_mapping(&self.pool, series_id, slug, &entry.external_id).await?;

            let local = self.local_state(user_id, series_id).await?;
            let remote = Some(ProgressState {
                progress: entry.progress,
                updated_at: entry.updated_at,
            });
            let rec = reconcile_progress(local, remote, policy);

            // Adopt the remote status only when the remote side is authoritative (this also
            // imports the entry onto the watchlist on first pull); `notify` is preserved.
            if rec.winner == Side::Remote {
                tracking::watchlist_set_status(&self.pool, user_id, series_id, entry.status)
                    .await?;
            }
            if rec.update_local {
                tracking::progress_set(&self.pool, user_id, series_id, rec.agreed_progress).await?;
                report.updated += 1;
            }
        }
        sync::mark_synced(
            &self.pool,
            user_id,
            slug,
            Some(&viewer.name),
            OffsetDateTime::now_utc(),
        )
        .await?;
        Ok(report)
    }

    /// Push the local watchlist/progress to `provider`.
    pub(crate) async fn push(
        &self,
        slug: &str,
        user_id: UserId,
        policy: Option<ConflictPolicy>,
    ) -> anyhow::Result<PushReport> {
        match self.push_inner(slug, user_id, policy).await {
            Ok(report) => Ok(report),
            Err(e) => {
                let _ = sync::record_sync_error(&self.pool, user_id, slug, &e.to_string()).await;
                Err(e)
            }
        }
    }

    async fn push_inner(
        &self,
        slug: &str,
        user_id: UserId,
        policy: Option<ConflictPolicy>,
    ) -> anyhow::Result<PushReport> {
        let provider = self.provider(slug)?;
        let policy = policy.unwrap_or(self.default_policy);
        let access = self.access_token(slug, provider, user_id).await?;
        let viewer = provider.viewer(&access).await?;
        let remote_entries = provider.fetch_list(&access, &viewer).await?;
        let remote_by_id: HashMap<&str, &RemoteEntry> = remote_entries
            .iter()
            .map(|e| (e.external_id.as_str(), e))
            .collect();

        let watchlist = tracking::watchlist_list(&self.pool, user_id).await?;
        let mut report = PushReport::default();
        for entry in &watchlist {
            report.considered += 1;
            let Some(external_id) = self
                .resolve_media_id(provider, slug, &access, entry.series_id)
                .await?
            else {
                report.unmapped += 1;
                continue;
            };

            let remote = remote_by_id
                .get(external_id.as_str())
                .map(|e| ProgressState {
                    progress: e.progress,
                    updated_at: e.updated_at,
                });
            let local = self.local_state(user_id, entry.series_id).await?;
            let rec = reconcile_progress(local, remote, policy);

            if remote.is_none() || rec.update_remote {
                provider
                    .save_entry(&access, &external_id, entry.status, rec.agreed_progress)
                    .await?;
                report.pushed += 1;
            }
        }
        sync::mark_synced(
            &self.pool,
            user_id,
            slug,
            Some(&viewer.name),
            OffsetDateTime::now_utc(),
        )
        .await?;
        Ok(report)
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

    async fn local_state(
        &self,
        user_id: UserId,
        series_id: SeriesId,
    ) -> anyhow::Result<Option<ProgressState>> {
        Ok(tracking::progress_state(&self.pool, user_id, series_id)
            .await?
            .map(|(progress, updated_at)| ProgressState {
                progress,
                updated_at,
            }))
    }

    /// Resolve a remote entry to a canonical series: first via an existing mapping, then by
    /// the best confident title match against the local catalogue.
    ///
    /// Every candidate title (romaji/english/native, plus every AniList synonym) is scored
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
}
