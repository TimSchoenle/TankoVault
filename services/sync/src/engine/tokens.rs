//! Sealed OAuth token storage — the only place in this service that holds the encryption key.
//!
//! Tokens are sealed with [`SecretBox`] before they reach the database and are opened only
//! here, so no other collaborator needs the key in its state. Expiry-driven refresh lives here
//! too, because a refresh writes new sealed tokens and would otherwise duplicate the sealing.

use anyhow::Context;
use time::OffsetDateTime;

use tankovault_auth::SecretBox;
use tankovault_db::PgPool;
use tankovault_db::repo::sync;
use tankovault_domain::UserId;

use crate::provider::{ExternalProvider, OAuthTokens};

/// Seals, stores and opens a user's provider tokens.
pub(crate) struct TokenVault {
    pool: PgPool,
    secret: SecretBox,
}

impl TokenVault {
    pub(crate) const fn new(pool: PgPool, secret: SecretBox) -> Self {
        Self { pool, secret }
    }

    /// Seal `tokens` and persist them for `user_id` at `slug`.
    pub(crate) async fn store(
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

    /// Decrypt a usable access token for `user_id` at `provider`, refreshing it first if it has
    /// expired and a refresh token is available.
    pub(crate) async fn access(
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
                    self.store(slug, user_id, &tokens).await?;
                    return Ok(tokens.access_token);
                }
            }
        }

        String::from_utf8(self.secret.open(&account.access_token)?)
            .context("decoded access token was not valid UTF-8")
    }
}
