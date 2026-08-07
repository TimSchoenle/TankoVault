//! `GET /v1/me/capabilities` — the caller's permissions and this deployment's enabled features,
//! served together so a client never holds one stale against the other.
//!
//! Not carried in the token (a revoked permission used to stay live until it expired); the
//! server never trusts this response for authorization.

use crate::error::ApiResult;
use crate::openapi::ME_ACCOUNT_TAG;
use crate::state::{AppState, AuthUser};
use axum::Json;
use axum::extract::State;
use serde::Serialize;
use tankovault_domain::{Feature, Permission, PermissionSet};
use utoipa::ToSchema;

/// The caller's capabilities and the deployment's enabled features.
#[derive(Debug, Serialize, ToSchema)]
pub struct Capabilities {
    /// The permissions this caller currently holds, in registry order.
    #[schema(value_type = Vec<Permission>)]
    pub permissions: PermissionSet,
    /// Every feature switched on for this deployment, in registry order.
    ///
    /// Sent as the *enabled* list rather than a key→bool map: the client asks "may I show
    /// this", which is a membership test, and a map would invite it to distinguish
    /// "explicitly false" from "absent" — a distinction that exists in the control plane's
    /// override table and nowhere else.
    pub features: Vec<Feature>,
}

/// Get my capabilities
///
/// The permissions the caller holds and the features this deployment has enabled — everything
/// a client needs to decide which navigation, panels and controls to render.
///
/// Not cacheable: a permission revoked or a feature switched off must be reflected on the next
/// fetch, and this is the endpoint a client polls to find out.
///
/// The grants as stored, not as they resolve: a super user's list is the single
/// `system.superuser` token, and a client must treat that token the way the server does —
/// as holding everything — rather than reading the array as exhaustive.
#[utoipa::path(
    get,
    path = "/v1/me/capabilities",
    tag = ME_ACCOUNT_TAG,
    security(("bearer_auth" = [])),
    responses(
        (status = 200, description = "Held permissions and enabled features", body = Capabilities),
        (status = 401, description = "authentication required", body = crate::error::ProblemDetails),
        (status = 403, description = "the account is suspended", body = crate::error::ProblemDetails),
    )
)]
pub async fn capabilities(
    State(state): State<AppState>,
    user: AuthUser,
) -> ApiResult<Json<Capabilities>> {
    // No permission check: requiring one to read your own permissions would be circular.
    Ok(Json(Capabilities {
        permissions: user.permissions,
        features: state.features.enabled_features(),
    }))
}
