//! The two refusals every account-writing path shares.
//!
//! They are in their own module for the same reason [`crate::auth::validate`] is: they are not
//! one handler's private business. Both protect *properties of the deployment* rather than of a
//! row — no database constraint can express "somebody other than the caller must still be able
//! to grant permissions" — so every path that writes the columns involved has to check, and a
//! path that forgets is exactly the class of defect SEC-9 was. Status, permissions and erasure
//! all call them today; anything added later that touches the same columns must too.
//!
//! Both audit their refusal before returning it. An attempt to erase the last administrator is
//! precisely the event an operator wants to find afterwards.

use crate::audit::audit_failure;
use crate::error::{ApiError, ApiResult};
use crate::state::{AppState, AuthUser};
use tankovault_domain::{Permission, UserId};

/// Refuse an administrative action aimed at the caller's own account.
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

/// Refuse an action that would leave no active holder of [`Permission::UsersPermissions`].
///
/// Checked against *other* accounts, so it permits the action whenever anyone else could still
/// administer the deployment, and refuses only when this really is the last one.
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
