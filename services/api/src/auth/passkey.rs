//! Passwordless sign-in with a passkey.
//!
//! Two legs. `start` issues a challenge to nobody in particular; `finish` receives a signed
//! assertion, learns *from it* which account is signing in, verifies the signature against the
//! challenge it issued, and mints the same session [`login`](super::login::login) would.
//!
//! # Why the challenge names no account
//!
//! This is a **discoverable** (also "resident", also "usernameless") credential flow. The
//! server does not ask who you are before challenging you, because asking would reintroduce
//! the thing passkeys remove: an identifier the user has to remember and an oracle an attacker
//! can enumerate. `super::login` has to spend an argon2 hash on every unknown identifier to
//! avoid disclosing which accounts exist (SEC-10); this flow has no such branch to hide,
//! because the request contains no identifier at all.
//!
//! The account arrives in the response instead: the authenticator returns the **user handle**
//! it stored at registration alongside the credential id. Both are attacker-controllable
//! claims on their own, which is why [`passkey_login_finish`] treats them as claims — it
//! resolves the credential from the database, checks the handle agrees with the row's owner,
//! and only then hands the assertion to the verifier that checks the signature.
//!
//! # What a successful assertion actually proves
//!
//! That the holder possessed a private key bound to this origin, and that the authenticator
//! performed **user verification** (a PIN, a fingerprint, a face) — the relying party is built
//! with `UserVerificationPolicy::Required`, so a passkey sign-in is two factors in one gesture
//! rather than a password replacement that trades security for convenience.

use axum::Json;
use axum::extract::State;
use axum_extra::extract::cookie::CookieJar;
use serde::{Deserialize, Serialize};
use tankovault_domain::Feature;
use tankovault_service::AuditOutcome;
use utoipa::ToSchema;
use uuid::Uuid;
use webauthn_rs::prelude::{
    DiscoverableAuthentication, DiscoverableKey, Passkey, PublicKeyCredential,
};

use super::login::TokenResponse;
use super::session::issue_session;
use crate::audit::audit_anonymous;
use crate::error::{ApiError, ApiResult};
use crate::openapi::AUTH_TAG;
use crate::passkey::{begin_ceremony, relying_party, take_ceremony, verification_failed};
use crate::state::{AppState, ClientContext};
use tankovault_db::repo::users::passkeys::CeremonyKind;

/// The challenge, plus the handle the client echoes back to complete it.
///
/// `options` is a W3C `PublicKeyCredentialRequestOptions` envelope, passed to
/// `navigator.credentials.get()` verbatim. It is typed as an opaque JSON document here on
/// purpose: the shape is the `WebAuthn` specification's, not this API's, and mirroring it into
/// the `OpenAPI` document would be this service publishing a second, hand-maintained copy of a
/// standard — the exact drift that made `/v1/me/sync/*` move its DTOs into
/// `crates/contracts`. Both ends parse it with the *same* crate (`webauthn-rs-proto`, pinned to
/// one version in both workspaces), so there is nothing for the two to disagree about.
#[derive(Debug, Serialize, ToSchema)]
pub struct PasskeyChallenge {
    /// Opaque handle for this ceremony. Return it with the assertion.
    pub ceremony_id: Uuid,
    /// A W3C `PublicKeyCredentialRequestOptions` envelope. Hand it to the browser unmodified.
    #[schema(value_type = Object)]
    pub options: serde_json::Value,
}

/// A signed assertion, returned with the handle from [`PasskeyChallenge`].
#[derive(Debug, Deserialize, ToSchema)]
pub struct PasskeyLoginRequest {
    pub ceremony_id: Uuid,
    /// The `PublicKeyCredential` produced by `navigator.credentials.get()`, serialised as the
    /// `WebAuthn` specification defines it.
    #[schema(value_type = Object)]
    pub credential: serde_json::Value,
}

/// Begin a passkey sign-in
///
/// Issues a discoverable-credential challenge. Deliberately unauthenticated and deliberately
/// identifier-free: the account is learned from the response, not asked for here.
///
/// The response is safe to hand to any caller. It contains a random challenge and no
/// information about which accounts exist — mint one for a nonexistent user and it is
/// indistinguishable from any other, because no user was named.
#[utoipa::path(
    post,
    path = "/v1/auth/passkey/login/start",
    tag = AUTH_TAG,
    responses(
        (status = 200, description = "Challenge issued", body = PasskeyChallenge),
        (status = 429, description = "Too many attempts; retry later", body = crate::error::ProblemDetails),
        (status = 503, description = "This deployment has no WebAuthn relying party configured", body = crate::error::ProblemDetails),
    )
)]
pub async fn passkey_login_start(
    State(state): State<AppState>,
) -> ApiResult<Json<PasskeyChallenge>> {
    let webauthn = relying_party(&state)?;

    let (challenge, ceremony) = webauthn
        .start_discoverable_authentication()
        .map_err(|e| crate::passkey::ceremony_start_failed(&e))?;

    let ceremony_id = begin_ceremony(&state, None, CeremonyKind::Authenticate, &ceremony).await?;

    Ok(Json(PasskeyChallenge {
        ceremony_id,
        options: serde_json::to_value(&challenge).map_err(|e| {
            tracing::error!(error = %e, "could not serialise a webauthn challenge");
            ApiError::Internal
        })?,
    }))
}

/// Complete a passkey sign-in
///
/// Verifies the assertion against the challenge issued by
/// [`passkey_login_start`](passkey_login_start) and, on success, issues an access token and a
/// rotating refresh cookie — the same session a password sign-in produces.
///
/// Every outcome is audited under `auth.passkey_login`, matching
/// [`login`](super::login::login): an authentication log that only records successes cannot
/// answer the question anyone asks it after an incident.
#[utoipa::path(
    post,
    path = "/v1/auth/passkey/login/finish",
    tag = AUTH_TAG,
    request_body = PasskeyLoginRequest,
    responses(
        (status = 200, description = "Authenticated; access token issued", body = TokenResponse),
        (status = 401, description = "The assertion did not verify, or the challenge is no longer live", body = crate::error::ProblemDetails),
        (status = 403, description = "The account is suspended, or its email is not yet confirmed", body = crate::error::ProblemDetails),
        (status = 429, description = "Too many attempts; retry later", body = crate::error::ProblemDetails),
        (status = 503, description = "This deployment has no WebAuthn relying party configured", body = crate::error::ProblemDetails),
    )
)]
pub async fn passkey_login_finish(
    State(state): State<AppState>,
    client: ClientContext,
    jar: CookieJar,
    Json(body): Json<PasskeyLoginRequest>,
) -> ApiResult<(CookieJar, Json<TokenResponse>)> {
    let webauthn = relying_party(&state)?;

    let credential: PublicKeyCredential = serde_json::from_value(body.credential)
        .map_err(|e| ApiError::BadRequest(format!("malformed WebAuthn assertion: {e}")))?;

    // Consumes the challenge whatever happens below — it is a `DELETE ... RETURNING`, so none
    // of the early returns can leave a replayable challenge behind.
    let (_, ceremony): (_, DiscoverableAuthentication) =
        take_ceremony(&state, body.ceremony_id, CeremonyKind::Authenticate).await?;

    // Two *claims* from the response: which account the authenticator thinks this is, and
    // which credential it used. Neither is trusted yet; they are the lookup keys.
    let (claimed_user, credential_id) = webauthn
        .identify_discoverable_authentication(&credential)
        .map_err(|e| verification_failed(&e))?;

    let record =
        tankovault_db::repo::users::passkeys::find_by_credential_id(&state.pool, credential_id)
            .await?
            .ok_or(ApiError::Unauthorized)?;

    // The two claims must agree with each other *and* with the database. An authenticator that
    // returns a user handle pointing at one account while presenting a credential registered to
    // another is either broken or lying, and the interesting case is the second: without this
    // check, an attacker holding their own valid credential could name the victim's handle and
    // have the session issued against whichever of the two the code happened to trust.
    if record.user_id.as_uuid() != claimed_user {
        tracing::warn!(
            credential_owner = %record.user_id.as_uuid(),
            claimed_user = %claimed_user,
            "webauthn user handle does not match the credential's owner; refusing"
        );
        record_attempt(
            &state,
            &client,
            record.user_id,
            &serde_json::json!({
                "reason": "user_handle_mismatch",
                "claimed_user": claimed_user,
            }),
            AuditOutcome::Denied,
        )
        .await;
        return Err(ApiError::Unauthorized);
    }

    let mut passkey: Passkey = serde_json::from_value(record.credential.clone()).map_err(|e| {
        tracing::error!(error = %e, passkey_id = %record.id, "a stored passkey no longer deserialises");
        ApiError::Internal
    })?;

    // The signature check. Everything above only decided *which* public key to check against.
    let result = match webauthn.finish_discoverable_authentication(
        &credential,
        ceremony,
        &[DiscoverableKey::from(&passkey)],
    ) {
        Ok(result) => result,
        Err(e) => {
            record_attempt(
                &state,
                &client,
                record.user_id,
                &serde_json::json!({ "reason": "assertion_rejected" }),
                AuditOutcome::Failure,
            )
            .await;
            return Err(verification_failed(&e));
        }
    };

    let user = tankovault_db::repo::users::get(&state.pool, record.user_id).await?;
    ensure_may_sign_in(&state, &client, &user).await?;

    // Best-effort bookkeeping, in this order because none of it is worth failing an
    // already-successful authentication over. `update_credential` advances the signature
    // counter and the backup flags; `needs_update` is false for most passkeys, which have no
    // counter at all because they synchronise between devices.
    if result.needs_update() {
        passkey.update_credential(&result);
    }
    if let Err(e) = tankovault_db::repo::users::passkeys::record_use(
        &state.pool,
        &record.credential_id,
        &serde_json::to_value(&passkey).unwrap_or(record.credential),
    )
    .await
    {
        tracing::warn!(error = %e, "failed to record passkey use");
    }
    if let Err(e) = tankovault_db::repo::users::touch_last_login(&state.pool, user.id).await {
        tracing::warn!(error = %e, "failed to record last login");
    }

    record_attempt(
        &state,
        &client,
        user.id,
        &serde_json::json!({
            "passkey_id": record.id,
            "user_verified": result.user_verified(),
        }),
        AuditOutcome::Success,
    )
    .await;

    issue_session(&state, jar, &user, Uuid::now_v7()).await
}

/// The two account gates a password sign-in passes, applied to a passkey sign-in.
///
/// They are not optional here just because the credential is stronger. Refusing at the door is
/// what makes a refusal legible — the alternative is signing the user in successfully and then
/// failing every subsequent request — and the email gate in particular is load-bearing: without
/// it, registering a passkey would be a way *around* email confirmation, which is the one thing
/// confirmation exists to prevent.
///
/// # Errors
/// [`ApiError::Suspended`] or [`ApiError::EmailNotVerified`], each audited before it returns.
async fn ensure_may_sign_in(
    state: &AppState,
    client: &ClientContext,
    user: &tankovault_domain::User,
) -> ApiResult<()> {
    if !user.status.may_authenticate() {
        record_attempt(
            state,
            client,
            user.id,
            &serde_json::json!({ "reason": "account_suspended" }),
            AuditOutcome::Denied,
        )
        .await;
        return Err(ApiError::Suspended);
    }

    // Skipped when the operator has switched confirmation off, matching `super::login`: leaving
    // it enforced would strand every account that registered while it was on.
    if !state
        .features
        .is_enabled(Feature::AccountsEmailVerification)
    {
        return Ok(());
    }
    if !tankovault_db::repo::users::is_email_verified(&state.pool, user.id).await? {
        record_attempt(
            state,
            client,
            user.id,
            &serde_json::json!({ "reason": "email_unverified" }),
            AuditOutcome::Denied,
        )
        .await;
        return Err(ApiError::EmailNotVerified);
    }
    Ok(())
}

/// Record one passkey sign-in attempt under `auth.passkey_login`.
///
/// Every exit from [`passkey_login_finish`] past the point the account is known goes through
/// here, which is the property that matters: the handler has six of them — a handle mismatch, a
/// rejected assertion, a suspension, an unconfirmed address, and success — and an audit trail
/// that covers five of six is one that cannot be trusted to answer what happened.
///
/// The action name and the target are fixed here rather than passed in for the same reason.
/// They were repeated at every call site, and a target that is the user id at five of them and
/// something else at the sixth is a log nobody can filter.
async fn record_attempt(
    state: &AppState,
    client: &ClientContext,
    user_id: tankovault_domain::UserId,
    detail: &serde_json::Value,
    outcome: AuditOutcome,
) {
    audit_anonymous(
        state,
        client,
        Some(user_id),
        "auth.passkey_login",
        &user_id.as_uuid().to_string(),
        detail,
        outcome,
    )
    .await;
}
