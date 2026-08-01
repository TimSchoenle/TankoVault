//! Sealed OAuth token storage — the only place in this service that holds the encryption key.
//!
//! Tokens are sealed with [`Sealer`] before they reach the database and are opened only
//! here, so no other collaborator needs the key in its state. Expiry-driven refresh lives here
//! too, because a refresh writes new sealed tokens and would otherwise duplicate the sealing.

use secrecy::SecretString;
use time::OffsetDateTime;

use tankovault_auth::Sealer;
use tankovault_db::PgPool;
use tankovault_db::repo::sync;
use tankovault_domain::UserId;

use crate::provider::{ExternalProvider, OAuthTokens};

/// Seals, stores and opens a user's provider tokens.
pub(crate) struct TokenVault {
    pool: PgPool,
    secret: Sealer,
}

impl TokenVault {
    pub(crate) const fn new(pool: PgPool, secret: Sealer) -> Self {
        Self { pool, secret }
    }

    /// Seal `tokens` and persist them for `user_id` at `slug`.
    pub(crate) async fn store(
        &self,
        slug: &str,
        user_id: UserId,
        tokens: &OAuthTokens,
    ) -> anyhow::Result<()> {
        let access_ct = self.secret.seal_string(&tokens.access_token)?;
        let refresh_ct = tokens
            .refresh_token
            .as_ref()
            .map(|r| self.secret.seal_string(r))
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
    ) -> anyhow::Result<SecretString> {
        let account = sync::get_account(&self.pool, user_id, slug)
            .await?
            .ok_or_else(|| {
                crate::error::SyncError::NotLinked(provider.display_name().to_owned())
            })?;

        if let (Some(expiry), Some(refresh_ct)) =
            (account.expires_at, account.refresh_token.as_ref())
            && expiry <= OffsetDateTime::now_utc()
        {
            let refresh = self.secret.open_string(refresh_ct)?;
            if let Ok(tokens) = provider.refresh(&refresh).await {
                self.store(slug, user_id, &tokens).await?;
                return Ok(tokens.access_token);
            }
        }

        // `open_string` folds "wrong key" and "not UTF-8" into one error on purpose; see
        // its doc comment. The plaintext never leaves a `SecretString` on the way out.
        Ok(self.secret.open_string(&account.access_token)?)
    }
}
