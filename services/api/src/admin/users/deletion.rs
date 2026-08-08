//! Administrative erasure — irreversible, unlike everything else in `users`, which is why it
//! gets its own module: confirmation, the pre-emptive audit write, and both guards are all
//! load-bearing here.

use crate::audit::{audit, audit_failure};
use crate::error::{ApiError, ApiResult};
use crate::openapi::ADMIN_USERS_TAG;
use crate::state::{AppState, AuthUser};
use axum::Json;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use serde::Deserialize;
use tankovault_domain::{Permission, UserId};
use utoipa::ToSchema;
use uuid::Uuid;

use super::{guard_not_last_administrator, guard_not_self, guard_not_super_user};

#[derive(Debug, Deserialize, ToSchema)]
pub struct DeleteUser {
    /// The target account's username, typed back as confirmation.
    pub confirm_username: String,
    /// Why the account is being erased. Recorded in the audit trail, which is the only place
    /// the reason can survive — the account itself is about to be gone.
    #[serde(default)]
    pub reason: Option<String>,
}

/// Erase a user
///
/// Deletes the account and every row it owns, using the same cascade as self-service erasure
/// (GDPR Art. 17) so there is one implementation rather than two that can diverge. The
/// account's audit records survive in pseudonymised form; see
/// `tankovault_db::repo::privacy::erase_user`.
///
/// Irreversible. Requires the username back, refuses the caller's own account, the deployment's
/// super user, and the last active administrator.
#[utoipa::path(
    delete,
    path = "/v1/admin/users/{id}",
    tag = ADMIN_USERS_TAG,
    params(("id" = Uuid, Path, description = "User id")),
    request_body = DeleteUser,
    security(("bearer_auth" = [])),
    responses(
        (status = 204, description = "Account and all owned data erased"),
        (status = 400, description = "confirmation mismatch, own account, the super user, or the last administrator", body = crate::error::ProblemDetails),
        (status = 401, description = "authentication required", body = crate::error::ProblemDetails),
        (status = 403, description = "no second factor is enrolled, a step-up is required, or the caller does not hold the required permission", body = crate::error::ProblemDetails),
        (status = 404, description = "no such user", body = crate::error::ProblemDetails),
    )
)]
pub async fn delete_user(
    State(state): State<AppState>,
    user: AuthUser,
    Path(id): Path<Uuid>,
    Json(body): Json<DeleteUser>,
) -> ApiResult<StatusCode> {
    user.require(Permission::UsersDelete).await?;
    let target = UserId::from_uuid(id);
    guard_not_self(&state, &user, target, "user.delete").await?;
    guard_not_super_user(&state, &user, target, "user.delete").await?;
    guard_not_last_administrator(&state, &user, target, "user.delete").await?;

    let account = tankovault_db::repo::users::get(&state.pool, target).await?;
    if body.confirm_username.trim() != account.username {
        audit_failure(
            &state,
            &user,
            "user.delete",
            &id.to_string(),
            &serde_json::json!({ "reason": "confirmation_mismatch" }),
        )
        .await;
        return Err(ApiError::BadRequest(
            "confirmation did not match the account's username".to_owned(),
        ));
    }

    // Recorded before deletion: afterward the username — the only human-readable handle on
    // what was erased — is gone. The id is kept though it no longer identifies anything.
    audit(
        &state,
        &user,
        "user.delete",
        &id.to_string(),
        &serde_json::json!({
            "username": account.username,
            "reason": body.reason.as_deref().map(str::trim).filter(|s| !s.is_empty()),
        }),
    )
    .await;

    let erased = tankovault_db::repo::privacy::erase_user(&state.pool, target).await?;
    if !erased {
        return Err(ApiError::NotFound);
    }
    tracing::info!(
        user_id = %id,
        actor = %user.user_id.as_uuid(),
        "account erased by an administrator"
    );
    Ok(StatusCode::NO_CONTENT)
}
