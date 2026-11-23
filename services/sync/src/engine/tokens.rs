//! Owns sealed OAuth token storage, the only place in this service holding the encryption key.
//! Expiry-driven refresh lives here too, since a refresh re-seals and re-persists tokens.

use secrecy::SecretString;
use time::OffsetDateTime;

use tankovault_auth::{AuthError, Sealer};
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

    /// Seals `tokens` and persists them for `user_id` at `slug`.
    pub(crate) async fn store(
        &self,
        slug: &str,
        user_id: UserId,
        tokens: &OAuthTokens,
    ) -> anyhow::Result<()> {
        let aad = token_aad(user_id, slug);
        let access_ct = self.secret.seal_string_bound(&tokens.access_token, &aad)?;
        let refresh_ct = tokens
            .refresh_token
            .as_ref()
            .map(|r| self.secret.seal_string_bound(r, &aad))
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

    /// Decrypts a usable access token for `user_id` at `provider`, refreshing it first if it has
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
        let aad = token_aad(user_id, slug);

        if let (Some(expiry), Some(refresh_ct)) =
            (account.expires_at, account.refresh_token.as_ref())
            && expiry <= OffsetDateTime::now_utc()
        {
            let (refresh, _) = open_token(&self.secret, refresh_ct, &aad)?;
            if let Ok(tokens) = provider.refresh(&refresh).await {
                self.store(slug, user_id, &tokens).await?;
                return Ok(tokens.access_token);
            }
        }

        // `Sealer` folds "wrong key", "wrong owner" and "not UTF-8" into one error on purpose;
        // see `open_string`. The plaintext never leaves a `SecretString` on the way out.
        let (access, form) = open_token(&self.secret, &account.access_token, &aad)?;
        // Both columns are written by one `store`, so the access token's form is the row's.
        if form == Form::Legacy
            && let Err(e) = self.reseal(&account, &access, &aad).await
        {
            tracing::warn!(error = %e, provider = slug, "could not re-seal a legacy tracker token");
        }
        Ok(access)
    }

    /// Rewrites a legacy row's tokens in the bound form, unless a concurrent write got there first.
    async fn reseal(
        &self,
        account: &sync::ExternalAccount,
        access: &SecretString,
        aad: &[u8],
    ) -> anyhow::Result<()> {
        let refresh = account
            .refresh_token
            .as_deref()
            .map(|ct| open_token(&self.secret, ct, aad).map(|(token, _)| token))
            .transpose()?;
        let access_ct = self.secret.seal_string_bound(access, aad)?;
        let refresh_ct = refresh
            .as_ref()
            .map(|r| self.secret.seal_string_bound(r, aad))
            .transpose()?;
        sync::reseal_tokens(
            &self.pool,
            account.user_id,
            &account.provider,
            &account.access_token,
            account.refresh_token.as_deref(),
            &access_ct,
            refresh_ct.as_deref(),
        )
        .await?;
        Ok(())
    }
}

/// Associated data binding a sealed token to its row: the user id's 16 bytes, then the slug.
///
/// The id is fixed-width, so no two `(user, provider)` pairs encode to the same bytes.
fn token_aad(user_id: UserId, slug: &str) -> Vec<u8> {
    let mut aad = Vec::with_capacity(16 + slug.len());
    aad.extend_from_slice(user_id.as_uuid().as_bytes());
    aad.extend_from_slice(slug.as_bytes());
    aad
}

/// Which sealing a stored token was opened from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Form {
    /// Bound to its `(user, provider)` by [`token_aad`].
    Bound,
    /// Sealed without associated data, before tokens were bound.
    Legacy,
}

/// Opens a stored token, bound form first.
///
/// Legacy acceptance window: rows sealed before binding still hold unbound ciphertext, which
/// someone with database write access could move into another user's row. Such a value is
/// accepted and re-sealed bound by [`TokenVault::access`] on first use; removing the fallback
/// once no legacy rows remain is a follow-up.
fn open_token(
    sealer: &Sealer,
    ciphertext: &[u8],
    aad: &[u8],
) -> Result<(SecretString, Form), AuthError> {
    if let Ok(token) = sealer.open_string_bound(ciphertext, aad) {
        return Ok((token, Form::Bound));
    }
    sealer
        .open_string(ciphertext)
        .map(|token| (token, Form::Legacy))
}

#[cfg(test)]
mod tests {
    use super::*;
    use secrecy::ExposeSecret as _;

    /// A fresh key per test: nothing here depends on the key's value.
    fn sealer() -> Sealer {
        let mut key = [0u8; 32];
        key[..16].copy_from_slice(uuid::Uuid::new_v4().as_bytes());
        key[16..].copy_from_slice(uuid::Uuid::new_v4().as_bytes());
        Sealer::new(&key)
    }

    /// The bug: tokens were sealed without associated data, so a database writer could copy
    /// one user's ciphertext into another user's row and the sync engine would use it.
    #[test]
    fn a_token_sealed_for_one_user_and_provider_opens_for_no_other() {
        let s = sealer();
        let (a, b) = (UserId::new(), UserId::new());
        let sealed = s
            .seal_string_bound(&SecretString::from("token"), &token_aad(a, "anilist"))
            .unwrap();

        let (token, form) = open_token(&s, &sealed, &token_aad(a, "anilist")).unwrap();
        assert_eq!(token.expose_secret(), "token");
        assert_eq!(form, Form::Bound);

        for aad in [token_aad(b, "anilist"), token_aad(a, "myanimelist")] {
            assert!(open_token(&s, &sealed, &aad).is_err());
        }
    }

    /// Rows sealed before binding must keep working until they are re-sealed.
    #[test]
    fn a_legacy_token_still_opens_and_is_reported_as_legacy() {
        let s = sealer();
        let legacy = s.seal_string(&SecretString::from("old-token")).unwrap();
        let (token, form) = open_token(&s, &legacy, &token_aad(UserId::new(), "anilist")).unwrap();
        assert_eq!(token.expose_secret(), "old-token");
        assert_eq!(form, Form::Legacy);
    }
}
