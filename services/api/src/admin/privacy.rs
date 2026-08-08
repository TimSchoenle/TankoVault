//! The operator side of the data-subject request queue: triage, fulfilment and the record of
//! how each request was answered. Duplicates [`crate::me::privacy`] deliberately, since these
//! act on another person's account under separately audited permissions.

use crate::audit::audit;
use crate::error::{ApiError, ApiResult};
use crate::openapi::ADMIN_PRIVACY_TAG;
use crate::state::{AppState, AuthUser};
use crate::views::{IntoStored, IntoView};
use axum::Json;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderValue, header};
use axum::response::{IntoResponse, Response};
use serde::Deserialize;
use tankovault_contracts::admin::AdminPrivacyRequestView;
use tankovault_contracts::me::PrivacyRequestStatus;
use tankovault_domain::{Permission, UserId};
use utoipa::{IntoParams, ToSchema};
use uuid::Uuid;

/// Queue page size. The queue is a work list, not an archive; an operator who needs the full
/// history reads the audit trail.
const QUEUE_LIMIT: i64 = 200;

#[derive(Debug, Deserialize, IntoParams)]
pub struct QueueQuery {
    /// Show only unresolved requests. Defaults to `true` — the queue's job is the work
    /// outstanding, and resolved requests are the compliance record behind it.
    #[serde(default)]
    pub include_resolved: bool,
}

/// List data-subject requests
///
/// The privacy queue, most urgent first: open requests ordered by their Art. 12(3) deadline,
/// each flagged if that deadline has passed.
#[utoipa::path(
    get,
    path = "/v1/admin/privacy/requests",
    tag = ADMIN_PRIVACY_TAG,
    params(QueueQuery),
    security(("bearer_auth" = [])),
    responses(
        (status = 200, description = "The request queue", body = Vec<AdminPrivacyRequestView>),
        (status = 401, description = "authentication required", body = crate::error::ProblemDetails),
        (status = 403, description = "no second factor is enrolled, or the caller does not hold the required permission", body = crate::error::ProblemDetails),
    )
)]
pub async fn list_privacy_queue(
    State(state): State<AppState>,
    user: AuthUser,
    Query(q): Query<QueueQuery>,
) -> ApiResult<Json<Vec<AdminPrivacyRequestView>>> {
    user.require(Permission::PrivacyRead).await?;
    let rows = tankovault_db::repo::gdpr::list_admin(&state.pool, !q.include_resolved, QUEUE_LIMIT)
        .await?;
    Ok(Json(rows.into_view()))
}

/// Claim a data-subject request
///
/// Takes ownership of a pending request so two operators do not work the same one. Refuses if
/// somebody already claimed it, rather than silently taking it over.
#[utoipa::path(
    post,
    path = "/v1/admin/privacy/requests/{id}/claim",
    tag = ADMIN_PRIVACY_TAG,
    params(("id" = Uuid, Path, description = "Request id")),
    security(("bearer_auth" = [])),
    responses(
        (status = 200, description = "The claimed request", body = AdminPrivacyRequestView),
        (status = 409, description = "already claimed or already resolved", body = crate::error::ProblemDetails),
        (status = 401, description = "authentication required", body = crate::error::ProblemDetails),
        (status = 403, description = "no second factor is enrolled, a step-up is required, or the caller does not hold the required permission", body = crate::error::ProblemDetails),
        (status = 404, description = "no such request", body = crate::error::ProblemDetails),
    )
)]
pub async fn claim_privacy_request(
    State(state): State<AppState>,
    user: AuthUser,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<AdminPrivacyRequestView>> {
    user.require(Permission::PrivacyWrite).await?;

    if !tankovault_db::repo::gdpr::claim(&state.pool, id, user.user_id).await? {
        // Distinguish "gone" from "someone got there first": the second is a normal race
        // between two operators opening the same queue, and saying so is more useful than 404.
        let _ = tankovault_db::repo::gdpr::get_admin(&state.pool, id).await?;
        return Err(ApiError::Conflict(
            "this request has already been claimed or resolved".to_owned(),
        ));
    }

    audit(
        &state,
        &user,
        "privacy.request.claim",
        &id.to_string(),
        &serde_json::json!({}),
    )
    .await;
    Ok(Json(
        tankovault_db::repo::gdpr::get_admin(&state.pool, id)
            .await?
            .into_view(),
    ))
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct ResolveRequest {
    /// How it ends. Only `completed` and `rejected` are operator decisions; `cancelled` belongs
    /// to the subject and is refused here.
    pub status: PrivacyRequestStatus,
    /// What was done, or — for a rejection — why.
    ///
    /// Mandatory on a rejection: Art. 12(4) obliges the controller to give reasons for refusing
    /// to act, so a rejection without one is not a lawful response.
    #[serde(default)]
    pub note: Option<String>,
}

/// Resolve a data-subject request
///
/// Marks a request completed or rejected. Only transitions out of an open state, so a
/// resolution cannot be recorded twice or rewritten later.
///
/// Does **not** perform the underlying action — completing an erasure request does not erase.
/// Fulfilment is [`fulfil_erasure`] and [`export_subject_data`], each with its own permission,
/// so "we said we did it" and "we did it" are separate records.
#[utoipa::path(
    post,
    path = "/v1/admin/privacy/requests/{id}/resolve",
    tag = ADMIN_PRIVACY_TAG,
    params(("id" = Uuid, Path, description = "Request id")),
    request_body = ResolveRequest,
    security(("bearer_auth" = [])),
    responses(
        (status = 200, description = "The resolved request", body = AdminPrivacyRequestView),
        (status = 400, description = "invalid target status, or a rejection with no reason", body = crate::error::ProblemDetails),
        (status = 409, description = "the request was already resolved", body = crate::error::ProblemDetails),
        (status = 401, description = "authentication required", body = crate::error::ProblemDetails),
        (status = 403, description = "no second factor is enrolled, a step-up is required, or the caller does not hold the required permission", body = crate::error::ProblemDetails),
        (status = 404, description = "no such request", body = crate::error::ProblemDetails),
    )
)]
pub async fn resolve_privacy_request(
    State(state): State<AppState>,
    user: AuthUser,
    Path(id): Path<Uuid>,
    Json(body): Json<ResolveRequest>,
) -> ApiResult<Json<AdminPrivacyRequestView>> {
    user.require(Permission::PrivacyWrite).await?;

    if !matches!(
        body.status,
        PrivacyRequestStatus::Completed | PrivacyRequestStatus::Rejected
    ) {
        return Err(ApiError::BadRequest(
            "a request can only be resolved as completed or rejected".to_owned(),
        ));
    }

    let note = body
        .note
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty());
    if body.status == PrivacyRequestStatus::Rejected && note.is_none() {
        return Err(ApiError::BadRequest(
            "a rejection must state its reasons (GDPR Art. 12(4))".to_owned(),
        ));
    }

    if !tankovault_db::repo::gdpr::resolve(
        &state.pool,
        id,
        body.status.into_stored(),
        Some(user.user_id),
        note,
    )
    .await?
    {
        let _ = tankovault_db::repo::gdpr::get_admin(&state.pool, id).await?;
        return Err(ApiError::Conflict(
            "this request has already been resolved".to_owned(),
        ));
    }

    audit(
        &state,
        &user,
        "privacy.request.resolve",
        &id.to_string(),
        &serde_json::json!({ "status": body.status, "note": note }),
    )
    .await;
    Ok(Json(
        tankovault_db::repo::gdpr::get_admin(&state.pool, id)
            .await?
            .into_view(),
    ))
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct ExtendRequest {
    /// The new deadline, RFC 3339.
    #[schema(value_type = String)]
    #[serde(with = "time::serde::rfc3339")]
    pub due_at: time::OffsetDateTime,
    /// Why an extension is needed. Art. 12(3) requires the subject to be told of the extension
    /// and its reasons, so recording one is part of taking it.
    pub reason: String,
}

/// Extend a request's deadline
///
/// Art. 12(3) allows up to two further months for complex requests. Only moves a deadline
/// later: the subject has been told a date, and a controller shortening its own window after
/// the fact is not an extension.
#[utoipa::path(
    post,
    path = "/v1/admin/privacy/requests/{id}/extend",
    tag = ADMIN_PRIVACY_TAG,
    params(("id" = Uuid, Path, description = "Request id")),
    request_body = ExtendRequest,
    security(("bearer_auth" = [])),
    responses(
        (status = 200, description = "The request with its new deadline", body = AdminPrivacyRequestView),
        (status = 400, description = "the request is resolved, or the new date is not later", body = crate::error::ProblemDetails),
        (status = 401, description = "authentication required", body = crate::error::ProblemDetails),
        (status = 403, description = "no second factor is enrolled, a step-up is required, or the caller does not hold the required permission", body = crate::error::ProblemDetails),
        (status = 404, description = "no such request", body = crate::error::ProblemDetails),
    )
)]
pub async fn extend_privacy_request(
    State(state): State<AppState>,
    user: AuthUser,
    Path(id): Path<Uuid>,
    Json(body): Json<ExtendRequest>,
) -> ApiResult<Json<AdminPrivacyRequestView>> {
    user.require(Permission::PrivacyWrite).await?;

    let reason = body.reason.trim();
    if reason.is_empty() {
        return Err(ApiError::BadRequest(
            "an extension must state its reasons (GDPR Art. 12(3))".to_owned(),
        ));
    }

    if !tankovault_db::repo::gdpr::extend_due(&state.pool, id, body.due_at).await? {
        let _ = tankovault_db::repo::gdpr::get_admin(&state.pool, id).await?;
        return Err(ApiError::BadRequest(
            "the request is already resolved, or the new deadline is not later than the current one"
                .to_owned(),
        ));
    }

    audit(
        &state,
        &user,
        "privacy.request.extend",
        &id.to_string(),
        &serde_json::json!({ "due_at": body.due_at, "reason": reason }),
    )
    .await;
    Ok(Json(
        tankovault_db::repo::gdpr::get_admin(&state.pool, id)
            .await?
            .into_view(),
    ))
}

/// Export a subject's data for a request
///
/// Produces the same document as the subject's own export, for delivery in answer to an access
/// (Art. 15) or portability (Art. 20) request.
///
/// Gated on [`Permission::PrivacyExport`] rather than `privacy.write`, and audited under its own
/// action: this is the one operation in the queue that *discloses* another person's entire
/// record, and it needs to be separately grantable and separately searchable afterwards.
///
/// Refused for request kinds that are not about disclosure — an erasure request is not a licence
/// to read the account first.
#[utoipa::path(
    get,
    path = "/v1/admin/privacy/requests/{id}/export",
    tag = ADMIN_PRIVACY_TAG,
    params(("id" = Uuid, Path, description = "Request id")),
    security(("bearer_auth" = [])),
    responses(
        (status = 200, description = "The subject's complete personal-data export", body = serde_json::Value),
        (status = 400, description = "this request kind does not call for an export", body = crate::error::ProblemDetails),
        (status = 401, description = "authentication required", body = crate::error::ProblemDetails),
        (status = 403, description = "no second factor is enrolled, a step-up is required, or the caller does not hold the required permission", body = crate::error::ProblemDetails),
        (status = 404, description = "no such request, or its subject no longer exists", body = crate::error::ProblemDetails),
    )
)]
pub async fn export_subject_data(
    State(state): State<AppState>,
    user: AuthUser,
    Path(id): Path<Uuid>,
) -> ApiResult<Response> {
    user.require(Permission::PrivacyExport).await?;

    let request = tankovault_db::repo::gdpr::get_admin(&state.pool, id).await?;
    if !request.request.kind.needs_export() {
        return Err(ApiError::BadRequest(
            "this request is not an access or portability request".to_owned(),
        ));
    }
    // A missing subject means the account is gone — which for a completed erasure is the
    // expected end state, not an error in this system.
    let subject = request
        .user_id
        .map(UserId::from_uuid)
        .ok_or(ApiError::NotFound)?;

    let export = tankovault_db::repo::privacy::export_user_data(&state.pool, subject).await?;

    audit(
        &state,
        &user,
        "privacy.subject.export",
        &id.to_string(),
        &serde_json::json!({ "subject_id": subject.as_uuid(), "kind": request.request.kind }),
    )
    .await;
    tracing::info!(
        request_id = %id,
        actor = %user.user_id.as_uuid(),
        "personal data exported to fulfil a data-subject request"
    );

    let filename = format!("tankovault-export-{}.json", subject.as_uuid());
    let mut response = Json(export).into_response();
    if let Ok(value) = HeaderValue::from_str(&format!("attachment; filename=\"{filename}\"")) {
        response
            .headers_mut()
            .insert(header::CONTENT_DISPOSITION, value);
    }
    Ok(response)
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct FulfilErasure {
    /// The subject's username, typed back as confirmation.
    pub confirm_username: String,
}

/// Carry out an erasure request
///
/// Erases the subject's account and everything it owns, then resolves the request as completed
/// in the same call — so the compliance record cannot end up saying an erasure was done when it
/// was not, or the reverse.
///
/// Requires [`Permission::UsersDelete`] as well as [`Permission::PrivacyWrite`]: working the
/// privacy queue and being able to destroy an account are separate authorities, and this is the
/// one action that needs both.
#[utoipa::path(
    post,
    path = "/v1/admin/privacy/requests/{id}/fulfil-erasure",
    tag = ADMIN_PRIVACY_TAG,
    params(("id" = Uuid, Path, description = "Request id")),
    request_body = FulfilErasure,
    security(("bearer_auth" = [])),
    responses(
        (status = 200, description = "The completed request", body = AdminPrivacyRequestView),
        (status = 400, description = "not an erasure request, or the confirmation did not match", body = crate::error::ProblemDetails),
        (status = 401, description = "authentication required", body = crate::error::ProblemDetails),
        (status = 403, description = "no second factor is enrolled, a step-up is required, or the caller does not hold the required permission", body = crate::error::ProblemDetails),
        (status = 404, description = "no such request, or its subject no longer exists", body = crate::error::ProblemDetails),
    )
)]
pub async fn fulfil_erasure(
    State(state): State<AppState>,
    user: AuthUser,
    Path(id): Path<Uuid>,
    Json(body): Json<FulfilErasure>,
) -> ApiResult<Json<AdminPrivacyRequestView>> {
    user.require_all(&[Permission::PrivacyWrite, Permission::UsersDelete])
        .await?;

    let request = tankovault_db::repo::gdpr::get_admin(&state.pool, id).await?;
    if !request.request.kind.is_erasure() {
        return Err(ApiError::BadRequest(
            "this is not an erasure request".to_owned(),
        ));
    }
    if !request.request.status.is_open() {
        return Err(ApiError::Conflict(
            "this request has already been resolved".to_owned(),
        ));
    }
    let subject = request
        .user_id
        .map(UserId::from_uuid)
        .ok_or(ApiError::NotFound)?;

    let account = tankovault_db::repo::users::get(&state.pool, subject).await?;
    if body.confirm_username.trim() != account.username {
        return Err(ApiError::BadRequest(
            "confirmation did not match the subject's username".to_owned(),
        ));
    }

    // Resolve before erasing: erasure nulls the request's `user_id`, so resolving afterward
    // would record the outcome against a row whose subject link is already gone.
    tankovault_db::repo::gdpr::resolve(
        &state.pool,
        id,
        PrivacyRequestStatus::Completed.into_stored(),
        Some(user.user_id),
        Some("erasure carried out"),
    )
    .await?;

    audit(
        &state,
        &user,
        "privacy.request.fulfil_erasure",
        &id.to_string(),
        &serde_json::json!({ "subject_id": subject.as_uuid(), "username": account.username }),
    )
    .await;

    tankovault_db::repo::privacy::erase_user(&state.pool, subject).await?;
    tracing::info!(
        request_id = %id,
        actor = %user.user_id.as_uuid(),
        "account erased to fulfil a data-subject request"
    );

    Ok(Json(
        tankovault_db::repo::gdpr::get_admin(&state.pool, id)
            .await?
            .into_view(),
    ))
}
