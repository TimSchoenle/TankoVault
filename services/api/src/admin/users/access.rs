//! Whether the account may act at all (suspension), and whether it is acting right now (session
//! revocation). Neither is a capability edit; those are [`super::permissions`].

use crate::audit::audit;
use crate::error::ApiResult;
use crate::openapi::ADMIN_USERS_TAG;
use crate::state::{AppState, AuthUser};
use axum::Json;
use axum::extract::{Path, State};
use serde::Deserialize;
use tankovault_domain::{AccountStatus, Permission, UserId};
use utoipa::ToSchema;
use uuid::Uuid;

use super::detail::{UserDetailResponse, user_detail_response};
use super::{guard_not_last_administrator, guard_not_self, guard_not_super_user};

#[derive(Debug, Deserialize, ToSchema)]
pub struct SetUserStatus {
    pub status: AccountStatus,
    /// Why the account was suspended. Recorded on the account and shown to whoever looks at it
    /// next; ignored when reinstating.
    #[serde(default)]
    pub reason: Option<String>,
    /// Also revoke every live session, so the suspension takes effect immediately rather than
    /// when the current access token expires.
    ///
    /// Defaults to `true`: a suspension that leaves the account working for the next quarter of
    /// an hour is not what anyone means by suspending it. Settable to `false` for the case
    /// where an operator wants the account frozen but the user's current tab left alone.
    #[serde(default = "default_true")]
    pub revoke_sessions: bool,
}

fn default_true() -> bool {
    true
}

/// Suspend or reinstate a user
///
/// A suspended account cannot sign in, refresh a session, or take any action — the check
/// happens before authorization, so it is not something a permission can override. Nothing the
/// account owns is deleted, so the change is fully reversible.
///
/// Refuses the deployment's super user: suspending the owner cannot be undone by the owner.
#[utoipa::path(
    post,
    path = "/v1/admin/users/{id}/status",
    tag = ADMIN_USERS_TAG,
    params(("id" = Uuid, Path, description = "User id")),
    request_body = SetUserStatus,
    security(("bearer_auth" = [])),
    responses(
        (status = 200, description = "The updated account", body = UserDetailResponse),
        (status = 400, description = "cannot suspend your own account, the super user, or the last administrator", body = crate::error::ProblemDetails),
        (status = 401, description = "authentication required", body = crate::error::ProblemDetails),
        (status = 403, description = "caller does not hold the required permission", body = crate::error::ProblemDetails),
        (status = 404, description = "no such user", body = crate::error::ProblemDetails),
    )
)]
pub async fn set_user_status(
    State(state): State<AppState>,
    user: AuthUser,
    Path(id): Path<Uuid>,
    Json(body): Json<SetUserStatus>,
) -> ApiResult<Json<UserDetailResponse>> {
    user.require(Permission::UsersWrite).await?;
    let target = UserId::from_uuid(id);
    guard_not_self(&state, &user, target, "user.status").await?;

    if body.status == AccountStatus::Suspended {
        guard_not_super_user(&state, &user, target, "user.status").await?;
        guard_not_last_administrator(&state, &user, target, "user.status").await?;
    }

    let reason = body
        .reason
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty());
    tankovault_db::repo::user_admin::set_status(&state.pool, target, body.status, reason).await?;

    let revoked = if body.revoke_sessions {
        tankovault_db::repo::users::revoke_all_sessions(&state.pool, target).await?
    } else {
        0
    };

    audit(
        &state,
        &user,
        "user.status",
        &id.to_string(),
        &serde_json::json!({
            "status": body.status.as_str(),
            "reason": reason,
            "sessions_revoked": revoked,
        }),
    )
    .await;

    user_detail_response(&state, target).await
}

/// Revoke a user's sessions
///
/// Signs the account out of every device. Does not change what the account may do — use
/// suspension for that; this only ends the sessions currently in flight.
#[utoipa::path(
    post,
    path = "/v1/admin/users/{id}/revoke-sessions",
    tag = ADMIN_USERS_TAG,
    params(("id" = Uuid, Path, description = "User id")),
    security(("bearer_auth" = [])),
    responses(
        (status = 200, description = "How many sessions were revoked", body = serde_json::Value, example = json!({"revoked": 3})),
        (status = 401, description = "authentication required", body = crate::error::ProblemDetails),
        (status = 403, description = "caller does not hold the required permission", body = crate::error::ProblemDetails),
    )
)]
pub async fn revoke_user_sessions(
    State(state): State<AppState>,
    user: AuthUser,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<serde_json::Value>> {
    user.require(Permission::UsersSessions).await?;
    let target = UserId::from_uuid(id);
    let revoked = tankovault_db::repo::users::revoke_all_sessions(&state.pool, target).await?;
    audit(
        &state,
        &user,
        "user.sessions.revoke",
        &id.to_string(),
        &serde_json::json!({ "revoked": revoked }),
    )
    .await;
    Ok(Json(serde_json::json!({ "revoked": revoked })))
}
