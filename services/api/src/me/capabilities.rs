//! `GET /v1/me/capabilities` — what this caller may do, and what this deployment offers.
//!
//! # Why one endpoint for both
//!
//! A client has to answer two questions before it can render anything: *am I allowed to do
//! this* (permissions) and *does this deployment even have this* (feature flags). Both change
//! independently of the session, both are needed at exactly the same moment — the first paint
//! after sign-in — and neither is derivable from the other. Serving them together means the
//! client makes one request and can never hold a permission list from one instant next to a
//! flag set from another.
//!
//! # Why not claims in the token
//!
//! Because the previous design did exactly that with the RBAC role and it was wrong twice over:
//! a revoked privilege stayed live until the token expired, and the client's copy could not be
//! refreshed without minting a new token. This response is a plain read, so the client can
//! refetch it whenever it likes — and, crucially, the server does not trust it: the numbers
//! here are for *drawing the UI*, and every action is authorized again by the handler that
//! performs it. Hiding a control the server would refuse anyway is a courtesy, never the
//! security boundary.

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
    // No permission check: every authenticated caller is entitled to know what they can do.
    // Requiring a permission to read one's own permissions would be circular.
    Ok(Json(Capabilities {
        permissions: user.permissions,
        features: state.features.enabled_features(),
    }))
}
