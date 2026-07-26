//! GDPR data-subject endpoints: portability (Art. 20), erasure (Art. 17), and the request
//! queue that covers everything those two cannot.
//!
//! Every handler here acts only on the authenticated principal — there is no path by which one
//! user reaches another's data, and no operator override. An administrator acting on someone
//! else's behalf uses [`crate::admin::privacy`], which has different permissions and a
//! different audit action; conflating them would leave the trail unable to answer "who asked
//! for this export?".
//!
//! The export and erasure endpoints are classified `RouteClass::Expensive` by the rate limiter
//! (see [`crate::route_classifier`]): an export assembles a dozen table scans, and an erasure
//! cascades across every user-owned table.
//!
//! # Why a request queue exists next to endpoints that already work
//!
//! Self-service export and erasure satisfy the two rights people actually exercise, instantly.
//! They cannot satisfy the rest of Chapter III: rectification, restriction and objection are
//! decisions a human has to make; Art. 12(3) imposes a one-month deadline, which needs a
//! tracked object with a due date rather than a call that either happened or did not; and
//! Art. 5(2) requires the controller to be able to *demonstrate* it responded. When self-service
//! erasure is switched off, the queue is also what preserves the right — the request is still
//! accepted, it just becomes mediated.

use crate::audit::{audit, audit_failure};
use crate::error::{ApiError, ApiResult};
use crate::openapi::ME_ACCOUNT_TAG;
use crate::state::{AppState, AuthUser};
use axum::Json;
use axum::extract::{Path, State};
use axum::http::{HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use serde::Deserialize;
use tankovault_db::repo::gdpr::{RequestKind, RequestRow, RequestStatus};
use utoipa::ToSchema;
use uuid::Uuid;

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
        (status = 404, description = "self-service export is switched off; file an access request instead"),
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
    security(("bearer" = [])),
    request_body = DeleteAccount,
    responses(
        (status = 204, description = "Account and all owned data erased"),
        (status = 400, description = "Confirmation did not match"),
        (status = 401, description = "Unauthenticated"),
        (status = 404, description = "self-service deletion is switched off; file an erasure request instead"),
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
/// Best-effort: a failure here must not block someone exercising Art. 17. The consequence of
/// missing it is a queue entry whose subject is gone, which the operator queue already renders
/// as such.
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
    pub kind: RequestKind,
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
        (status = 201, description = "The filed request, with its response deadline", body = RequestRow),
        (status = 400, description = "rectification without detail, or a duplicate open request", body = crate::error::ProblemDetails),
        (status = 401, description = "authentication required", body = crate::error::ProblemDetails),
        (status = 404, description = "the data-subject request queue is switched off", body = crate::error::ProblemDetails),
    )
)]
pub async fn create_privacy_request(
    State(state): State<AppState>,
    user: AuthUser,
    Json(body): Json<NewPrivacyRequest>,
) -> ApiResult<(StatusCode, Json<RequestRow>)> {
    let detail = body
        .detail
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty());

    if body.kind == RequestKind::Rectification && detail.is_none() {
        return Err(ApiError::BadRequest(
            "say which data is wrong and what it should be".to_owned(),
        ));
    }

    if tankovault_db::repo::gdpr::has_open_of_kind(&state.pool, user.user_id, body.kind).await? {
        return Err(ApiError::BadRequest(
            "you already have an open request of this kind".to_owned(),
        ));
    }

    let request =
        tankovault_db::repo::gdpr::create(&state.pool, user.user_id, body.kind, detail).await?;

    audit(
        &state,
        &user,
        "privacy.request.create",
        &request.id.to_string(),
        &serde_json::json!({ "kind": body.kind }),
    )
    .await;

    Ok((StatusCode::CREATED, Json(request)))
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
        (status = 200, description = "The caller's requests", body = Vec<RequestRow>),
        (status = 401, description = "authentication required", body = crate::error::ProblemDetails),
        (status = 404, description = "the data-subject request queue is switched off", body = crate::error::ProblemDetails),
    )
)]
pub async fn list_privacy_requests(
    State(state): State<AppState>,
    user: AuthUser,
) -> ApiResult<Json<Vec<RequestRow>>> {
    Ok(Json(
        tankovault_db::repo::gdpr::list_for_user(&state.pool, user.user_id).await?,
    ))
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
        (status = 404, description = "no such open request for this caller", body = crate::error::ProblemDetails),
    )
)]
pub async fn cancel_privacy_request(
    State(state): State<AppState>,
    user: AuthUser,
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
