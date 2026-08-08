//! Managing one's own passkeys: register, list, rename, revoke; sign-in lives in
//! [`crate::auth::passkey`].
//!
//! Its one non-obvious rule is the password check on registration — see
//! [`passkey_register_start`].

use axum::Json;
use axum::extract::{Path, State};
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;
use utoipa::ToSchema;
use uuid::Uuid;
use webauthn_rs::prelude::{Passkey, PasskeyRegistration, RegisterPublicKeyCredential};

use crate::audit::{audit, audit_failure};
use crate::error::{ApiError, ApiResult};
use crate::openapi::ME_ACCOUNT_TAG;
use crate::passkey::{
    begin_ceremony, ceremony_start_failed, relying_party, take_ceremony, verification_failed,
};
use crate::state::{AppState, AuthUser};
use crate::step_up::Elevated;
use tankovault_db::repo::users::webauthn::{CeremonyKind, CredentialPurpose};

/// Longest accepted passkey label, in characters; bounds the row and the list view only.
const MAX_LABEL_LEN: usize = 64;

/// The label applied when the client sends none.
const DEFAULT_LABEL: &str = "Passkey";

/// A registered passkey as the account page shows it.
///
/// Note what is **not** here: the credential itself, the public key and the credential id. None
/// of them is a secret, but none is useful to a browser either, and a credential id is the
/// lookup key a sign-in resolves an account from — publishing every account's ids on an
/// authenticated page would hand an attacker who compromises one session the material to
/// recognise that user's authenticator elsewhere.
#[derive(Debug, Serialize, ToSchema)]
pub struct PasskeyDto {
    pub id: Uuid,
    pub label: String,
    #[serde(with = "time::serde::rfc3339")]
    #[schema(value_type = String)]
    pub created_at: OffsetDateTime,
    /// When this key was last used to sign in; absent if it never has been. Surfaced so a user
    /// can tell a live key from one registered on a laptop they no longer own.
    #[serde(
        with = "time::serde::rfc3339::option",
        skip_serializing_if = "Option::is_none"
    )]
    #[schema(value_type = Option<String>)]
    pub last_used_at: Option<OffsetDateTime>,
}

/// What to call the new key.
#[derive(Debug, Deserialize, ToSchema)]
pub struct PasskeyRegisterStart {
    /// What to call the key in the revoke list. Defaults to "Passkey" when absent or blank.
    #[serde(default)]
    pub label: Option<String>,
}

/// The challenge, plus the handle the client echoes back to complete it.
///
/// `options` is a W3C `PublicKeyCredentialCreationOptions` envelope. It is an opaque JSON
/// document here for the reason [`crate::auth::passkey::PasskeyChallenge`] gives: the shape
/// belongs to the `WebAuthn` specification, and both ends parse it with the same crate.
#[derive(Debug, Serialize, ToSchema)]
pub struct PasskeyRegisterChallenge {
    pub ceremony_id: Uuid,
    /// A W3C `PublicKeyCredentialCreationOptions` envelope. Hand it to the browser unmodified.
    #[schema(value_type = Object)]
    pub options: serde_json::Value,
}

/// The attestation returned by `navigator.credentials.create()`.
#[derive(Debug, Deserialize, ToSchema)]
pub struct PasskeyRegisterFinish {
    pub ceremony_id: Uuid,
    /// The `RegisterPublicKeyCredential` the browser produced, serialised as the `WebAuthn`
    /// specification defines it.
    #[schema(value_type = Object)]
    pub credential: serde_json::Value,
}

/// A new name for an existing key.
#[derive(Debug, Deserialize, ToSchema)]
pub struct PasskeyRename {
    pub label: String,
}

/// List my passkeys
///
/// Every passkey registered to the caller, newest first.
#[utoipa::path(
    get,
    path = "/v1/me/passkeys",
    tag = ME_ACCOUNT_TAG,
    security(("bearer_auth" = [])),
    responses(
        (status = 200, description = "Registered passkeys", body = Vec<PasskeyDto>),
        (status = 401, description = "authentication required", body = crate::error::ProblemDetails),
    )
)]
pub async fn list_passkeys(
    State(state): State<AppState>,
    user: AuthUser,
) -> ApiResult<Json<Vec<PasskeyDto>>> {
    let rows = tankovault_db::repo::users::webauthn::list_for_user(
        &state.pool,
        user.user_id,
        CredentialPurpose::Passkey,
    )
    .await?;
    Ok(Json(
        rows.into_iter()
            .map(|r| PasskeyDto {
                id: r.id,
                label: r.label,
                created_at: r.created_at,
                last_used_at: r.last_used_at,
            })
            .collect(),
    ))
}

/// Begin registering a passkey
///
/// Issues a `WebAuthn` creation challenge for the caller's account.
///
/// # Two gates, and why a passkey has the strictest one on the account
///
/// A passkey is a **permanent** credential, and an access token is a 15-minute one. Anyone who
/// got hold of a token for those fifteen minutes — a shared browser, a proxy log, an XSS
/// payload that survives one page — could otherwise register their own authenticator and keep
/// the account forever, surviving every password change and session revocation the real owner
/// performs. So installing a credential requires proving you hold one:
///
/// 1. **A second factor must already be enrolled.** Not merely presented — enrolled. A passkey
///    signs in on its own, with no second leg, so minting one is the act of creating a
///    single-factor bypass of everything below it. An account that has not yet decided to hold
///    a second factor has not earned the right to create something that outranks it.
/// 2. **A step-up.** The factor must be presented *now*, not merely exist.
///
/// This used to take `current_password` instead, which defended against the wrong attacker: the
/// person most likely to be holding a stolen token is the person who phished the password to
/// open it.
///
/// Credentials already registered to the account — passkeys **and** security keys — are sent as
/// the exclusion list, so an authenticator that already holds one for this account says so at
/// the prompt instead of silently minting a second credential that would let one touch satisfy
/// both legs of a sign-in.
#[utoipa::path(
    post,
    path = "/v1/me/passkeys/register/start",
    tag = ME_ACCOUNT_TAG,
    request_body = PasskeyRegisterStart,
    security(("bearer_auth" = [])),
    responses(
        (status = 200, description = "Challenge issued", body = PasskeyRegisterChallenge),
        (status = 400, description = "The label is too long", body = crate::error::ProblemDetails),
        (status = 401, description = "authentication required", body = crate::error::ProblemDetails),
        (status = 403, description = "a step-up is required, or no second factor is enrolled", body = crate::error::ProblemDetails),
        (status = 503, description = "This deployment has no WebAuthn relying party configured", body = crate::error::ProblemDetails),
    )
)]
pub async fn passkey_register_start(
    State(state): State<AppState>,
    Elevated(user): Elevated,
    Json(body): Json<PasskeyRegisterStart>,
) -> ApiResult<Json<PasskeyRegisterChallenge>> {
    let webauthn = relying_party(&state)?;
    let label = normalise_label(body.label.as_deref())?;

    // Gate 1. `Elevated` already proved a factor was *presented*, which for an unenrolled
    // account can be satisfied by the password fallback — so the enrolment itself is checked
    // separately here rather than being assumed from the elevation.
    if !user.mfa_enrolled {
        audit_failure(
            &state,
            &user,
            "me.passkey_register",
            &user.user_id.as_uuid().to_string(),
            &serde_json::json!({ "reason": "mfa_not_enrolled" }),
        )
        .await;
        return Err(ApiError::MfaEnrolmentRequired);
    }

    let account = tankovault_db::repo::users::get(&state.pool, user.user_id).await?;

    let existing =
        tankovault_db::repo::users::webauthn::credential_ids_for_user(&state.pool, user.user_id)
            .await?;

    // `user_unique_id` is the account's own id: the stable, one-row-per-account handle the
    // authenticator stores. Not a secret leaked to the device — the owner already reads it via
    // `GET /v1/me/profile`, and it travels nowhere else.
    let (challenge, ceremony) = webauthn
        .start_passkey_registration(
            account.id.as_uuid(),
            &account.username,
            &account.username,
            Some(existing),
        )
        .map_err(|e| ceremony_start_failed(&e))?;

    // The label is decided now and travels with the ceremony rather than being resent at
    // `finish`, so the two requests can't disagree on what it was.
    let ceremony_id = begin_ceremony(
        &state,
        Some(user.user_id),
        CeremonyKind::Register,
        &StoredRegistration {
            registration: ceremony,
            label,
        },
    )
    .await?;

    Ok(Json(PasskeyRegisterChallenge {
        ceremony_id,
        options: serde_json::to_value(&challenge).map_err(|e| {
            tracing::error!(error = %e, "could not serialise a webauthn challenge");
            ApiError::Internal
        })?,
    }))
}

/// The registration ceremony as stored: the library's state plus the label chosen alongside it.
#[derive(serde::Serialize, serde::Deserialize)]
struct StoredRegistration {
    registration: PasskeyRegistration,
    label: String,
}

/// Finish registering a passkey
///
/// Verifies the attestation against the challenge issued by [`passkey_register_start`] and
/// stores the credential.
#[utoipa::path(
    post,
    path = "/v1/me/passkeys/register/finish",
    tag = ME_ACCOUNT_TAG,
    request_body = PasskeyRegisterFinish,
    security(("bearer_auth" = [])),
    responses(
        (status = 201, description = "Registered", body = PasskeyDto),
        (status = 400, description = "Malformed credential", body = crate::error::ProblemDetails),
        (status = 401, description = "authentication required, or the challenge is no longer live", body = crate::error::ProblemDetails),
        (status = 409, description = "This authenticator is already registered", body = crate::error::ProblemDetails),
        (status = 503, description = "This deployment has no WebAuthn relying party configured", body = crate::error::ProblemDetails),
    )
)]
pub async fn passkey_register_finish(
    State(state): State<AppState>,
    user: AuthUser,
    Json(body): Json<PasskeyRegisterFinish>,
) -> ApiResult<(axum::http::StatusCode, Json<PasskeyDto>)> {
    let webauthn = relying_party(&state)?;

    let credential: RegisterPublicKeyCredential = serde_json::from_value(body.credential)
        .map_err(|e| ApiError::BadRequest(format!("malformed WebAuthn credential: {e}")))?;

    let (owner, stored): (_, StoredRegistration) =
        take_ceremony(&state, body.ceremony_id, CeremonyKind::Register).await?;

    // A registration ceremony belongs to the account that started it — otherwise a leaked
    // ceremony id lets an attacker install their own authenticator on someone else's account.
    // `Unauthorized` rather than `Forbidden` so the caller isn't told the ceremony exists.
    if owner != Some(user.user_id) {
        tracing::warn!(
            caller = %user.user_id.as_uuid(),
            "a passkey registration ceremony was completed by an account that did not start it"
        );
        return Err(ApiError::Unauthorized);
    }

    let passkey: Passkey = webauthn
        .finish_passkey_registration(&credential, &stored.registration)
        .map_err(|e| verification_failed(&e))?;

    let serialised = serde_json::to_value(&passkey).map_err(|e| {
        tracing::error!(error = %e, "could not serialise a verified passkey");
        ApiError::Internal
    })?;

    // A `409` here is the global uniqueness constraint firing on an already-registered
    // credential id; see `0022_passkeys.up.sql` for why that's a conflict, not an upsert.
    let record = tankovault_db::repo::users::webauthn::insert(
        &state.pool,
        user.user_id,
        passkey.cred_id(),
        &serialised,
        &stored.label,
        CredentialPurpose::Passkey,
    )
    .await?;

    audit(
        &state,
        &user,
        "me.passkey_register",
        &record.id.to_string(),
        &serde_json::json!({ "label": record.label }),
    )
    .await;

    Ok((
        axum::http::StatusCode::CREATED,
        Json(PasskeyDto {
            id: record.id,
            label: record.label,
            created_at: record.created_at,
            last_used_at: record.last_used_at,
        }),
    ))
}

/// Rename a passkey
///
/// Change the label on one of the caller's own passkeys. Scoped to ownership; a foreign or
/// unknown id yields `404`.
#[utoipa::path(
    patch,
    path = "/v1/me/passkeys/{id}",
    tag = ME_ACCOUNT_TAG,
    params(("id" = Uuid, Path, description = "Passkey id")),
    request_body = PasskeyRename,
    security(("bearer_auth" = [])),
    responses(
        (status = 204, description = "Renamed"),
        (status = 400, description = "The label is too long", body = crate::error::ProblemDetails),
        (status = 401, description = "authentication required", body = crate::error::ProblemDetails),
        (status = 404, description = "No such passkey for this caller", body = crate::error::ProblemDetails),
    )
)]
pub async fn rename_passkey(
    State(state): State<AppState>,
    user: AuthUser,
    Path(id): Path<Uuid>,
    Json(body): Json<PasskeyRename>,
) -> ApiResult<axum::http::StatusCode> {
    let label = normalise_label(Some(&body.label))?;
    let changed = tankovault_db::repo::users::webauthn::rename(
        &state.pool,
        user.user_id,
        id,
        &label,
        CredentialPurpose::Passkey,
    )
    .await?;
    if changed == 0 {
        return Err(ApiError::NotFound);
    }
    Ok(axum::http::StatusCode::NO_CONTENT)
}

/// Revoke a passkey
///
/// Delete one of the caller's own passkeys. Scoped to ownership; a foreign or unknown id yields
/// `404`.
///
/// Deleting the last passkey is allowed, and is not a lockout: every account on this deployment
/// has a password, and passkeys are additive to it. Refusing would be the wrong protection
/// anyway — the case a user most urgently needs this for is a device they have just lost.
#[utoipa::path(
    delete,
    path = "/v1/me/passkeys/{id}",
    tag = ME_ACCOUNT_TAG,
    params(("id" = Uuid, Path, description = "Passkey id")),
    security(("bearer_auth" = [])),
    responses(
        (status = 204, description = "Revoked"),
        (status = 401, description = "authentication required", body = crate::error::ProblemDetails),
        (status = 404, description = "No such passkey for this caller", body = crate::error::ProblemDetails),
    )
)]
pub async fn delete_passkey(
    State(state): State<AppState>,
    user: AuthUser,
    Path(id): Path<Uuid>,
) -> ApiResult<axum::http::StatusCode> {
    let removed = tankovault_db::repo::users::webauthn::delete(
        &state.pool,
        user.user_id,
        id,
        CredentialPurpose::Passkey,
    )
    .await?;
    if removed == 0 {
        return Err(ApiError::NotFound);
    }
    audit(
        &state,
        &user,
        "me.passkey_revoke",
        &id.to_string(),
        &serde_json::json!({}),
    )
    .await;
    Ok(axum::http::StatusCode::NO_CONTENT)
}

/// Trim a caller-supplied label, substitute the default for an empty one, and bound its length.
///
/// # Errors
/// [`ApiError::BadRequest`] when the label exceeds [`MAX_LABEL_LEN`] **characters** (not bytes —
/// truncating a multi-byte label at a byte boundary would be wrong).
fn normalise_label(label: Option<&str>) -> ApiResult<String> {
    let label = label.unwrap_or("").trim();
    if label.chars().count() > MAX_LABEL_LEN {
        return Err(ApiError::BadRequest(format!(
            "label must be at most {MAX_LABEL_LEN} characters"
        )));
    }
    Ok(if label.is_empty() {
        DEFAULT_LABEL.to_owned()
    } else {
        label.to_owned()
    })
}

#[cfg(test)]
mod tests {
    use super::{DEFAULT_LABEL, MAX_LABEL_LEN, normalise_label};

    #[test]
    fn a_blank_label_becomes_the_default() {
        assert_eq!(
            normalise_label(None).expect("no label is fine"),
            DEFAULT_LABEL
        );
        assert_eq!(
            normalise_label(Some("   ")).expect("whitespace is fine"),
            DEFAULT_LABEL
        );
    }

    #[test]
    fn a_label_is_trimmed() {
        assert_eq!(
            normalise_label(Some("  Work laptop  ")).expect("trims"),
            "Work laptop"
        );
    }

    /// The limit counts characters, not bytes.
    ///
    /// A byte limit would reject a perfectly ordinary Japanese or emoji label at a third of the
    /// length an ASCII one is allowed, which is the kind of rule that looks like a bug to
    /// everyone who hits it and like a working validator to everyone who does not.
    #[test]
    fn the_length_limit_counts_characters() {
        let full_width = "鍵".repeat(MAX_LABEL_LEN);
        assert!(
            normalise_label(Some(&full_width)).is_ok(),
            "{MAX_LABEL_LEN} characters must be accepted however many bytes they take"
        );
        let too_long = "a".repeat(MAX_LABEL_LEN + 1);
        assert!(normalise_label(Some(&too_long)).is_err());
    }
}
