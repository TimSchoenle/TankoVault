//! GDPR data-subject endpoints: portability (Art. 20), erasure (Art. 17), and a request queue
//! for rights needing human judgment (rectification, restriction, objection).
//!
//! Every handler acts only on the authenticated principal; administrator-initiated actions go
//! through [`crate::admin::privacy`] instead.

use crate::audit::{audit, audit_failure};
use crate::error::{ApiError, ApiResult};
use crate::openapi::ME_ACCOUNT_TAG;
use crate::state::{AppState, AuthUser};
use crate::step_up::Elevated;
use crate::views::{IntoStored, IntoView};
use axum::Json;
use axum::extract::{Path, State};
use axum::http::{HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use serde::Deserialize;
use tankovault_contracts::me::{PrivacyRequestKind, PrivacyRequestView};
use tankovault_db::repo::gdpr::RequestStatus;
use utoipa::ToSchema;
use uuid::Uuid;

/// Export my data
///
/// Everything the system holds about the authenticated user, as a single JSON document
/// (GDPR Art. 20, right to data portability).
///
/// Served as an attachment rather than an inline body: the response is the user's entire
/// personal record and a browser should offer to save it rather than render it.
/// Credentials — password hash, session token hashes, third-party OAuth tokens, the TOTP
/// secret — are excluded; see `tankovault_db::repo::privacy::export_user_data`.
///
/// Behind a step-up. This one response is every reading habit, every linked account and every
/// address the system holds about one person, assembled into a file designed to be saved and
/// mailed. It is the single highest-value thing a stolen session can ask for.
#[utoipa::path(
    get,
    path = "/v1/me/export",
    tag = ME_ACCOUNT_TAG,
    security(("bearer_auth" = [])),
    responses(
        (status = 200, description = "Complete personal-data export", body = serde_json::Value),
        (status = 401, description = "Unauthenticated"),
        (status = 403, description = "a step-up is required"),
        (status = 404, description = "self-service export is switched off; file an access request instead"),
    )
)]
pub async fn export_data(
    State(state): State<AppState>,
    Elevated(user): Elevated,
) -> ApiResult<Response> {
    let export = tankovault_db::repo::privacy::export_user_data(&state.pool, user.user_id).await?;

    // The export is itself the highest-value artefact this system produces, so it's audited too.
    audit(
        &state,
        &user,
        "account.export",
        &user.user_id.as_uuid().to_string(),
        &serde_json::json!({ "route": "self_service" }),
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
///
/// Any of the user's own open data-subject requests are resolved as completed *before* the
/// cascade, so the compliance record shows the erasure was carried out rather than leaving a
/// request that appears abandoned.
#[utoipa::path(
    delete,
    path = "/v1/me",
    tag = ME_ACCOUNT_TAG,
    security(("bearer_auth" = [])),
    request_body = DeleteAccount,
    responses(
        (status = 204, description = "Account and all owned data erased"),
        (status = 400, description = "Confirmation did not match"),
        (status = 401, description = "Unauthenticated"),
        (status = 403, description = "a step-up is required"),
        (status = 404, description = "self-service deletion is switched off; file an erasure request instead"),
    )
)]
pub async fn delete_account(
    State(state): State<AppState>,
    // Behind a step-up *in addition to* the typed-username confirmation below. The confirmation
    // guards against a misclick; it is not a credential, and on its own it left the single
    // irreversible action on the account reachable by anyone holding a token.
    Elevated(user): Elevated,
    Json(req): Json<DeleteAccount>,
) -> ApiResult<StatusCode> {
    let account = tankovault_db::repo::users::get(&state.pool, user.user_id).await?;

    if req.confirm_username.trim() != account.username {
        // Repeated failed deletion attempts are a signal worth auditing.
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

    // Record before deleting: afterwards the actor no longer exists and `actor_id` would be
    // rejected or nulled, losing the record of why the account is gone.
    audit(
        &state,
        &user,
        "account.delete",
        &user.user_id.as_uuid().to_string(),
        &serde_json::json!({ "username": account.username, "route": "self_service" }),
    )
    .await;

    close_open_requests_for_self_erasure(&state, user.user_id).await;

    tankovault_db::repo::privacy::erase_user(&state.pool, user.user_id).await?;
    tracing::info!(user_id = %user.user_id.as_uuid(), "account erased on user request");

    Ok(StatusCode::NO_CONTENT)
}

/// Resolve the user's open requests as completed, immediately before their account is erased.
///
/// Best-effort: a failure here must not block someone exercising Art. 17.
async fn close_open_requests_for_self_erasure(
    state: &AppState,
    user_id: tankovault_domain::UserId,
) {
    let open = match tankovault_db::repo::gdpr::list_for_user(&state.pool, user_id).await {
        Ok(rows) => rows,
        Err(e) => {
            tracing::warn!(error = %e, "could not read privacy requests before erasure");
            return;
        }
    };
    for request in open.iter().filter(|r| r.status.is_open()) {
        if let Err(e) = tankovault_db::repo::gdpr::resolve(
            &state.pool,
            request.id,
            RequestStatus::Completed,
            None,
            Some("closed automatically: the subject erased their own account"),
        )
        .await
        {
            tracing::warn!(error = %e, request_id = %request.id, "could not close a privacy request");
        }
    }
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct NewPrivacyRequest {
    /// Which right is being exercised.
    pub kind: PrivacyRequestKind,
    /// What the subject is asking for, in their words. Required for rectification — "correct
    /// my data" without saying which field to what is not actionable — and optional otherwise.
    #[serde(default)]
    pub detail: Option<String>,
}

/// File a data-subject request
///
/// Opens a tracked request against the authenticated account and returns it with its Art. 12(3)
/// deadline, which is the one thing the subject is entitled to know up front.
///
/// One open request per kind: a second would start its own clock and show the queue two
/// deadlines for one obligation.
#[utoipa::path(
    post,
    path = "/v1/me/privacy/requests",
    tag = ME_ACCOUNT_TAG,
    request_body = NewPrivacyRequest,
    security(("bearer_auth" = [])),
    responses(
        (status = 201, description = "The filed request, with its response deadline", body = PrivacyRequestView),
        (status = 400, description = "rectification without detail, or a duplicate open request", body = crate::error::ProblemDetails),
        (status = 401, description = "authentication required", body = crate::error::ProblemDetails),
        (status = 403, description = "a step-up is required", body = crate::error::ProblemDetails),
        (status = 404, description = "the data-subject request queue is switched off", body = crate::error::ProblemDetails),
    )
)]
pub async fn create_privacy_request(
    State(state): State<AppState>,
    // A filed access request ends with an operator handing the caller's whole record to
    // whatever address the account carries, and an erasure request ends with the account gone.
    // Both are the export and the deletion above, taking a slower route.
    Elevated(user): Elevated,
    Json(body): Json<NewPrivacyRequest>,
) -> ApiResult<(StatusCode, Json<PrivacyRequestView>)> {
    let detail = body
        .detail
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty());
    if let Some(detail) = detail {
        crate::auth::validate_free_text("detail", detail)?;
    }

    if body.kind == PrivacyRequestKind::Rectification && detail.is_none() {
        return Err(ApiError::BadRequest(
            "say which data is wrong and what it should be".to_owned(),
        ));
    }

    if tankovault_db::repo::gdpr::has_open_of_kind(
        &state.pool,
        user.user_id,
        body.kind.into_stored(),
    )
    .await?
    {
        return Err(ApiError::BadRequest(
            "you already have an open request of this kind".to_owned(),
        ));
    }

    let request = tankovault_db::repo::gdpr::create(
        &state.pool,
        user.user_id,
        body.kind.into_stored(),
        detail,
    )
    .await?;

    audit(
        &state,
        &user,
        "privacy.request.create",
        &request.id.to_string(),
        &serde_json::json!({ "kind": body.kind }),
    )
    .await;

    Ok((StatusCode::CREATED, Json(request.into_view())))
}

/// List my data-subject requests
///
/// The authenticated account's own requests, newest first, including resolved ones — the
/// subject is entitled to see how their requests were handled, not only which are outstanding.
#[utoipa::path(
    get,
    path = "/v1/me/privacy/requests",
    tag = ME_ACCOUNT_TAG,
    security(("bearer_auth" = [])),
    responses(
        (status = 200, description = "The caller's requests", body = Vec<PrivacyRequestView>),
        (status = 401, description = "authentication required", body = crate::error::ProblemDetails),
        (status = 404, description = "the data-subject request queue is switched off", body = crate::error::ProblemDetails),
    )
)]
pub async fn list_privacy_requests(
    State(state): State<AppState>,
    user: AuthUser,
) -> ApiResult<Json<Vec<PrivacyRequestView>>> {
    let rows = tankovault_db::repo::gdpr::list_for_user(&state.pool, user.user_id).await?;
    Ok(Json(rows.into_view()))
}

/// Withdraw a data-subject request
///
/// Cancels one of the caller's own open requests. Scoped to ownership, so the id alone is not
/// authority to cancel someone else's, and refused once resolved — a request that has already
/// been answered is a compliance record, not a draft.
#[utoipa::path(
    delete,
    path = "/v1/me/privacy/requests/{id}",
    tag = ME_ACCOUNT_TAG,
    params(("id" = Uuid, Path, description = "Request id")),
    security(("bearer_auth" = [])),
    responses(
        (status = 204, description = "Withdrawn"),
        (status = 401, description = "authentication required", body = crate::error::ProblemDetails),
        (status = 403, description = "a step-up is required", body = crate::error::ProblemDetails),
        (status = 404, description = "no such open request for this caller", body = crate::error::ProblemDetails),
    )
)]
pub async fn cancel_privacy_request(
    State(state): State<AppState>,
    // Withdrawing is how an attacker would silence the rectification request their victim filed
    // about the change they made.
    Elevated(user): Elevated,
    Path(id): Path<Uuid>,
) -> ApiResult<StatusCode> {
    let cancelled = tankovault_db::repo::gdpr::cancel_own(&state.pool, id, user.user_id).await?;
    if !cancelled {
        return Err(ApiError::NotFound);
    }
    audit(
        &state,
        &user,
        "privacy.request.cancel",
        &id.to_string(),
        &serde_json::json!({}),
    )
    .await;
    Ok(StatusCode::NO_CONTENT)
}
