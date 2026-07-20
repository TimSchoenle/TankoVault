//! Sync engine: OAuth linking plus `AniList` ⇆ local pull/push (design §15).
//!
//! Tokens are sealed with [`SecretBox`] before persistence and only ever decrypted here.
//! Series are mapped to canonical works by reusing [`tankovault_matcher`] over trigram
//! candidates, then cached in `sync_mappings` so later syncs skip re-matching. Reconciling
//! progress across the two sides is delegated to the pure [`crate::mapping`] logic.

use std::collections::HashMap;

use anyhow::{Context, anyhow};
use serde::Serialize;
use time::OffsetDateTime;

use tankovault_auth::SecretBox;
use tankovault_db::PgPool;
use tankovault_db::repo::{catalog, matching, sync, tracking};
use tankovault_domain::{SeriesId, UserId, normalize_title};
use tankovault_matcher::{Candidate, Decision, Query, Thresholds, decide};

use crate::anilist::{AniListClient, OAuthTokens, PROVIDER, RemoteEntry};
use crate::mapping::{AniListStatus, ConflictPolicy, ProgressState, Side, reconcile_progress};

/// Outcome of a pull (`AniList` → local).
#[derive(Debug, Default, Serialize)]
pub(crate) struct PullReport {
    /// Entries returned by `AniList`.
    pub(crate) fetched: usize,
    /// Entries resolved to a canonical local series.
    pub(crate) matched: usize,
    /// Local progress rows written.
    pub(crate) updated: usize,
    /// Entries with no confident local match (skipped).
    pub(crate) unmatched: usize,
}

/// Outcome of a push (local → `AniList`).
#[derive(Debug, Default, Serialize)]
pub(crate) struct PushReport {
    /// Local watchlist entries examined.
    pub(crate) considered: usize,
    /// Remote entries created or updated.
    pub(crate) pushed: usize,
    /// Watchlist entries with no resolvable `AniList` media (skipped).
    pub(crate) unmapped: usize,
}

/// The stateful sync engine, shared behind an `Arc` in service state.
pub(crate) struct SyncEngine {
    pool: PgPool,
    client: AniListClient,
    secret: SecretBox,
    default_policy: ConflictPolicy,
    thresholds: Thresholds,
    candidate_limit: i64,
}

impl SyncEngine {
    pub(crate) fn new(
        pool: PgPool,
        client: AniListClient,
        secret: SecretBox,
        default_policy: ConflictPolicy,
    ) -> Self {
        Self {
            pool,
            client,
            secret,
            default_policy,
            thresholds: Thresholds::default(),
            candidate_limit: 10,
        }
    }

    /// The `AniList` consent URL to redirect a user to.
    #[must_use]
    pub(crate) fn authorize_url(&self) -> String {
        self.client.authorize_url()
    }

    /// Exchange an OAuth `code` and persist the (encrypted) tokens for `user_id`.
    pub(crate) async fn link(&self, user_id: UserId, code: &str) -> anyhow::Result<()> {
        let tokens = self.client.exchange_code(code).await?;
        self.store_tokens(user_id, &tokens).await
    }

    /// Remove a user's `AniList` link. Returns `true` if an account was removed.
    pub(crate) async fn unlink(&self, user_id: UserId) -> anyhow::Result<bool> {
        Ok(sync::delete_account(&self.pool, user_id, PROVIDER).await?)
    }

    async fn store_tokens(&self, user_id: UserId, tokens: &OAuthTokens) -> anyhow::Result<()> {
        let access_ct = self.secret.seal(tokens.access_token.as_bytes())?;
        let refresh_ct = tokens
            .refresh_token
            .as_ref()
            .map(|r| self.secret.seal(r.as_bytes()))
            .transpose()?;
        sync::upsert_account(
            &self.pool,
            user_id,
            PROVIDER,
            &access_ct,
            refresh_ct.as_deref(),
            tokens.expires_at,
        )
        .await?;
        Ok(())
    }

    /// Decrypt a usable access token for `user_id`, refreshing it first if expired and a
    /// refresh token is available.
    async fn access_token(&self, user_id: UserId) -> anyhow::Result<String> {
        let account = sync::get_account(&self.pool, user_id, PROVIDER)
            .await?
            .ok_or_else(|| anyhow!("no AniList account linked for user"))?;

        if let (Some(expiry), Some(refresh_ct)) =
            (account.expires_at, account.refresh_token.as_ref())
        {
            if expiry <= OffsetDateTime::now_utc() {
                let refresh = String::from_utf8(self.secret.open(refresh_ct)?)
                    .context("decoded refresh token was not valid UTF-8")?;
                if let Ok(tokens) = self.client.refresh(&refresh).await {
                    self.store_tokens(user_id, &tokens).await?;
                    return Ok(tokens.access_token);
                }
            }
        }

        String::from_utf8(self.secret.open(&account.access_token)?)
            .context("decoded access token was not valid UTF-8")
    }

    /// Pull the user's `AniList` list into the local watchlist/progress.
    pub(crate) async fn pull(
        &self,
        user_id: UserId,
        policy: Option<ConflictPolicy>,
    ) -> anyhow::Result<PullReport> {
        let policy = policy.unwrap_or(self.default_policy);
        let access = self.access_token(user_id).await?;
        let viewer = self.client.viewer_id(&access).await?;
        let entries = self.client.fetch_media_list(&access, viewer).await?;

        let mut report = PullReport {
            fetched: entries.len(),
            ..Default::default()
        };
        for entry in &entries {
            let Some(series_id) = self.resolve_series(entry).await? else {
                report.unmatched += 1;
                continue;
            };
            report.matched += 1;
            sync::upsert_mapping(&self.pool, series_id, PROVIDER, &entry.media_id.to_string())
                .await?;

            let local = self.local_state(user_id, series_id).await?;
            let remote = Some(ProgressState {
                progress: entry.progress,
                updated_at: entry.updated_at,
            });
            let rec = reconcile_progress(local, remote, policy);

            // Adopt the remote status only when the remote side is authoritative (this also
            // imports the entry onto the watchlist on first pull); `notify` is preserved.
            if rec.winner == Side::Remote {
                tracking::watchlist_set_status(
                    &self.pool,
                    user_id,
                    series_id,
                    entry.status.to_watch_status(),
                )
                .await?;
            }
            if rec.update_local {
                tracking::progress_set(&self.pool, user_id, series_id, rec.agreed_progress).await?;
                report.updated += 1;
            }
        }
        Ok(report)
    }

    /// Push the local watchlist/progress to `AniList`.
    pub(crate) async fn push(
        &self,
        user_id: UserId,
        policy: Option<ConflictPolicy>,
    ) -> anyhow::Result<PushReport> {
        let policy = policy.unwrap_or(self.default_policy);
        let access = self.access_token(user_id).await?;
        let viewer = self.client.viewer_id(&access).await?;
        let remote_entries = self.client.fetch_media_list(&access, viewer).await?;
        let remote_by_id: HashMap<i64, &RemoteEntry> =
            remote_entries.iter().map(|e| (e.media_id, e)).collect();

        let watchlist = tracking::watchlist_list(&self.pool, user_id).await?;
        let mut report = PushReport::default();
        for entry in &watchlist {
            report.considered += 1;
            let Some(media_id) = self.resolve_media_id(&access, entry.series_id).await? else {
                report.unmapped += 1;
                continue;
            };

            let remote = remote_by_id.get(&media_id).map(|e| ProgressState {
                progress: e.progress,
                updated_at: e.updated_at,
            });
            let local = self.local_state(user_id, entry.series_id).await?;
            let rec = reconcile_progress(local, remote, policy);

            if remote.is_none() || rec.update_remote {
                let status = AniListStatus::from_watch_status(entry.status);
                self.client
                    .save_entry(
                        &access,
                        media_id,
                        status,
                        progress_to_int(rec.agreed_progress),
                    )
                    .await?;
                report.pushed += 1;
            }
        }
        Ok(report)
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
    /// confident title match against the local catalogue.
    async fn resolve_series(&self, entry: &RemoteEntry) -> anyhow::Result<Option<SeriesId>> {
        if let Some(id) =
            sync::mapping_series_for_external(&self.pool, PROVIDER, &entry.media_id.to_string())
                .await?
        {
            return Ok(Some(id));
        }

        for title in &entry.titles {
            let normalized = normalize_title(title);
            if normalized.is_empty() {
                continue;
            }
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
                    })
                    .collect();
            let query = Query {
                normalized_title: normalized,
                content_type: entry.content_type,
                release_year: entry.start_year,
            };
            if let Decision::Attach(id) = decide(&query, &candidates, self.thresholds) {
                return Ok(Some(id));
            }
        }
        Ok(None)
    }

    /// Resolve a local series to an `AniList` media id: via an existing mapping, else by a
    /// title search (whose result is cached as a mapping).
    async fn resolve_media_id(
        &self,
        access: &str,
        series_id: SeriesId,
    ) -> anyhow::Result<Option<i64>> {
        if let Some(ext) =
            sync::mapping_external_for_series(&self.pool, series_id, PROVIDER).await?
        {
            if let Ok(id) = ext.parse::<i64>() {
                return Ok(Some(id));
            }
        }
        let series = catalog::get_series(&self.pool, series_id).await?;
        if let Some(id) = self
            .client
            .search_media(access, &series.canonical_title)
            .await?
        {
            sync::upsert_mapping(&self.pool, series_id, PROVIDER, &id.to_string()).await?;
            return Ok(Some(id));
        }
        Ok(None)
    }
}

/// Round a fractional local progress to the whole-chapter count `AniList` expects.
#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
fn progress_to_int(progress: f64) -> i64 {
    progress.max(0.0).round() as i64
}
