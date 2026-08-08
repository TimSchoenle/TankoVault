//! The second leg of a password sign-in: the account is known and its password verified, and a
//! second factor is still owed.
//!
//! # What the first leg hands over, and what it does not
//!
//! [`crate::auth::login`] answers `mfa_required` with an opaque handle and **no session** — no
//! access token, no refresh cookie. The handle resolves a `mfa_challenges` row that says only
//! which account is half-signed-in; holding it authorises nothing except the attempt to finish.
//! That separation is the entire security property: a caller who has the password but not the
//! factor must end up with strictly less than they had before.
//!
//! # Passkey sign-in does not come through here
//!
//! Deliberately. See the module doc on [`crate::mfa`].

use axum::Json;
use axum::extract::State;
use axum_extra::extract::cookie::CookieJar;
use secrecy::SecretString;
use serde::{Deserialize, Serialize};
use tankovault_service::AuditOutcome;
use utoipa::ToSchema;
use uuid::Uuid;

use crate::audit::audit_anonymous;
use crate::error::{ApiError, ApiResult};
use crate::mfa::{SecurityKeyAssertion, SecurityKeyChallenge};
use crate::openapi::AUTH_TAG;
use crate::state::{AppState, ClientContext};

use super::login::TokenResponse;
use super::session::issue_session;

/// How many factors may be presented against one pending sign-in.
///
/// A TOTP code is six digits; a challenge that lives five minutes with no cap is a million
/// guesses against a one-in-a-million secret, which is not a second factor. Five is generous for
/// a mistyped code and far short of useful for a script. Exhausting it destroys the challenge,
/// so the cost of trying again is a fresh password sign-in — itself on the auth rate-limit
/// budget.
const MAX_ATTEMPTS: i32 = 5;

/// The handle the first leg issued, plus exactly one factor.
#[derive(Debug, Deserialize, ToSchema)]
pub struct MfaVerifyRequest {
    /// The `challenge_token` from the sign-in response.
    #[schema(value_type = String)]
    pub challenge_token: SecretString,
    /// A code from the enrolled authenticator app.
    #[schema(value_type = Option<String>)]
    #[serde(default)]
    pub totp_code: Option<SecretString>,
    /// One unused recovery code.
    #[schema(value_type = Option<String>)]
    #[serde(default)]
    pub recovery_code: Option<SecretString>,
    /// A completed security-key assertion, from [`mfa_security_key_start`].
    #[serde(default)]
    pub security_key: Option<SecurityKeyAssertion>,
}

/// The handle alone, to fetch a security-key challenge for the pending sign-in.
#[derive(Debug, Deserialize, ToSchema)]
pub struct MfaChallengeRequest {
    #[schema(value_type = String)]
    pub challenge_token: SecretString,
}

/// What a client must do to finish signing in.
#[derive(Debug, Serialize, ToSchema)]
pub struct MfaChallenge {
    /// Present this alongside a factor at `POST /v1/auth/mfa/verify`.
    // Unwrapped exactly once, at the boundary handing it to the client. It is a bearer handle
    // for the rest of the sign-in, so it is wrapped everywhere else.
    #[serde(serialize_with = "crate::secret::expose_onto_wire")]
    #[schema(value_type = String)]
    pub challenge_token: SecretString,
    /// Which factors this account can actually present, so the client offers only those.
    ///
    /// Never the account's *identity* — this response is reachable with a password alone, so
    /// anything beyond "an authenticator app and two security keys exist here" would be a
    /// disclosure bought with a credential the attacker already spent.
    pub methods: Vec<MfaMethod>,
    /// Seconds until the pending sign-in lapses.
    pub expires_in: i64,
}

/// A factor a pending sign-in may be finished with.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum MfaMethod {
    Totp,
    SecurityKey,
    RecoveryCode,
}

/// Begin a security-key challenge for a pending sign-in
///
/// The assertion options for the account behind `challenge_token`. Separate from
/// `/v1/me/step-up/security-key/start` because there is no session yet — the caller is
/// half-signed-in, and the handle is the only thing naming the account.
#[utoipa::path(
    post,
    path = "/v1/auth/mfa/security-key/start",
    tag = AUTH_TAG,
    request_body = MfaChallengeRequest,
    responses(
        (status = 200, description = "Challenge issued", body = SecurityKeyChallenge),
        (status = 400, description = "No security key is registered to this account", body = crate::error::ProblemDetails),
        (status = 401, description = "Unknown or expired challenge", body = crate::error::ProblemDetails),
        (status = 503, description = "This deployment has no WebAuthn relying party configured", body = crate::error::ProblemDetails),
    )
)]
pub async fn mfa_security_key_start(
    State(state): State<AppState>,
    Json(req): Json<MfaChallengeRequest>,
) -> ApiResult<Json<SecurityKeyChallenge>> {
    // Deliberately does *not* charge an attempt: fetching a challenge is not a guess, and
    // charging for it would let a client burn its own attempts by retrying a flaky network.
    let pending = tankovault_db::repo::users::mfa::charge_challenge_attempt(
        &state.pool,
        &tankovault_auth::hash_handle(&req.challenge_token),
    )
    .await?
    .ok_or(ApiError::Unauthorized)?;

    Ok(Json(
        crate::mfa::begin_security_key_assertion(&state, pending.user_id).await?,
    ))
}

/// Finish signing in
///
/// Trades the pending sign-in plus one factor for the session the first leg withheld.
///
/// Every outcome is audited, success and failure alike — an authentication log that records only
/// successes cannot answer the question anyone actually asks it after an incident.
#[utoipa::path(
    post,
    path = "/v1/auth/mfa/verify",
    tag = AUTH_TAG,
    request_body = MfaVerifyRequest,
    responses(
        (status = 200, description = "Authenticated; access token issued", body = TokenResponse),
        (status = 400, description = "No factor was presented, or more than one", body = crate::error::ProblemDetails),
        (status = 401, description = "Unknown, expired or exhausted challenge, or a wrong factor", body = crate::error::ProblemDetails),
        (status = 403, description = "The account was suspended since the first leg", body = crate::error::ProblemDetails),
        (status = 429, description = "Too many attempts; retry later", body = crate::error::ProblemDetails),
    )
)]
pub async fn mfa_verify(
    State(state): State<AppState>,
    client: ClientContext,
    jar: CookieJar,
    Json(req): Json<MfaVerifyRequest>,
) -> ApiResult<(CookieJar, Json<TokenResponse>)> {
    let presented = u8::from(req.totp_code.is_some())
        + u8::from(req.recovery_code.is_some())
        + u8::from(req.security_key.is_some());
    if presented != 1 {
        return Err(ApiError::BadRequest(
            "present exactly one of totp_code, recovery_code or security_key".to_owned(),
        ));
    }

    // Resolving the challenge *charges* an attempt, in the same statement. A separate
    // "count this failure" call is one an early return can skip, and the six-digit code this
    // protects only holds up if every guess is counted.
    let pending = tankovault_db::repo::users::mfa::charge_challenge_attempt(
        &state.pool,
        &tankovault_auth::hash_handle(&req.challenge_token),
    )
    .await?
    .ok_or(ApiError::Unauthorized)?;

    if pending.attempts > MAX_ATTEMPTS {
        tankovault_db::repo::users::mfa::delete_challenge(&state.pool, pending.id).await?;
        return Err(refuse(
            &state,
            &client,
            pending.user_id,
            &serde_json::json!({ "reason": "attempts_exhausted" }),
            AuditOutcome::Denied,
            ApiError::Unauthorized,
        )
        .await);
    }

    let (ok, factor) = present_factor(&state, pending.user_id, &req).await?;
    if !ok {
        return Err(refuse(
            &state,
            &client,
            pending.user_id,
            &serde_json::json!({ "reason": "bad_factor", "factor": factor }),
            AuditOutcome::Failure,
            ApiError::Unauthorized,
        )
        .await);
    }

    // Consumed on success, so one challenge yields at most one session even if two requests
    // carrying different valid factors arrive together.
    tankovault_db::repo::users::mfa::delete_challenge(&state.pool, pending.id).await?;

    let user = tankovault_db::repo::users::get(&state.pool, pending.user_id).await?;
    // Re-checked here, not just at the first leg: a suspension applied between the two legs
    // must stop the sign-in, or a five-minute window exists in which an administrator's action
    // does nothing.
    if !user.status.may_authenticate() {
        return Err(refuse(
            &state,
            &client,
            user.id,
            &serde_json::json!({ "reason": "account_suspended" }),
            AuditOutcome::Denied,
            ApiError::Suspended,
        )
        .await);
    }

    audit_anonymous(
        &state,
        &client,
        Some(user.id),
        "auth.mfa.verify",
        &user.id.as_uuid().to_string(),
        &serde_json::json!({ "factor": factor }),
        AuditOutcome::Success,
    )
    .await;
    if factor == "recovery_code" {
        // Worth its own record: a recovery code being spent is either a user who lost their
        // phone or an attacker who found a printout, and both are things an operator wants to
        // see without reading every successful sign-in.
        audit_anonymous(
            &state,
            &client,
            Some(user.id),
            "auth.recovery_code.used",
            &user.id.as_uuid().to_string(),
            &serde_json::json!({}),
            AuditOutcome::Success,
        )
        .await;
    }

    if let Err(e) = tankovault_db::repo::users::touch_last_login(&state.pool, user.id).await {
        tracing::warn!(error = %e, "failed to record last login");
    }

    issue_session(&state, jar, &user, Uuid::now_v7()).await
}

/// Verify whichever factor the request carried, returning whether it held and which it was.
///
/// Split out so [`mfa_verify`] reads as the sequence of gates it is. The `factor` string is for
/// the audit record only — the caller answers identically whichever one failed.
///
/// # Errors
/// Propagates a database or configuration failure. A *wrong* factor is `Ok((false, _))`.
async fn present_factor(
    state: &AppState,
    user_id: tankovault_domain::UserId,
    req: &MfaVerifyRequest,
) -> ApiResult<(bool, &'static str)> {
    if let Some(code) = req.totp_code.as_ref() {
        return Ok((crate::mfa::verify_totp(state, user_id, code).await?, "totp"));
    }
    if let Some(code) = req.recovery_code.as_ref() {
        return Ok((
            crate::mfa::verify_recovery_code(state, user_id, code).await?,
            "recovery_code",
        ));
    }
    // The caller has already established that exactly one field is set, so this is the third.
    let assertion = req.security_key.as_ref().ok_or(ApiError::Internal)?;
    Ok((
        crate::mfa::finish_security_key_assertion(state, user_id, assertion)
            .await
            .is_ok(),
        "security_key",
    ))
}

/// Audit a refused second leg and hand back the error to return.
///
/// One shape for every refusal, for the reason `crate::auth::login::refuse` gives.
async fn refuse(
    state: &AppState,
    client: &ClientContext,
    user_id: tankovault_domain::UserId,
    detail: &serde_json::Value,
    outcome: AuditOutcome,
    error: ApiError,
) -> ApiError {
    audit_anonymous(
        state,
        client,
        Some(user_id),
        "auth.mfa.verify",
        &user_id.as_uuid().to_string(),
        detail,
        outcome,
    )
    .await;
    error
}

/// Open a pending sign-in for `user_id` and describe how it can be finished.
///
/// # Errors
/// [`ApiError::Internal`] on a database failure.
pub(super) async fn open_challenge(
    state: &AppState,
    user_id: tankovault_domain::UserId,
) -> ApiResult<MfaChallenge> {
    let token = tankovault_auth::generate_handle();
    tankovault_db::repo::users::mfa::insert_challenge(
        &state.pool,
        user_id,
        &tankovault_auth::hash_handle(&token),
        time::OffsetDateTime::now_utc() + state.mfa_challenge_ttl,
    )
    .await?;

    let mut methods = Vec::new();
    if tankovault_db::repo::users::mfa::get_totp(&state.pool, user_id)
        .await?
        .is_some_and(|t| t.confirmed_at.is_some())
    {
        methods.push(MfaMethod::Totp);
    }
    if tankovault_db::repo::users::webauthn::has_security_key(&state.pool, user_id).await? {
        methods.push(MfaMethod::SecurityKey);
    }
    if tankovault_db::repo::users::mfa::recovery_codes_remaining(&state.pool, user_id).await? > 0 {
        methods.push(MfaMethod::RecoveryCode);
    }

    Ok(MfaChallenge {
        challenge_token: token,
        methods,
        expires_in: state.mfa_challenge_ttl.whole_seconds(),
    })
}
