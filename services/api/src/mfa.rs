//! Second-factor verification, shared by the two places that need it.
//!
//! The sign-in second leg (`crate::auth::mfa`) and the step-up prompt (`crate::me::mfa`) verify
//! the *same* factors under the *same* rules. They live in different modules because they are
//! different flows to a reader — "how do I sign in" and "why is it asking again" — but the
//! verification itself is here, once, because two copies of "is this TOTP code valid" is two
//! places for the replay guard to be forgotten.
//!
//! # Passkeys are not a factor here, and that is deliberate
//!
//! `POST /v1/auth/passkey/login/*` stays a **single-leg** sign-in. A passkey is already
//! phishing-resistant and user-verified — the authenticator checked a PIN or a biometric before
//! releasing the signature — so demanding a code after it is theatre that teaches users to treat
//! the strongest credential as the weakest. Only the password path grows a second leg. A future
//! reader tempted to "fix" this asymmetry should read that sentence again; a test in
//! `services/api/tests/mfa.rs` pins it.

use secrecy::{ExposeSecret as _, SecretSlice, SecretString};
use serde::{Deserialize, Serialize};
use tankovault_db::repo::users::mfa::StepUpMethod;
use tankovault_db::repo::users::webauthn::{CeremonyKind, CredentialPurpose};
use tankovault_domain::UserId;
use time::OffsetDateTime;
use utoipa::ToSchema;
use uuid::Uuid;
use webauthn_rs::prelude::{PublicKeyCredential, SecurityKey, SecurityKeyAuthentication};

use crate::error::{ApiError, ApiResult};
use crate::passkey::{begin_ceremony, relying_party, take_ceremony, verification_failed};
use crate::state::AppState;

/// A `WebAuthn` assertion challenge for the caller's security keys, plus the handle that
/// completes it.
#[derive(Debug, Serialize, ToSchema)]
pub struct SecurityKeyChallenge {
    pub ceremony_id: Uuid,
    /// A W3C `PublicKeyCredentialRequestOptions` envelope. Hand it to the browser unmodified.
    #[schema(value_type = Object)]
    pub options: serde_json::Value,
}

/// The assertion `navigator.credentials.get()` produced.
#[derive(Debug, Deserialize, ToSchema)]
pub struct SecurityKeyAssertion {
    pub ceremony_id: Uuid,
    /// The `PublicKeyCredential` the browser produced, serialised as the `WebAuthn`
    /// specification defines it.
    #[schema(value_type = Object)]
    pub credential: serde_json::Value,
}

/// Open a user's TOTP secret.
///
/// The one place the sealed column becomes key material. Returns `Ok(None)` when there is no
/// *confirmed* enrolment — an unconfirmed row is a secret the user was shown and may never have
/// stored, and treating it as a live factor would lock them out of their own account.
///
/// # Errors
/// [`ApiError::Unavailable`] when this deployment configured no `auth.mfa_encryption_key`;
/// [`ApiError::Internal`] when the stored ciphertext will not open, which means the key was
/// rotated under a live enrolment and is an operator problem, not a caller one.
async fn open_confirmed_secret(
    state: &AppState,
    user_id: UserId,
) -> ApiResult<Option<(SecretSlice<u8>, Option<i64>)>> {
    let sealer = sealer(state)?;
    let Some(enrolment) = tankovault_db::repo::users::mfa::get_totp(&state.pool, user_id).await?
    else {
        return Ok(None);
    };
    if enrolment.confirmed_at.is_none() {
        return Ok(None);
    }

    let secret = sealer.open(&enrolment.secret).map_err(|e| {
        tracing::error!(
            error = %e,
            user_id = %user_id.as_uuid(),
            "a stored TOTP secret will not open; auth.mfa_encryption_key has changed under a \
             live enrolment. Restore the previous key, or clear user_totp and have affected \
             accounts enrol again."
        );
        ApiError::Internal
    })?;
    Ok(Some((secret, enrolment.last_step)))
}

/// The configured sealer, or the `503` an unconfigured deployment owes.
///
/// # Errors
/// [`ApiError::Unavailable`] when `auth.mfa_encryption_key` is unset.
pub(crate) fn sealer(state: &AppState) -> ApiResult<&tankovault_auth::Sealer> {
    state.mfa_sealer.as_ref().ok_or_else(|| {
        tracing::warn!(
            "an authenticator-app (TOTP) operation was requested but auth.mfa_encryption_key is \
             not configured; set TANKOVAULT_AUTH__MFA_ENCRYPTION_KEY to a base64-encoded 32-byte \
             key"
        );
        ApiError::Unavailable
    })
}

/// Verify a TOTP code against a user's confirmed enrolment, advancing the replay floor.
///
/// Returns `false` for a wrong, malformed or replayed code, and for a user with no confirmed
/// enrolment — all of which the caller answers identically.
///
/// The replay floor is advanced with a conditional `UPDATE`, and a lost race is treated as a
/// **failure**: two requests carrying the same code reach this together, both verify against the
/// floor they read, and exactly one wins the write. Returning `true` to the loser would make the
/// code reusable for precisely as long as the concurrency lasted, which is the window an
/// automated replay operates in.
///
/// # Errors
/// [`ApiError::Unavailable`] with no sealing key; [`ApiError::Internal`] on an unopenable
/// secret or a database failure.
pub(crate) async fn verify_totp(
    state: &AppState,
    user_id: UserId,
    code: &SecretString,
) -> ApiResult<bool> {
    let Some((secret, last_step)) = open_confirmed_secret(state, user_id).await? else {
        return Ok(false);
    };
    let Some(step) =
        tankovault_auth::totp::verify(&secret, code, OffsetDateTime::now_utc(), last_step)
    else {
        return Ok(false);
    };
    let advanced =
        tankovault_db::repo::users::mfa::advance_totp_step(&state.pool, user_id, step).await?;
    Ok(advanced > 0)
}

/// Consume one of a user's recovery codes.
///
/// # Errors
/// [`ApiError::Internal`] on a database failure.
pub(crate) async fn verify_recovery_code(
    state: &AppState,
    user_id: UserId,
    code: &SecretString,
) -> ApiResult<bool> {
    let hash = tankovault_auth::recovery::hash_code(code);
    Ok(tankovault_db::repo::users::mfa::consume_recovery_code(&state.pool, user_id, &hash).await?)
}

/// Issue a fresh recovery-code set, returning the plaintext for its single display.
///
/// Replaces any previous set. The plaintext exists only in this return value — there is no path
/// back from the stored digests, which is what makes "we cannot recover it for you, generate a
/// new set" an honest answer rather than a policy.
///
/// # Errors
/// [`ApiError::Internal`] on a database failure.
pub(crate) async fn issue_recovery_codes(
    state: &AppState,
    user_id: UserId,
) -> ApiResult<Vec<SecretString>> {
    let codes = tankovault_auth::recovery::generate_codes();
    let hashes: Vec<String> = codes
        .iter()
        .map(tankovault_auth::recovery::hash_code)
        .collect();

    let mut conn = state.pool.acquire().await.map_err(|e| {
        tracing::error!(error = %e, "could not acquire a connection to store recovery codes");
        ApiError::Internal
    })?;
    tankovault_db::repo::users::mfa::replace_recovery_codes(&mut conn, user_id, &hashes).await?;
    Ok(codes)
}

/// Begin a security-key assertion for `user_id`.
///
/// # Errors
/// [`ApiError::Unavailable`] with no relying party; [`ApiError::BadRequest`] when the account
/// holds no security key, since there is nothing to assert; [`ApiError::Internal`] on a stored
/// credential that no longer deserialises.
pub(crate) async fn begin_security_key_assertion(
    state: &AppState,
    user_id: UserId,
) -> ApiResult<SecurityKeyChallenge> {
    let webauthn = relying_party(state)?;
    let rows = tankovault_db::repo::users::webauthn::list_for_user(
        &state.pool,
        user_id,
        CredentialPurpose::SecurityKey,
    )
    .await?;
    if rows.is_empty() {
        return Err(ApiError::BadRequest(
            "no security key is registered to this account".to_owned(),
        ));
    }

    let keys: Vec<SecurityKey> = rows
        .into_iter()
        .map(|r| serde_json::from_value(r.credential))
        .collect::<Result<_, _>>()
        .map_err(|e| {
            tracing::error!(
                error = %e,
                user_id = %user_id.as_uuid(),
                "a stored security key no longer deserialises; the webauthn-rs version moved \
                 under it"
            );
            ApiError::Internal
        })?;

    let (challenge, ceremony) = webauthn
        .start_securitykey_authentication(&keys)
        .map_err(|e| crate::passkey::ceremony_start_failed(&e))?;

    let ceremony_id = begin_ceremony(
        state,
        Some(user_id),
        CeremonyKind::AuthenticateSecurityKey,
        &ceremony,
    )
    .await?;

    Ok(SecurityKeyChallenge {
        ceremony_id,
        options: serde_json::to_value(&challenge).map_err(|e| {
            tracing::error!(error = %e, "could not serialise a security-key challenge");
            ApiError::Internal
        })?,
    })
}

/// Complete a security-key assertion for `user_id`.
///
/// Three things must agree, and each is checked because any one of them alone can be
/// attacker-chosen: the ceremony must have been started by this user, the signature must verify
/// against the challenge that ceremony carried, and the asserted credential must be a
/// **security key** belonging to this user. Without the last of those, a caller could assert
/// their own passkey — a first-factor credential they already used to sign in — and have it
/// counted as the second factor.
///
/// # Errors
/// [`ApiError::Unauthorized`] for every failure, collapsed so the failure mode discloses
/// nothing; see `crate::passkey::verification_failed`.
pub(crate) async fn finish_security_key_assertion(
    state: &AppState,
    user_id: UserId,
    assertion: &SecurityKeyAssertion,
) -> ApiResult<()> {
    let webauthn = relying_party(state)?;
    let credential: PublicKeyCredential = serde_json::from_value(assertion.credential.clone())
        .map_err(|e| {
            tracing::debug!(error = %e, "malformed security-key assertion");
            ApiError::Unauthorized
        })?;

    let (ceremony_user, ceremony): (_, SecurityKeyAuthentication) = take_ceremony(
        state,
        assertion.ceremony_id,
        CeremonyKind::AuthenticateSecurityKey,
    )
    .await?;
    if ceremony_user != Some(user_id) {
        return Err(ApiError::Unauthorized);
    }

    let result = webauthn
        .finish_securitykey_authentication(&credential, &ceremony)
        .map_err(|e| verification_failed(&e))?;

    let record = tankovault_db::repo::users::webauthn::find_by_credential_id(
        &state.pool,
        result.cred_id().as_ref(),
        CredentialPurpose::SecurityKey,
    )
    .await?
    .ok_or(ApiError::Unauthorized)?;
    if record.user_id != user_id {
        return Err(ApiError::Unauthorized);
    }

    // The signature counter is this credential class's clone detection, so persisting the
    // advance is not bookkeeping — a counter that never moves is how a cloned key stays
    // undetected.
    if result.needs_update() {
        let mut key: SecurityKey = serde_json::from_value(record.credential.clone())
            .map_err(|_| ApiError::Unauthorized)?;
        key.update_credential(&result);
        if let Ok(updated) = serde_json::to_value(&key) {
            if let Err(e) = tankovault_db::repo::users::webauthn::record_use(
                &state.pool,
                &record.credential_id,
                &updated,
            )
            .await
            {
                tracing::warn!(error = %e, "failed to record security-key use");
            }
            return Ok(());
        }
    }
    if let Err(e) = tankovault_db::repo::users::webauthn::record_use(
        &state.pool,
        &record.credential_id,
        &record.credential,
    )
    .await
    {
        tracing::warn!(error = %e, "failed to record security-key use");
    }
    Ok(())
}

/// A step-up elevation, handed to the client once.
#[derive(Debug, Serialize, ToSchema)]
pub struct StepUpGrant {
    /// Present this in the `X-Step-Up` header on the sensitive request.
    // Unwrapped exactly once, by `expose_onto_wire`, at the boundary handing it to the client —
    // the same treatment `TokenResponse::access_token` gets, and for the same reason.
    #[serde(serialize_with = "crate::secret::expose_onto_wire")]
    #[schema(value_type = String)]
    pub token: SecretString,
    /// Seconds until the elevation lapses and the next sensitive action prompts again.
    pub expires_in: i64,
}

/// Mint a step-up grant for `user_id`, recording which factor earned it.
///
/// # Errors
/// [`ApiError::Internal`] on a database failure.
pub(crate) async fn issue_step_up(
    state: &AppState,
    user_id: UserId,
    method: StepUpMethod,
) -> ApiResult<StepUpGrant> {
    let token = tankovault_auth::generate_handle();
    tankovault_db::repo::users::mfa::insert_step_up(
        &state.pool,
        user_id,
        &tankovault_auth::hash_handle(&token),
        method,
        OffsetDateTime::now_utc() + state.step_up_ttl,
    )
    .await?;
    Ok(StepUpGrant {
        token,
        expires_in: state.step_up_ttl.whole_seconds(),
    })
}

/// Whether `user_id` currently holds any second factor.
///
/// # Errors
/// [`ApiError::Internal`] on a database failure.
pub(crate) async fn is_enrolled(state: &AppState, user_id: UserId) -> ApiResult<bool> {
    Ok(tankovault_db::repo::users::mfa::is_enrolled(&state.pool, user_id).await?)
}

/// Expose the TOTP secret opener to the enrolment flow, which must verify a code against an
/// **unconfirmed** row — the one case [`verify_totp`] deliberately refuses.
///
/// # Errors
/// As [`sealer`], plus [`ApiError::Internal`] on an unopenable secret.
pub(crate) fn open_secret(
    state: &AppState,
    sealed: &[u8],
    user_id: UserId,
) -> ApiResult<SecretSlice<u8>> {
    sealer(state)?.open(sealed).map_err(|e| {
        tracing::error!(
            error = %e,
            user_id = %user_id.as_uuid(),
            "a stored TOTP secret will not open; auth.mfa_encryption_key has changed"
        );
        ApiError::Internal
    })
}

/// Seal a freshly generated TOTP secret for storage.
///
/// # Errors
/// As [`sealer`], plus [`ApiError::Internal`] if the AEAD provider fails.
pub(crate) fn seal_secret(state: &AppState, secret: &SecretSlice<u8>) -> ApiResult<Vec<u8>> {
    sealer(state)?.seal(secret.expose_secret()).map_err(|e| {
        tracing::error!(error = %e, "could not seal a TOTP secret");
        ApiError::Internal
    })
}
