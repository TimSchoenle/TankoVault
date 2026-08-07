//! What the account may do: the grant editor's write path and the catalogue it renders from.
//! Both are served from the same compiled registry, so the list an operator sees and the set
//! the write path accepts cannot disagree.

use crate::audit::{audit, audit_failure};
use crate::error::{ApiError, ApiResult};
use crate::openapi::ADMIN_USERS_TAG;
use crate::state::{AppState, AuthUser};
use axum::Json;
use axum::extract::{Path, State};
use serde::{Deserialize, Serialize};
use tankovault_domain::{Permission, PermissionGroup, PermissionPreset, PermissionSet, UserId};
use utoipa::ToSchema;
use uuid::Uuid;

use super::detail::{UserDetailResponse, user_detail_response};
use super::{guard_not_last_administrator, guard_not_self};

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
        (status = 400, description = "cannot edit your own permissions, strip the last administrator, or grant the super user", body = crate::error::ProblemDetails),
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

    // The super user grant is minted by the installer and by nothing else, so this path may
    // only ever confirm one that already exists. Repeating it is what the console does on every
    // save — the checklist seeds from the stored grants, and the catalogue has no toggle to
    // clear this one with — while *adding* it is an escalation past the whole permission model,
    // since it would outlive every capability the enum currently has.
    if desired.is_super_user() {
        let already_super = tankovault_db::repo::permissions::resolve(&state.pool, target)
            .await?
            .is_some_and(|p| p.permissions.is_super_user());
        if !already_super {
            audit_failure(
                &state,
                &user,
                "user.permissions",
                &id.to_string(),
                &serde_json::json!({ "reason": "super_user_not_grantable" }),
            )
            .await;
            return Err(ApiError::BadRequest(
                "the super user is created by the installer and cannot be granted here".to_owned(),
            ));
        }
    }

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
        // `grantable()`, not `all()`: listing the super user grant would offer a checkbox the
        // editor is not allowed to tick, and every submit that left it ticked would be refused.
        permissions: Permission::grantable()
            .into_iter()
            .map(|p| PermissionInfo {
                key: p,
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
