//! Managing one's own second factor, and the step-up prompt that guards sensitive actions.
//!
//! The verification itself lives in [`crate::mfa`], shared with the sign-in second leg. What is
//! here is enrolment, revocation, and the endpoints a client calls to satisfy a `403
//! step_up_required`.
//!
//! # Why enrolment is itself a sensitive action
//!
//! Every write below demands a step-up **once a factor already exists**. An attacker holding a
//! live access token but no factor would otherwise simply enrol their own — replacing the
//! owner's second factor with theirs, and locking the owner out with the very mechanism that
//! was supposed to protect them. The first enrolment cannot demand a factor that does not exist
//! yet, so it falls back to the password, which is exactly the protection the account had a
//! moment earlier and no less.

use axum::Json;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use secrecy::SecretString;
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;
use utoipa::ToSchema;
use uuid::Uuid;
use webauthn_rs::prelude::{RegisterPublicKeyCredential, SecurityKeyRegistration};

use crate::audit::{audit, audit_failure};
use crate::error::{ApiError, ApiResult};
use crate::mfa::{SecurityKeyAssertion, SecurityKeyChallenge, StepUpGrant};
use crate::openapi::ME_ACCOUNT_TAG;
use crate::passkey::{
    begin_ceremony, ceremony_start_failed, relying_party, take_ceremony, verification_failed,
};
use crate::state::{AppState, AuthUser};
use crate::step_up::{Elevated, require_elevation_if_enrolled};
use tankovault_db::repo::users::mfa::StepUpMethod;
use tankovault_db::repo::users::webauthn::{CeremonyKind, CredentialPurpose};

/// Longest accepted security-key label, in characters. Matches the passkey limit; the two lists
/// sit next to each other on one page and a different ceiling on each would read as a bug.
const MAX_LABEL_LEN: usize = 64;

/// The label applied when the client sends none.
const DEFAULT_LABEL: &str = "Security key";

// ---------------------------------------------------------------------------
// Status
// ---------------------------------------------------------------------------

/// A registered security key, as the account page shows it.
///
/// Deliberately without the credential id, for the reason `PasskeyDto` gives: it is the lookup
/// key a sign-in resolves from, and publishing every account's ids on an authenticated page
/// hands whoever compromises one session the material to recognise that user's authenticator
/// elsewhere.
#[derive(Debug, Serialize, ToSchema)]
pub struct SecurityKeyDto {
    pub id: Uuid,
    pub label: String,
    #[serde(with = "time::serde::rfc3339")]
    #[schema(value_type = String)]
    pub created_at: OffsetDateTime,
    /// When this key was last presented; absent if it never has been.
    #[serde(
        with = "time::serde::rfc3339::option",
        skip_serializing_if = "Option::is_none"
    )]
    #[schema(value_type = Option<String>)]
    pub last_used_at: Option<OffsetDateTime>,
}

/// The caller's second-factor state, as the account page renders it.
#[derive(Debug, Serialize, ToSchema)]
pub struct MfaStatus {
    /// Whether any usable factor exists. The single answer the passkey gate and the privileged
    /// requirement both consult — clients must not recompute it from the fields below.
    pub enrolled: bool,
    /// When the authenticator app was confirmed, or absent if there is none.
    ///
    /// An enrolment that was started and never confirmed reads as absent here, on purpose: it
    /// is not a factor, and showing it as one would tell a user they are protected when they
    /// are not.
    #[serde(
        with = "time::serde::rfc3339::option",
        skip_serializing_if = "Option::is_none"
    )]
    #[schema(value_type = Option<String>)]
    pub totp_confirmed_at: Option<OffsetDateTime>,
    pub security_keys: Vec<SecurityKeyDto>,
    /// Unused recovery codes. Zero with a factor enrolled is a state worth warning about: the
    /// account has a second factor and no way past it if the factor is lost.
    pub recovery_codes_remaining: i64,
    /// Whether this deployment requires every account to enrol, not just privileged ones.
    pub required: bool,
    /// Whether authenticator-app enrolment is available at all. `false` when the operator has
    /// configured no sealing key, in which case the client offers security keys only rather
    /// than a button that answers `503`.
    pub totp_available: bool,
}

/// Get my two-factor status
///
/// Everything the account page needs to render the second-factor section, in one read.
#[utoipa::path(
    get,
    path = "/v1/me/mfa",
    tag = ME_ACCOUNT_TAG,
    security(("bearer_auth" = [])),
    responses(
        (status = 200, description = "The caller's second-factor state", body = MfaStatus),
        (status = 401, description = "authentication required", body = crate::error::ProblemDetails),
    )
)]
pub async fn mfa_status(
    State(state): State<AppState>,
    user: AuthUser,
) -> ApiResult<Json<MfaStatus>> {
    let totp = tankovault_db::repo::users::mfa::get_totp(&state.pool, user.user_id).await?;
    let keys = tankovault_db::repo::users::webauthn::list_for_user(
        &state.pool,
        user.user_id,
        CredentialPurpose::SecurityKey,
    )
    .await?;
    let remaining =
        tankovault_db::repo::users::mfa::recovery_codes_remaining(&state.pool, user.user_id)
            .await?;

    Ok(Json(MfaStatus {
        enrolled: user.mfa_enrolled,
        totp_confirmed_at: totp.and_then(|t| t.confirmed_at),
        security_keys: keys
            .into_iter()
            .map(|r| SecurityKeyDto {
                id: r.id,
                label: r.label,
                created_at: r.created_at,
                last_used_at: r.last_used_at,
            })
            .collect(),
        recovery_codes_remaining: remaining,
        required: state
            .features
            .is_enabled(tankovault_domain::Feature::AccountsMfaRequired),
        totp_available: state.mfa_sealer.is_some(),
    }))
}

// ---------------------------------------------------------------------------
// TOTP
// ---------------------------------------------------------------------------

/// A freshly issued authenticator-app secret. Shown once, never retrievable again.
#[derive(Debug, Serialize, ToSchema)]
pub struct TotpEnrolmentChallenge {
    /// The shared secret, base32-encoded, for a user whose app cannot scan a QR code.
    #[serde(serialize_with = "crate::secret::expose_onto_wire")]
    #[schema(value_type = String)]
    pub secret: SecretString,
    /// The `otpauth://` URI the client renders as a QR code. Carries the secret, so it is as
    /// sensitive as the field above.
    #[serde(serialize_with = "crate::secret::expose_onto_wire")]
    #[schema(value_type = String)]
    pub provisioning_uri: SecretString,
}

/// A code proving the user stored the secret.
#[derive(Debug, Deserialize, ToSchema)]
pub struct TotpConfirm {
    #[schema(value_type = String)]
    pub code: SecretString,
}

/// A recovery-code set, returned exactly once.
#[derive(Debug, Serialize, ToSchema)]
pub struct RecoveryCodes {
    /// The plaintext codes. Only the digests are stored, so this response is the only copy
    /// that will ever exist — a client that does not show them has lost them.
    #[schema(value_type = Vec<String>)]
    #[serde(serialize_with = "crate::secret::expose_list_onto_wire")]
    pub codes: Vec<SecretString>,
}

/// Begin authenticator-app enrolment
///
/// Issues a fresh shared secret and the `otpauth://` URI for it. Nothing is a usable factor
/// until [`confirm_totp`] accepts a code generated from it.
///
/// Restarting is allowed and simply replaces the pending secret — a user whose QR code never
/// reached their phone asks again. Replacing a *confirmed* enrolment is refused; remove it
/// first, which costs a step-up, so a stolen session cannot swap the owner's factor for its own.
#[utoipa::path(
    post,
    path = "/v1/me/mfa/totp",
    tag = ME_ACCOUNT_TAG,
    security(("bearer_auth" = [])),
    responses(
        (status = 200, description = "Secret issued; confirm it to enrol", body = TotpEnrolmentChallenge),
        (status = 401, description = "authentication required", body = crate::error::ProblemDetails),
        (status = 403, description = "a step-up is required, or a confirmed enrolment already exists", body = crate::error::ProblemDetails),
        (status = 409, description = "an authenticator app is already enrolled", body = crate::error::ProblemDetails),
        (status = 503, description = "this deployment configured no auth.mfa_encryption_key", body = crate::error::ProblemDetails),
    )
)]
pub async fn begin_totp(
    State(state): State<AppState>,
    user: AuthUser,
) -> ApiResult<Json<TotpEnrolmentChallenge>> {
    require_elevation_if_enrolled(&user)?;

    let account = tankovault_db::repo::users::get(&state.pool, user.user_id).await?;
    let secret = tankovault_auth::totp::generate_secret();
    let sealed = crate::mfa::seal_secret(&state, &secret)?;

    let stored = tankovault_db::repo::users::mfa::begin_totp_enrolment(
        &state.pool,
        user.user_id,
        &sealed,
        &account.username,
    )
    .await?;
    if !stored {
        return Err(ApiError::Conflict(
            "an authenticator app is already enrolled; remove it before enrolling another"
                .to_owned(),
        ));
    }

    Ok(Json(TotpEnrolmentChallenge {
        secret: tankovault_auth::totp::encode_secret(&secret),
        provisioning_uri: tankovault_auth::totp::provisioning_uri(
            &state.totp_issuer,
            &account.username,
            &secret,
        ),
    }))
}

/// Confirm authenticator-app enrolment
///
/// Accepts a code generated from the pending secret, which is the only proof the user actually
/// stored it. On success the enrolment becomes a live factor and a **recovery-code set is
/// issued** — returned here and never again.
///
/// The codes come with the first factor rather than being a separate opt-in step, because a
/// second factor without an escape hatch is a way to lose an account, and an escape hatch
/// nobody clicked through to is the same thing.
#[utoipa::path(
    post,
    path = "/v1/me/mfa/totp/confirm",
    tag = ME_ACCOUNT_TAG,
    request_body = TotpConfirm,
    security(("bearer_auth" = [])),
    responses(
        (status = 200, description = "Enrolled; recovery codes issued, shown once", body = RecoveryCodes),
        (status = 400, description = "No enrolment is pending", body = crate::error::ProblemDetails),
        (status = 401, description = "authentication required, or the code is wrong", body = crate::error::ProblemDetails),
        (status = 503, description = "this deployment configured no auth.mfa_encryption_key", body = crate::error::ProblemDetails),
    )
)]
pub async fn confirm_totp(
    State(state): State<AppState>,
    user: AuthUser,
    Json(body): Json<TotpConfirm>,
) -> ApiResult<Json<RecoveryCodes>> {
    let Some(enrolment) =
        tankovault_db::repo::users::mfa::get_totp(&state.pool, user.user_id).await?
    else {
        return Err(ApiError::BadRequest(
            "no authenticator-app enrolment is pending".to_owned(),
        ));
    };
    if enrolment.confirmed_at.is_some() {
        return Err(ApiError::BadRequest(
            "this authenticator app is already enrolled".to_owned(),
        ));
    }

    // Verified against the *unconfirmed* row, which `crate::mfa::verify_totp` deliberately
    // refuses to look at — this is the one moment an unconfirmed secret is allowed to answer.
    let secret = crate::mfa::open_secret(&state, &enrolment.secret, user.user_id)?;
    let Some(step) = tankovault_auth::totp::verify(
        &secret,
        &body.code,
        OffsetDateTime::now_utc(),
        enrolment.last_step,
    ) else {
        audit_failure(
            &state,
            &user,
            "me.mfa.enrol",
            &user.user_id.as_uuid().to_string(),
            &serde_json::json!({ "factor": "totp", "reason": "bad_code" }),
        )
        .await;
        return Err(ApiError::Unauthorized);
    };

    if tankovault_db::repo::users::mfa::confirm_totp(&state.pool, user.user_id, step).await? == 0 {
        // Lost a race with another confirmation of the same pending enrolment. Answering
        // "already enrolled" rather than re-issuing recovery codes matters: a second set would
        // silently invalidate the first, which the winning request has already displayed.
        return Err(ApiError::BadRequest(
            "this authenticator app is already enrolled".to_owned(),
        ));
    }

    let codes = crate::mfa::issue_recovery_codes(&state, user.user_id).await?;
    audit(
        &state,
        &user,
        "me.mfa.enrol",
        &user.user_id.as_uuid().to_string(),
        &serde_json::json!({ "factor": "totp" }),
    )
    .await;
    Ok(Json(RecoveryCodes { codes }))
}

/// Remove the authenticator app
///
/// Every step-up grant the caller holds is revoked with it: an elevation earned by the factor
/// being removed must not outlive it, or the window it opened stays usable after the credential
/// is gone.
#[utoipa::path(
    delete,
    path = "/v1/me/mfa/totp",
    tag = ME_ACCOUNT_TAG,
    security(("bearer_auth" = [])),
    responses(
        (status = 204, description = "Removed"),
        (status = 401, description = "authentication required", body = crate::error::ProblemDetails),
        (status = 403, description = "a step-up is required", body = crate::error::ProblemDetails),
        (status = 404, description = "no authenticator app is enrolled", body = crate::error::ProblemDetails),
    )
)]
pub async fn delete_totp(
    State(state): State<AppState>,
    Elevated(user): Elevated,
) -> ApiResult<StatusCode> {
    let removed = tankovault_db::repo::users::mfa::delete_totp(&state.pool, user.user_id).await?;
    if removed == 0 {
        return Err(ApiError::NotFound);
    }
    tankovault_db::repo::users::mfa::revoke_step_ups(&state.pool, user.user_id).await?;
    audit(
        &state,
        &user,
        "me.mfa.remove",
        &user.user_id.as_uuid().to_string(),
        &serde_json::json!({ "factor": "totp" }),
    )
    .await;
    Ok(StatusCode::NO_CONTENT)
}

// ---------------------------------------------------------------------------
// Security keys
// ---------------------------------------------------------------------------

/// What to call a new security key.
#[derive(Debug, Deserialize, ToSchema)]
pub struct SecurityKeyRegisterStart {
    /// Defaults to "Security key" when absent or blank.
    #[serde(default)]
    pub label: Option<String>,
}

/// The creation challenge, plus the handle the client echoes back.
#[derive(Debug, Serialize, ToSchema)]
pub struct SecurityKeyRegisterChallenge {
    pub ceremony_id: Uuid,
    /// A W3C `PublicKeyCredentialCreationOptions` envelope. Hand it to the browser unmodified.
    #[schema(value_type = Object)]
    pub options: serde_json::Value,
}

/// The attestation `navigator.credentials.create()` produced.
#[derive(Debug, Deserialize, ToSchema)]
pub struct SecurityKeyRegisterFinish {
    pub ceremony_id: Uuid,
    #[schema(value_type = Object)]
    pub credential: serde_json::Value,
}

/// A new name for an existing key.
#[derive(Debug, Deserialize, ToSchema)]
pub struct SecurityKeyRename {
    pub label: String,
}

/// The ceremony state carried between the two registration legs.
#[derive(serde::Serialize, serde::Deserialize)]
struct StoredRegistration {
    registration: SecurityKeyRegistration,
    /// Decided at `start` and carried through, so the two requests cannot disagree on it.
    label: String,
}

/// Begin registering a security key
///
/// Issues a `WebAuthn` creation challenge hinted toward a cross-platform authenticator.
///
/// The exclusion list is every credential already registered to the account — passkeys
/// **included**. That cross-purpose exclusion is the point: an authenticator that already holds
/// this account's passkey must not also become its second factor, or one touch would clear both
/// legs of a sign-in that is supposed to need two independent proofs.
#[utoipa::path(
    post,
    path = "/v1/me/mfa/security-keys/register/start",
    tag = ME_ACCOUNT_TAG,
    request_body = SecurityKeyRegisterStart,
    security(("bearer_auth" = [])),
    responses(
        (status = 200, description = "Challenge issued", body = SecurityKeyRegisterChallenge),
        (status = 400, description = "The label is too long", body = crate::error::ProblemDetails),
        (status = 401, description = "authentication required", body = crate::error::ProblemDetails),
        (status = 403, description = "a step-up is required", body = crate::error::ProblemDetails),
        (status = 503, description = "this deployment has no WebAuthn relying party configured", body = crate::error::ProblemDetails),
    )
)]
pub async fn security_key_register_start(
    State(state): State<AppState>,
    user: AuthUser,
    Json(body): Json<SecurityKeyRegisterStart>,
) -> ApiResult<Json<SecurityKeyRegisterChallenge>> {
    require_elevation_if_enrolled(&user)?;

    let webauthn = relying_party(&state)?;
    let label = normalise_label(body.label.as_deref())?;
    let account = tankovault_db::repo::users::get(&state.pool, user.user_id).await?;
    let existing =
        tankovault_db::repo::users::webauthn::credential_ids_for_user(&state.pool, user.user_id)
            .await?;

    let (challenge, registration) = webauthn
        .start_securitykey_registration(
            account.id.as_uuid(),
            &account.username,
            &account.username,
            Some(existing),
            // No attestation CA list: this deployment accepts any authenticator, the same
            // policy passkeys are registered under. Demanding attestation would mean
            // maintaining a list of approved hardware, which is an enterprise problem and not
            // this one.
            None,
            None,
        )
        .map_err(|e| ceremony_start_failed(&e))?;

    let ceremony_id = begin_ceremony(
        &state,
        Some(user.user_id),
        CeremonyKind::RegisterSecurityKey,
        &StoredRegistration {
            registration,
            label,
        },
    )
    .await?;

    Ok(Json(SecurityKeyRegisterChallenge {
        ceremony_id,
        options: serde_json::to_value(&challenge).map_err(|e| {
            tracing::error!(error = %e, "could not serialise a security-key challenge");
            ApiError::Internal
        })?,
    }))
}

/// Finish registering a security key
///
/// Verifies the attestation and stores the credential as a second factor.
///
/// When this is the caller's **first** factor, a recovery-code set is issued with it and
/// returned once — the same rule authenticator-app enrolment follows, for the same reason.
#[utoipa::path(
    post,
    path = "/v1/me/mfa/security-keys/register/finish",
    tag = ME_ACCOUNT_TAG,
    request_body = SecurityKeyRegisterFinish,
    security(("bearer_auth" = [])),
    responses(
        (status = 200, description = "Registered; recovery codes when this is the first factor", body = SecurityKeyRegistered),
        (status = 401, description = "authentication required, or verification failed", body = crate::error::ProblemDetails),
        (status = 409, description = "this authenticator is already registered", body = crate::error::ProblemDetails),
        (status = 503, description = "this deployment has no WebAuthn relying party configured", body = crate::error::ProblemDetails),
    )
)]
pub async fn security_key_register_finish(
    State(state): State<AppState>,
    user: AuthUser,
    Json(body): Json<SecurityKeyRegisterFinish>,
) -> ApiResult<Json<SecurityKeyRegistered>> {
    let webauthn = relying_party(&state)?;
    let credential: RegisterPublicKeyCredential =
        serde_json::from_value(body.credential).map_err(|e| {
            tracing::debug!(error = %e, "malformed security-key attestation");
            ApiError::Unauthorized
        })?;

    let (ceremony_user, stored): (_, StoredRegistration) =
        take_ceremony(&state, body.ceremony_id, CeremonyKind::RegisterSecurityKey).await?;
    if ceremony_user != Some(user.user_id) {
        return Err(ApiError::Unauthorized);
    }

    let key = webauthn
        .finish_securitykey_registration(&credential, &stored.registration)
        .map_err(|e| verification_failed(&e))?;

    // Read before the insert: afterwards the account always has a factor, and "was this the
    // first" — the question that decides whether recovery codes are issued — is unanswerable.
    let had_factor = crate::mfa::is_enrolled(&state, user.user_id).await?;

    let serialised = serde_json::to_value(&key).map_err(|e| {
        tracing::error!(error = %e, "could not serialise a security key for storage");
        ApiError::Internal
    })?;
    let record = tankovault_db::repo::users::webauthn::insert(
        &state.pool,
        user.user_id,
        key.cred_id().as_ref(),
        &serialised,
        &stored.label,
        CredentialPurpose::SecurityKey,
    )
    .await?;

    let codes = if had_factor {
        None
    } else {
        Some(crate::mfa::issue_recovery_codes(&state, user.user_id).await?)
    };

    audit(
        &state,
        &user,
        "me.mfa.enrol",
        &user.user_id.as_uuid().to_string(),
        &serde_json::json!({ "factor": "security_key", "key_id": record.id }),
    )
    .await;

    Ok(Json(SecurityKeyRegistered {
        key: SecurityKeyDto {
            id: record.id,
            label: record.label,
            created_at: record.created_at,
            last_used_at: record.last_used_at,
        },
        recovery_codes: codes,
    }))
}

/// A newly registered security key, plus the recovery codes if it was the first factor.
#[derive(Debug, Serialize, ToSchema)]
pub struct SecurityKeyRegistered {
    pub key: SecurityKeyDto,
    /// Present only when this registration was the account's first second factor. Shown once.
    #[schema(value_type = Option<Vec<String>>)]
    #[serde(
        serialize_with = "crate::secret::expose_optional_list_onto_wire",
        skip_serializing_if = "Option::is_none"
    )]
    pub recovery_codes: Option<Vec<SecretString>>,
}

/// Rename a security key
#[utoipa::path(
    patch,
    path = "/v1/me/mfa/security-keys/{id}",
    tag = ME_ACCOUNT_TAG,
    params(("id" = Uuid, Path, description = "The key's id")),
    request_body = SecurityKeyRename,
    security(("bearer_auth" = [])),
    responses(
        (status = 204, description = "Renamed"),
        (status = 400, description = "The label is too long", body = crate::error::ProblemDetails),
        (status = 401, description = "authentication required", body = crate::error::ProblemDetails),
        (status = 403, description = "a step-up is required", body = crate::error::ProblemDetails),
        (status = 404, description = "no such key on this account", body = crate::error::ProblemDetails),
    )
)]
pub async fn rename_security_key(
    State(state): State<AppState>,
    Elevated(user): Elevated,
    Path(id): Path<Uuid>,
    Json(body): Json<SecurityKeyRename>,
) -> ApiResult<StatusCode> {
    let label = normalise_label(Some(&body.label))?;
    let changed = tankovault_db::repo::users::webauthn::rename(
        &state.pool,
        user.user_id,
        id,
        &label,
        CredentialPurpose::SecurityKey,
    )
    .await?;
    if changed == 0 {
        return Err(ApiError::NotFound);
    }
    Ok(StatusCode::NO_CONTENT)
}

/// Revoke a security key
///
/// Revokes every step-up grant with it, for the reason [`delete_totp`] gives.
#[utoipa::path(
    delete,
    path = "/v1/me/mfa/security-keys/{id}",
    tag = ME_ACCOUNT_TAG,
    params(("id" = Uuid, Path, description = "The key's id")),
    security(("bearer_auth" = [])),
    responses(
        (status = 204, description = "Revoked"),
        (status = 401, description = "authentication required", body = crate::error::ProblemDetails),
        (status = 403, description = "a step-up is required", body = crate::error::ProblemDetails),
        (status = 404, description = "no such key on this account", body = crate::error::ProblemDetails),
    )
)]
pub async fn delete_security_key(
    State(state): State<AppState>,
    Elevated(user): Elevated,
    Path(id): Path<Uuid>,
) -> ApiResult<StatusCode> {
    let removed = tankovault_db::repo::users::webauthn::delete(
        &state.pool,
        user.user_id,
        id,
        CredentialPurpose::SecurityKey,
    )
    .await?;
    if removed == 0 {
        return Err(ApiError::NotFound);
    }
    tankovault_db::repo::users::mfa::revoke_step_ups(&state.pool, user.user_id).await?;
    audit(
        &state,
        &user,
        "me.mfa.remove",
        &user.user_id.as_uuid().to_string(),
        &serde_json::json!({ "factor": "security_key", "key_id": id }),
    )
    .await;
    Ok(StatusCode::NO_CONTENT)
}

// ---------------------------------------------------------------------------
// Recovery codes
// ---------------------------------------------------------------------------

/// Regenerate my recovery codes
///
/// Issues a fresh set and invalidates every previous code. Returned once.
#[utoipa::path(
    post,
    path = "/v1/me/mfa/recovery-codes",
    tag = ME_ACCOUNT_TAG,
    security(("bearer_auth" = [])),
    responses(
        (status = 200, description = "A fresh set, shown once", body = RecoveryCodes),
        (status = 401, description = "authentication required", body = crate::error::ProblemDetails),
        (status = 403, description = "a step-up is required", body = crate::error::ProblemDetails),
    )
)]
pub async fn regenerate_recovery_codes(
    State(state): State<AppState>,
    Elevated(user): Elevated,
) -> ApiResult<Json<RecoveryCodes>> {
    let codes = crate::mfa::issue_recovery_codes(&state, user.user_id).await?;
    audit(
        &state,
        &user,
        "me.mfa.recovery_codes",
        &user.user_id.as_uuid().to_string(),
        &serde_json::json!({ "action": "regenerated" }),
    )
    .await;
    Ok(Json(RecoveryCodes { codes }))
}

// ---------------------------------------------------------------------------
// Step-up
// ---------------------------------------------------------------------------

/// One factor, presented to earn an elevation.
///
/// Exactly one field must be set. A flat struct rather than a tagged enum because the generated
/// client renders a `oneOf` as an enum that is awkward to construct, and because "exactly one of
/// these" is checked here in one place either way.
#[derive(Debug, Deserialize, ToSchema)]
pub struct StepUpRequest {
    /// A code from the enrolled authenticator app.
    #[schema(value_type = Option<String>)]
    #[serde(default)]
    pub totp_code: Option<SecretString>,
    /// One unused recovery code.
    #[schema(value_type = Option<String>)]
    #[serde(default)]
    pub recovery_code: Option<SecretString>,
    /// The account password. Accepted **only** while no second factor is enrolled — see the
    /// handler.
    #[schema(value_type = Option<String>)]
    #[serde(default)]
    pub password: Option<SecretString>,
}

/// Step up
///
/// Trades a second factor for a short-lived elevation, presented in `X-Step-Up` on the
/// sensitive request that demanded it.
///
/// # The password branch
///
/// Accepted only while the account has **no** factor enrolled. Such an account has no stronger
/// proof to offer, and refusing it would leave the sensitive routes unreachable — including the
/// enrolment that would fix that. The moment a factor exists the branch is refused here, and
/// grants it already issued stop being honoured (`crate::step_up`), so enrolling never leaves a
/// weaker proof usable beside the stronger one.
#[utoipa::path(
    post,
    path = "/v1/me/step-up",
    tag = ME_ACCOUNT_TAG,
    request_body = StepUpRequest,
    security(("bearer_auth" = [])),
    responses(
        (status = 200, description = "Elevated", body = StepUpGrant),
        (status = 400, description = "No factor was presented, or more than one", body = crate::error::ProblemDetails),
        (status = 401, description = "authentication required, or the factor is wrong", body = crate::error::ProblemDetails),
    )
)]
pub async fn step_up(
    State(state): State<AppState>,
    user: AuthUser,
    Json(body): Json<StepUpRequest>,
) -> ApiResult<Json<StepUpGrant>> {
    let presented = u8::from(body.totp_code.is_some())
        + u8::from(body.recovery_code.is_some())
        + u8::from(body.password.is_some());
    if presented != 1 {
        return Err(ApiError::BadRequest(
            "present exactly one of totp_code, recovery_code or password".to_owned(),
        ));
    }

    let method = if let Some(code) = body.totp_code.as_ref() {
        if !crate::mfa::verify_totp(&state, user.user_id, code).await? {
            return Err(refuse_step_up(&state, &user, "totp").await);
        }
        StepUpMethod::Totp
    } else if let Some(code) = body.recovery_code.as_ref() {
        if !crate::mfa::verify_recovery_code(&state, user.user_id, code).await? {
            return Err(refuse_step_up(&state, &user, "recovery_code").await);
        }
        StepUpMethod::RecoveryCode
    } else {
        let password = body.password.as_ref().ok_or(ApiError::Internal)?;
        // Refused *before* the hash is checked: an enrolled account must not be able to elevate
        // with the password at all, and verifying first would make the refusal depend on
        // whether the attacker guessed right.
        if user.mfa_enrolled {
            return Err(refuse_step_up(&state, &user, "password_but_enrolled").await);
        }
        let account = tankovault_db::repo::users::get(&state.pool, user.user_id).await?;
        let credentials =
            tankovault_db::repo::users::find_credentials(&state.pool, &account.username)
                .await?
                .ok_or(ApiError::Unauthorized)?;
        let ok = tankovault_auth::verify_password(
            password,
            &credentials.password_hash,
            &state.password_pepper,
        )
        .map_err(|_| ApiError::Internal)?;
        if !ok {
            return Err(refuse_step_up(&state, &user, "password").await);
        }
        StepUpMethod::Password
    };

    let grant = crate::mfa::issue_step_up(&state, user.user_id, method).await?;
    audit(
        &state,
        &user,
        "auth.step_up",
        &user.user_id.as_uuid().to_string(),
        &serde_json::json!({ "method": method.as_str() }),
    )
    .await;
    Ok(Json(grant))
}

/// Audit a refused elevation and answer `401`.
///
/// Every refusal is recorded, not just the interesting ones: a run of failed step-ups against
/// one account is the signal that somebody holds a session they should not, and an audit trail
/// that only logs successes cannot show it.
async fn refuse_step_up(state: &AppState, user: &AuthUser, factor: &str) -> ApiError {
    audit_failure(
        state,
        user,
        "auth.step_up",
        &user.user_id.as_uuid().to_string(),
        &serde_json::json!({ "factor": factor }),
    )
    .await;
    ApiError::Unauthorized
}

/// Begin a security-key step-up
///
/// Issues an assertion challenge for the caller's registered security keys.
#[utoipa::path(
    post,
    path = "/v1/me/step-up/security-key/start",
    tag = ME_ACCOUNT_TAG,
    security(("bearer_auth" = [])),
    responses(
        (status = 200, description = "Challenge issued", body = SecurityKeyChallenge),
        (status = 400, description = "No security key is registered", body = crate::error::ProblemDetails),
        (status = 401, description = "authentication required", body = crate::error::ProblemDetails),
        (status = 503, description = "this deployment has no WebAuthn relying party configured", body = crate::error::ProblemDetails),
    )
)]
pub async fn step_up_security_key_start(
    State(state): State<AppState>,
    user: AuthUser,
) -> ApiResult<Json<SecurityKeyChallenge>> {
    Ok(Json(
        crate::mfa::begin_security_key_assertion(&state, user.user_id).await?,
    ))
}

/// Finish a security-key step-up
#[utoipa::path(
    post,
    path = "/v1/me/step-up/security-key/finish",
    tag = ME_ACCOUNT_TAG,
    request_body = SecurityKeyAssertion,
    security(("bearer_auth" = [])),
    responses(
        (status = 200, description = "Elevated", body = StepUpGrant),
        (status = 401, description = "authentication required, or verification failed", body = crate::error::ProblemDetails),
        (status = 503, description = "this deployment has no WebAuthn relying party configured", body = crate::error::ProblemDetails),
    )
)]
pub async fn step_up_security_key_finish(
    State(state): State<AppState>,
    user: AuthUser,
    Json(body): Json<SecurityKeyAssertion>,
) -> ApiResult<Json<StepUpGrant>> {
    if let Err(e) = crate::mfa::finish_security_key_assertion(&state, user.user_id, &body).await {
        audit_failure(
            &state,
            &user,
            "auth.step_up",
            &user.user_id.as_uuid().to_string(),
            &serde_json::json!({ "factor": "security_key" }),
        )
        .await;
        return Err(e);
    }

    let grant = crate::mfa::issue_step_up(&state, user.user_id, StepUpMethod::SecurityKey).await?;
    audit(
        &state,
        &user,
        "auth.step_up",
        &user.user_id.as_uuid().to_string(),
        &serde_json::json!({ "method": StepUpMethod::SecurityKey.as_str() }),
    )
    .await;
    Ok(Json(grant))
}

/// Trim a label, apply the default, and bound its length.
///
/// # Errors
/// [`ApiError::BadRequest`] when the label exceeds [`MAX_LABEL_LEN`] characters.
fn normalise_label(raw: Option<&str>) -> ApiResult<String> {
    let trimmed = raw.unwrap_or("").trim();
    let label = if trimmed.is_empty() {
        DEFAULT_LABEL
    } else {
        trimmed
    };
    // Characters, not bytes: the limit bounds what a list renders, and a user naming a key in
    // Japanese should not hit it three times sooner than one naming it in English.
    if label.chars().count() > MAX_LABEL_LEN {
        return Err(ApiError::BadRequest(format!(
            "label must be at most {MAX_LABEL_LEN} characters"
        )));
    }
    Ok(label.to_owned())
}

#[cfg(test)]
mod tests {
    use super::{DEFAULT_LABEL, MAX_LABEL_LEN, normalise_label};

    #[test]
    fn a_blank_label_becomes_the_default_and_a_long_one_is_refused() {
        assert_eq!(normalise_label(None).unwrap(), DEFAULT_LABEL);
        assert_eq!(normalise_label(Some("   ")).unwrap(), DEFAULT_LABEL);
        assert_eq!(normalise_label(Some(" YubiKey 5C ")).unwrap(), "YubiKey 5C");
        assert!(normalise_label(Some(&"x".repeat(MAX_LABEL_LEN + 1))).is_err());
    }

    /// The limit counts characters, not bytes.
    ///
    /// The bug this pins: a byte limit rejects a label a third of the length for anyone writing
    /// in a non-Latin script, and does so with a message quoting a number they can count to and
    /// see they are under.
    #[test]
    fn the_label_limit_counts_characters() {
        let multibyte = "鍵".repeat(MAX_LABEL_LEN);
        assert!(
            multibyte.len() > MAX_LABEL_LEN,
            "the fixture must be multi-byte"
        );
        assert!(normalise_label(Some(&multibyte)).is_ok());
    }
}
