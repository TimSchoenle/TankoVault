//! The account lifecycle: OAuth linking, unlinking, the status card, and the per-account
//! automatic-sync settings including the effective conflict policy (design v2 §B.1/§B.6).

use std::sync::Arc;
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;

use tankovault_contracts::sync::{AccountSettings, AccountStatus};
use tankovault_db::PgPool;
use tankovault_db::repo::sync;
use tankovault_domain::UserId;

use super::registry::ProviderRegistry;
use super::tokens::TokenVault;
use crate::mapping::ConflictPolicy;

/// Links, unlinks and configures a user's provider accounts.
pub(crate) struct AccountService {
    pool: PgPool,
    registry: Arc<ProviderRegistry>,
    tokens: Arc<TokenVault>,
    /// The service-wide seed default, used only until an account has its own policy.
    default_policy: ConflictPolicy,
}

impl AccountService {
    pub(crate) const fn new(
        pool: PgPool,
        registry: Arc<ProviderRegistry>,
        tokens: Arc<TokenVault>,
        default_policy: ConflictPolicy,
    ) -> Self {
        Self {
            pool,
            registry,
            tokens,
            default_policy,
        }
    }

    /// The `provider`'s consent URL to redirect a user to.
    pub(crate) fn authorize_url(&self, slug: &str) -> anyhow::Result<String> {
        Ok(self.registry.get(slug)?.authorize_url())
    }

    /// Exchange an OAuth `code` and persist the (encrypted) tokens for `user_id`.
    pub(crate) async fn link(&self, slug: &str, user_id: UserId, code: &str) -> anyhow::Result<()> {
        let provider = self.registry.get(slug)?;
        let tokens = provider.exchange_code(code).await?;
        self.tokens.store(slug, user_id, &tokens).await?;
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

    /// Remove a user's link to `provider`. Returns `true` if an account was removed.
    pub(crate) async fn unlink(&self, slug: &str, user_id: UserId) -> anyhow::Result<bool> {
        self.registry.get(slug)?;
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
        self.registry.get(slug)?;
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

    /// The account's automatic-sync settings plus its pending-conflict count (design v2 §B.6).
    pub(crate) async fn settings(
        &self,
        slug: &str,
        user_id: UserId,
    ) -> anyhow::Result<AccountSettings> {
        self.registry.get(slug)?;
        let account = sync::get_account(&self.pool, user_id, slug).await?;
        let pending = sync::count_pending_conflicts(&self.pool, user_id).await?;
        Ok(match account {
            Some(a) => AccountSettings {
                linked: true,
                auto_sync_enabled: a.auto_sync_enabled,
                conflict_policy: Self::persisted_policy(&a.conflict_policy),
                pending_conflicts: pending,
            },
            None => AccountSettings {
                linked: false,
                auto_sync_enabled: false,
                conflict_policy: self.default_policy,
                pending_conflicts: pending,
            },
        })
    }

    /// The effective conflict policy for an account: an explicit override wins; otherwise the
    /// account's persisted policy; otherwise the service seed default (design v2 §B.1/§B.3).
    ///
    /// Lives here rather than in the reconciler because it *is* an account setting — and
    /// keeping it here is what stops `default_policy` from being duplicated into a second
    /// collaborator, where the two copies could be seeded from different values.
    pub(crate) async fn effective_policy(
        &self,
        slug: &str,
        user_id: UserId,
        override_policy: Option<ConflictPolicy>,
    ) -> ConflictPolicy {
        if let Some(p) = override_policy {
            return p;
        }
        match sync::get_account(&self.pool, user_id, slug).await {
            Ok(Some(a)) => Self::persisted_policy(&a.conflict_policy),
            _ => self.default_policy,
        }
    }

    /// A policy token read back out of the database.
    ///
    /// The column is `text`, so this is the one place a value can arrive that the type system
    /// did not vouch for — a row written before the vocabulary existed, or by hand. Falling
    /// back to the service default is deliberate: refusing to sync an account because one
    /// settings column is unreadable is worse than syncing it under the default policy. What
    /// is *not* deliberate is doing it silently, which is what the old `_ => NewestWins` parse
    /// arm did at every call site, so this logs.
    pub(crate) fn persisted_policy(token: &str) -> ConflictPolicy {
        token.parse().unwrap_or_else(|e| {
            tracing::warn!(error = %e, "unreadable persisted conflict policy; using the default");
            ConflictPolicy::default()
        })
    }

    /// Update the account's automatic-sync settings (design v2 §B.6).
    ///
    /// The policy arrives already parsed, so "an unknown token can never be persisted" is now
    /// a property of the type rather than a check this function performs — the request is
    /// rejected at the edge, by `serde`, before any handler runs.
    pub(crate) async fn update_settings(
        &self,
        slug: &str,
        user_id: UserId,
        auto_sync_enabled: Option<bool>,
        conflict_policy: Option<ConflictPolicy>,
    ) -> anyhow::Result<()> {
        self.registry.get(slug)?;
        sync::update_account_settings(
            &self.pool,
            user_id,
            slug,
            auto_sync_enabled,
            conflict_policy.map(ConflictPolicy::as_str),
        )
        .await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::{AccountService, ConflictPolicy};

    /// A token written by this service always reads back as itself. The persistence column is
    /// `text`, so nothing but this pairing keeps the write and the read in agreement.
    #[test]
    fn a_persisted_token_reads_back_as_the_policy_that_wrote_it() {
        for policy in ConflictPolicy::ALL {
            assert_eq!(
                AccountService::persisted_policy(policy.as_str()),
                policy,
                "`{policy}` does not survive a round trip through the settings column"
            );
        }
    }

    /// The one place a bad token is *tolerated* rather than refused, and the reason is that the
    /// alternative is worse: a row written before the vocabulary existed would otherwise make
    /// the account unsyncable rather than merely unconfigured.
    ///
    /// Note what this does **not** cover: the request path. An unknown policy in a `PATCH`
    /// body is now rejected by `serde` before `update_settings` is reached, so this fallback
    /// can no longer be a route by which a bad value is stored — which is what made the old
    /// `_ => NewestWins` parse arm a silent policy change rather than an error.
    #[test]
    fn an_unreadable_persisted_token_falls_back_to_the_default() {
        assert_eq!(
            AccountService::persisted_policy("newest-wins"),
            ConflictPolicy::default()
        );
        assert_eq!(
            AccountService::persisted_policy(""),
            ConflictPolicy::default()
        );
    }
}
