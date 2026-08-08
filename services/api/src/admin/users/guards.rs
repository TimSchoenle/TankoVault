//! The refusals every account-writing path in this module shares. None of them is expressible
//! as a database constraint, so every path touching these columns must call them explicitly.

use crate::audit::audit_failure;
use crate::error::{ApiError, ApiResult};
use crate::state::{AppState, AuthUser};
use tankovault_domain::{Permission, UserId};

/// Refuse an administrative action aimed at the caller's own account. `/v1/me` is where someone
/// acts on themselves; an administrator who can self-grant a capability leaves an audit trail
/// nobody can rely on.
pub(crate) async fn guard_not_self(
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

/// Refuses an action that would leave no active holder of [`Permission::UsersPermissions`] —
/// done silently, the deployment could never grant anything again.
///
/// Checked against *other* accounts: permits the action whenever someone else could still
/// administer the deployment.
pub(crate) async fn guard_not_last_administrator(
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

/// Refuses an action that would take the deployment's super user out of service — suspension or
/// erasure, aimed at the one account [`Permission::SuperUser`] belongs to.
///
/// [`guard_not_last_administrator`] does not cover this. It permits the action as soon as any
/// other active administrator exists, and the super user grant is not like the grants that
/// administrator holds: it is single-slot, cannot be minted through the API, and dies with the
/// row it sits on. So an administrator who suspends or erases the owner ends the deployment's
/// ownership permanently, and — since the reconciler promotes the earliest remaining
/// administrator — may well end it in their own favour. Nothing about holding `users.write` or
/// `users.delete` implies that.
///
/// The owner is not locked out by their own tools: this refuses actions *aimed at* the super
/// user, and administrative actions cannot be aimed at yourself in the first place
/// ([`guard_not_self`]).
pub(crate) async fn guard_not_super_user(
    state: &AppState,
    user: &AuthUser,
    target: UserId,
    action: &'static str,
) -> ApiResult<()> {
    let target_is_owner = tankovault_db::repo::permissions::resolve(&state.pool, target)
        .await?
        .is_some_and(|p| p.permissions.is_super_user());
    if !target_is_owner {
        return Ok(());
    }

    audit_failure(
        state,
        user,
        action,
        &target.as_uuid().to_string(),
        &serde_json::json!({ "reason": "super_user_target" }),
    )
    .await;
    Err(ApiError::BadRequest(
        "this is the deployment's super user; the account that owns the installation cannot be \
         suspended or erased from here"
            .to_owned(),
    ))
}
