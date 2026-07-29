//! User administration: the directory, per-account detail, identity edits, suspension,
//! forced sign-out, permission grants and administrative erasure.
//!
//! # The rules that are enforced here and nowhere else
//!
//! Three refusals live in this module because they are properties of the *deployment*, not of
//! any single row, and no database constraint can express them:
//!
//! 1. **No self-administration.** Suspension, erasure and permission edits refuse to target the
//!    caller. Not because it is dangerous in itself but because it is the wrong endpoint:
//!    `/v1/me` is where someone acts on their own account, and an administrator who can quietly
//!    grant themselves a capability produces an audit trail nobody can rely on.
//! 2. **The last administrator is protected.** Revoking, suspending or erasing the final active
//!    holder of [`Permission::UsersPermissions`] leaves the deployment with no way to grant
//!    anything ever again, recoverable only by editing the database by hand. Every path that
//!    could do it checks first.
//! 3. **Erasure demands the username back.** It is irreversible and cascades across every table;
//!    typing the name is the difference between an administrator deciding and an administrator
//!    mis-clicking.
//!
//! Everything mutating is audited, including the refusals — an attempt to erase the last
//! administrator is exactly the event an operator wants to find later.

use crate::audit::{audit, audit_failure};
use crate::error::{ApiError, ApiResult};
use crate::openapi::ADMIN_USERS_TAG;
use crate::state::{AppState, AuthUser};
use crate::views::IntoView;
use axum::Json;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use serde::{Deserialize, Serialize};
use tankovault_contracts::admin::{GrantView, UserDetailView, UserDirectoryPage};
use tankovault_domain::{
    AccountStatus, Permission, PermissionGroup, PermissionPreset, PermissionSet, UserId,
};
use utoipa::{IntoParams, ToSchema};
use uuid::Uuid;

/// Directory paging. Deliberately capped server-side — the console offers 25/50/100 and a
/// caller asking for everything would page the whole user table into one response.
const MAX_PAGE: i64 = 200;

#[derive(Debug, Deserialize, IntoParams)]
pub struct DirectoryQuery {
    /// Case-insensitive substring match on username or email. Empty lists everyone.
    #[serde(default)]
    pub search: Option<String>,
    #[serde(default)]
    pub limit: Option<i64>,
    #[serde(default)]
    pub offset: Option<i64>,
}

/// List users
///
/// The operator user directory: identity, account state, permission and tracking counts, and
/// last sign-in, with search and paging.
#[utoipa::path(
    get,
    path = "/v1/admin/users",
    tag = ADMIN_USERS_TAG,
    params(DirectoryQuery),
    security(("bearer_auth" = [])),
    responses(
        (status = 200, description = "A page of the user directory", body = UserDirectoryPage),
        (status = 401, description = "authentication required", body = crate::error::ProblemDetails),
        (status = 403, description = "caller does not hold the required permission", body = crate::error::ProblemDetails),
    )
)]
pub async fn list_users(
    State(state): State<AppState>,
    user: AuthUser,
    Query(q): Query<DirectoryQuery>,
) -> ApiResult<Json<UserDirectoryPage>> {
    user.require(Permission::UsersRead).await?;
    let search = q.search.unwrap_or_default();
    let limit = q.limit.unwrap_or(50).clamp(1, MAX_PAGE);
    let offset = q.offset.unwrap_or(0).max(0);
    Ok(Json(
        tankovault_db::repo::user_admin::directory(&state.pool, search.trim(), limit, offset)
            .await?
            .into_view(),
    ))
}

/// One account's administrative detail, together with its grants.
#[derive(Debug, Serialize, ToSchema)]
pub struct UserDetailResponse {
    pub user: UserDetailView,
    /// The account's permission grants, with provenance.
    pub permissions: Vec<GrantView>,
}

/// Read one account's detail panel: the account row plus its grants.
///
/// Every mutating handler below answers with the *re-read* state rather than echoing its own
/// input, so the client always renders what the database now holds. That made the same two
/// queries appear five times; they live here instead.
async fn user_detail_response(
    state: &AppState,
    target: UserId,
) -> ApiResult<Json<UserDetailResponse>> {
    let user = tankovault_db::repo::user_admin::detail(&state.pool, target).await?;
    let permissions = tankovault_db::repo::permissions::list_for_user(&state.pool, target).await?;
    Ok(Json(UserDetailResponse {
        user: user.into_view(),
        permissions: permissions.into_view(),
    }))
}

/// Get a user
///
/// Everything the user-detail panel shows: identity, account state, activity counts, and the
/// permissions currently granted with who granted each and when.
#[utoipa::path(
    get,
    path = "/v1/admin/users/{id}",
    tag = ADMIN_USERS_TAG,
    params(("id" = Uuid, Path, description = "User id")),
    security(("bearer_auth" = [])),
    responses(
        (status = 200, description = "The account and its grants", body = UserDetailResponse),
        (status = 401, description = "authentication required", body = crate::error::ProblemDetails),
        (status = 403, description = "caller does not hold the required permission", body = crate::error::ProblemDetails),
        (status = 404, description = "no such user", body = crate::error::ProblemDetails),
    )
)]
pub async fn get_user(
    State(state): State<AppState>,
    user: AuthUser,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<UserDetailResponse>> {
    user.require(Permission::UsersRead).await?;
    let target = UserId::from_uuid(id);
    user_detail_response(&state, target).await
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct AdminProfileUpdate {
    #[serde(default)]
    pub username: Option<String>,
    #[serde(default)]
    pub email: Option<String>,
}

/// Edit a user's identity
///
/// Change another account's username and/or email. Omitted fields are left alone.
///
/// Changing an email does **not** re-open verification: an administrator setting an address is
/// asserting it is correct, and forcing a confirmation round trip would lock the account out
/// until the user acted on mail they did not expect.
#[utoipa::path(
    patch,
    path = "/v1/admin/users/{id}",
    tag = ADMIN_USERS_TAG,
    params(("id" = Uuid, Path, description = "User id")),
    request_body = AdminProfileUpdate,
    security(("bearer_auth" = [])),
    responses(
        (status = 200, description = "The updated account", body = UserDetailResponse),
        (status = 401, description = "authentication required", body = crate::error::ProblemDetails),
        (status = 403, description = "caller does not hold the required permission", body = crate::error::ProblemDetails),
        (status = 404, description = "no such user", body = crate::error::ProblemDetails),
        (status = 409, description = "email or username already taken", body = crate::error::ProblemDetails),
    )
)]
pub async fn update_user(
    State(state): State<AppState>,
    user: AuthUser,
    Path(id): Path<Uuid>,
    Json(body): Json<AdminProfileUpdate>,
) -> ApiResult<Json<UserDetailResponse>> {
    user.require(Permission::UsersWrite).await?;
    let target = UserId::from_uuid(id);

    let username = body
        .username
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty());
    let email = body
        .email
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty());
    if username.is_none() && email.is_none() {
        return Err(ApiError::BadRequest("nothing to update".to_owned()));
    }

    tankovault_db::repo::user_admin::update_identity(&state.pool, target, username, email).await?;
    audit(
        &state,
        &user,
        "user.update",
        &id.to_string(),
        &serde_json::json!({ "username": username, "email": email }),
    )
    .await;

    user_detail_response(&state, target).await
}

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
#[utoipa::path(
    post,
    path = "/v1/admin/users/{id}/status",
    tag = ADMIN_USERS_TAG,
    params(("id" = Uuid, Path, description = "User id")),
    request_body = SetUserStatus,
    security(("bearer_auth" = [])),
    responses(
        (status = 200, description = "The updated account", body = UserDetailResponse),
        (status = 400, description = "cannot suspend your own account, or the last administrator", body = crate::error::ProblemDetails),
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

#[derive(Debug, Deserialize, ToSchema)]
pub struct SetPermissions {
    /// The complete permission set the account should have afterwards.
    ///
    /// A whole set rather than add/remove operations: the editor is a checklist, and submitting
    /// the intended end state means two administrators editing concurrently produce one of their
    /// two intents rather than an interleaving of both. Unknown tokens are rejected by
    /// deserialisation — a capability that does not exist cannot be granted.
    pub permissions: Vec<Permission>,
}

/// Set a user's permissions
///
/// Replaces the account's entire grant set. The audit record names what was actually added and
/// removed, not the submitted list, so "who gave them that" is answerable afterwards.
#[utoipa::path(
    put,
    path = "/v1/admin/users/{id}/permissions",
    tag = ADMIN_USERS_TAG,
    params(("id" = Uuid, Path, description = "User id")),
    request_body = SetPermissions,
    security(("bearer_auth" = [])),
    responses(
        (status = 200, description = "The updated account and its grants", body = UserDetailResponse),
        (status = 400, description = "cannot edit your own permissions, or strip the last administrator", body = crate::error::ProblemDetails),
        (status = 401, description = "authentication required", body = crate::error::ProblemDetails),
        (status = 403, description = "caller does not hold the required permission", body = crate::error::ProblemDetails),
        (status = 404, description = "no such user", body = crate::error::ProblemDetails),
    )
)]
pub async fn set_user_permissions(
    State(state): State<AppState>,
    user: AuthUser,
    Path(id): Path<Uuid>,
    Json(body): Json<SetPermissions>,
) -> ApiResult<Json<UserDetailResponse>> {
    user.require(Permission::UsersPermissions).await?;
    let target = UserId::from_uuid(id);
    guard_not_self(&state, &user, target, "user.permissions").await?;

    let desired: PermissionSet = body.permissions.into_iter().collect();

    // Only a *removal* of the meta-capability can strip the last administrator; granting it can
    // only ever add one.
    if !desired.has(Permission::UsersPermissions) {
        guard_not_last_administrator(&state, &user, target, "user.permissions").await?;
    }

    // The target must exist. `replace` on a missing user would insert nothing and report an
    // empty diff, which reads as "no change" rather than "no such user".
    let _ = tankovault_db::repo::user_admin::detail(&state.pool, target).await?;

    let mut conn = state.pool.acquire().await.map_err(|e| {
        tracing::error!(error = %e, "failed to acquire a connection for a permission edit");
        ApiError::Internal
    })?;
    let diff = tankovault_db::repo::permissions::replace(&mut conn, target, &desired, user.user_id)
        .await?;
    drop(conn);

    audit(
        &state,
        &user,
        "user.permissions",
        &id.to_string(),
        &serde_json::json!({ "added": diff.added, "removed": diff.removed }),
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

/// Confirm a user's email address
///
/// The escape hatch for an account that never received its confirmation link. Without it, a
/// deployment whose outbound mail broke leaves those accounts permanently unable to sign in
/// with no self-service route back.
#[utoipa::path(
    post,
    path = "/v1/admin/users/{id}/verify-email",
    tag = ADMIN_USERS_TAG,
    params(("id" = Uuid, Path, description = "User id")),
    security(("bearer_auth" = [])),
    responses(
        (status = 200, description = "The updated account", body = UserDetailResponse),
        (status = 401, description = "authentication required", body = crate::error::ProblemDetails),
        (status = 403, description = "caller does not hold the required permission", body = crate::error::ProblemDetails),
        (status = 404, description = "no such user", body = crate::error::ProblemDetails),
    )
)]
pub async fn verify_user_email(
    State(state): State<AppState>,
    user: AuthUser,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<UserDetailResponse>> {
    user.require(Permission::UsersWrite).await?;
    let target = UserId::from_uuid(id);
    tankovault_db::repo::user_admin::force_verify_email(&state.pool, target).await?;
    audit(
        &state,
        &user,
        "user.verify_email",
        &id.to_string(),
        &serde_json::json!({}),
    )
    .await;
    user_detail_response(&state, target).await
}

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
/// Irreversible. Requires the username back, refuses the caller's own account, and refuses the
/// last active administrator.
#[utoipa::path(
    delete,
    path = "/v1/admin/users/{id}",
    tag = ADMIN_USERS_TAG,
    params(("id" = Uuid, Path, description = "User id")),
    request_body = DeleteUser,
    security(("bearer_auth" = [])),
    responses(
        (status = 204, description = "Account and all owned data erased"),
        (status = 400, description = "confirmation mismatch, own account, or the last administrator", body = crate::error::ProblemDetails),
        (status = 401, description = "authentication required", body = crate::error::ProblemDetails),
        (status = 403, description = "caller does not hold the required permission", body = crate::error::ProblemDetails),
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

    // Recorded before the deletion: afterwards the account no longer exists and its username —
    // the only human-readable handle on what was erased — is unrecoverable. The target id is
    // retained because it is no longer an identifier for anything.
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

/// One entry of the permission catalogue the editor renders.
#[derive(Debug, Serialize, ToSchema)]
pub struct PermissionInfo {
    pub key: Permission,
    pub group: PermissionGroup,
    /// What the capability allows, in the operator's terms.
    pub description: &'static str,
}

/// A named starting point in the permission editor.
#[derive(Debug, Serialize, ToSchema)]
pub struct PresetInfo {
    pub key: PermissionPreset,
    /// The permissions this preset expands to. Sent expanded rather than by name so applying a
    /// preset is a pure client-side operation over the same checklist the operator then edits —
    /// which is what keeps a preset from behaving like a stored role.
    pub permissions: Vec<Permission>,
}

/// The permission catalogue and its presets.
#[derive(Debug, Serialize, ToSchema)]
pub struct PermissionCatalogue {
    pub permissions: Vec<PermissionInfo>,
    pub presets: Vec<PresetInfo>,
}

/// List assignable permissions
///
/// Every capability this build defines, with its grouping and description, plus the preset
/// bundles the editor offers. Served from the compiled registry, so the editor can never list a
/// capability the backend does not enforce or miss one it does.
#[utoipa::path(
    get,
    path = "/v1/admin/permissions",
    tag = ADMIN_USERS_TAG,
    security(("bearer_auth" = [])),
    responses(
        (status = 200, description = "The permission catalogue", body = PermissionCatalogue),
        (status = 401, description = "authentication required", body = crate::error::ProblemDetails),
        (status = 403, description = "caller does not hold the required permission", body = crate::error::ProblemDetails),
    )
)]
pub async fn permission_catalogue(user: AuthUser) -> ApiResult<Json<PermissionCatalogue>> {
    user.require(Permission::UsersRead).await?;
    Ok(Json(PermissionCatalogue {
        permissions: Permission::all()
            .iter()
            .map(|p| PermissionInfo {
                key: *p,
                group: p.group(),
                description: p.description(),
            })
            .collect(),
        presets: PermissionPreset::all()
            .iter()
            .map(|preset| PresetInfo {
                key: *preset,
                permissions: preset.permissions(),
            })
            .collect(),
    }))
}

/// Refuse an administrative action aimed at the caller's own account.
async fn guard_not_self(
    state: &AppState,
    user: &AuthUser,
    target: UserId,
    action: &'static str,
) -> ApiResult<()> {
    if target != user.user_id {
        return Ok(());
    }
    audit_failure(
        state,
        user,
        action,
        &target.as_uuid().to_string(),
        &serde_json::json!({ "reason": "self_target" }),
    )
    .await;
    Err(ApiError::BadRequest(
        "use your own account settings; administrative actions cannot target yourself".to_owned(),
    ))
}

/// Refuse an action that would leave no active holder of [`Permission::UsersPermissions`].
///
/// Checked against *other* accounts, so it permits the action whenever anyone else could still
/// administer the deployment, and refuses only when this really is the last one.
async fn guard_not_last_administrator(
    state: &AppState,
    user: &AuthUser,
    target: UserId,
    action: &'static str,
) -> ApiResult<()> {
    let target_is_admin = tankovault_db::repo::permissions::resolve(&state.pool, target)
        .await?
        .is_some_and(|p| p.permissions.has(Permission::UsersPermissions));
    if !target_is_admin {
        return Ok(());
    }

    let others = tankovault_db::repo::permissions::other_active_holders(
        &state.pool,
        Permission::UsersPermissions,
        target,
    )
    .await?;
    if others > 0 {
        return Ok(());
    }

    audit_failure(
        state,
        user,
        action,
        &target.as_uuid().to_string(),
        &serde_json::json!({ "reason": "last_administrator" }),
    )
    .await;
    Err(ApiError::BadRequest(
        "this is the only active account that can administer permissions; grant that \
         permission to someone else first"
            .to_owned(),
    ))
}
