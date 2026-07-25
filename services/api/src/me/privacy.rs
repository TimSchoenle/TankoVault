//! GDPR data-subject endpoints: portability (Art. 20) and erasure (Art. 17).
//!
//! Both act only on the authenticated principal — there is no path here by which one user
//! reaches another's data, and no operator override. An admin acting on someone else's
//! account would be a different endpoint with a different audit action; conflating them
//! would leave the audit trail unable to answer "who asked for this export?".
//!
//! Both are classified `RouteClass::Expensive` by the rate limiter (see
//! [`crate::route_classifier`]): an export assembles a dozen table scans, and an erasure
//! cascades across every user-owned table.

use crate::audit::{audit, audit_failure};
use crate::error::{ApiError, ApiResult};
use crate::openapi::ME_ACCOUNT_TAG;
use crate::state::{AppState, AuthUser};
use axum::Json;
use axum::extract::State;
use axum::http::{HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use serde::Deserialize;
use utoipa::ToSchema;

/// Export my data
///
/// Everything the system holds about the authenticated user, as a single JSON document
/// (GDPR Art. 20, right to data portability).
///
/// Served as an attachment rather than an inline body: the response is the user's entire
/// personal record and a browser should offer to save it rather than render it.
/// Credentials — password hash, session token hashes, third-party OAuth tokens — are
/// excluded; see `tankovault_db::repo::privacy::export_user_data`.
#[utoipa::path(
    get,
    path = "/v1/me/export",
    tag = ME_ACCOUNT_TAG,
    security(("bearer" = [])),
    responses(
        (status = 200, description = "Complete personal-data export", body = serde_json::Value),
        (status = 401, description = "Unauthenticated"),
    )
)]
pub async fn export_data(State(state): State<AppState>, user: AuthUser) -> ApiResult<Response> {
    let export = tankovault_db::repo::privacy::export_user_data(&state.pool, user.user_id).await?;

    // Auditing the export is itself a privacy control: it is the highest-value artefact
    // this system produces, and an unexplained one is worth investigating.
    audit(
        &state,
        &user,
        "account.export",
        &user.user_id.as_uuid().to_string(),
        &serde_json::json!({}),
    )
    .await;

    let filename = format!("tankovault-export-{}.json", user.user_id.as_uuid());
    let mut response = Json(export).into_response();
    if let Ok(value) = HeaderValue::from_str(&format!("attachment; filename=\"{filename}\"")) {
        response
            .headers_mut()
            .insert(header::CONTENT_DISPOSITION, value);
    }
    Ok(response)
}

/// Confirmation payload for account deletion.
///
/// Deletion is irreversible and cascades across every table, so it demands an explicit,
/// unambiguous intent rather than being reachable by a stray `DELETE`. Requiring the
/// username back defends against both a mis-clicked button and a forced cross-site
/// navigation, neither of which can supply it.
#[derive(Debug, Deserialize, ToSchema)]
pub struct DeleteAccount {
    /// The authenticated user's own username, typed back as confirmation.
    pub confirm_username: String,
}

/// Delete my account
///
/// Erases the account and every row owned by it (GDPR Art. 17, right to erasure).
///
/// Audit records of privileged actions the user performed are **retained in pseudonymised
/// form**: the actor reference is nulled, so the record of what happened survives while
/// the link to a person does not. Documented on
/// `tankovault_db::repo::privacy::erase_user`.
#[utoipa::path(
    delete,
    path = "/v1/me",
    tag = ME_ACCOUNT_TAG,
    security(("bearer" = [])),
    request_body = DeleteAccount,
    responses(
        (status = 204, description = "Account and all owned data erased"),
        (status = 400, description = "Confirmation did not match"),
        (status = 401, description = "Unauthenticated"),
    )
)]
pub async fn delete_account(
    State(state): State<AppState>,
    user: AuthUser,
    Json(req): Json<DeleteAccount>,
) -> ApiResult<StatusCode> {
    let account = tankovault_db::repo::users::get(&state.pool, user.user_id).await?;

    if req.confirm_username.trim() != account.username {
        // Audit the refusal: repeated failed deletion attempts on an account are a signal
        // worth having, and this is the one branch where the user's intent is in doubt.
        audit_failure(
            &state,
            &user,
            "account.delete",
            &user.user_id.as_uuid().to_string(),
            &serde_json::json!({ "reason": "confirmation_mismatch" }),
        )
        .await;
        return Err(ApiError::BadRequest(
            "confirmation did not match your username".to_owned(),
        ));
    }

    // Record *before* deleting. Afterwards the actor no longer exists and the insert's
    // `actor_id` would be rejected or nulled — losing the one record that explains why
    // the account is gone.
    audit(
        &state,
        &user,
        "account.delete",
        &user.user_id.as_uuid().to_string(),
        &serde_json::json!({ "username": account.username }),
    )
    .await;

    tankovault_db::repo::privacy::erase_user(&state.pool, user.user_id).await?;
    tracing::info!(user_id = %user.user_id.as_uuid(), "account erased on user request");

    Ok(StatusCode::NO_CONTENT)
}
