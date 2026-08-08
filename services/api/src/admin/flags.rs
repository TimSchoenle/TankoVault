//! The feature-flag control plane: read the catalogue, switch a feature on or off, or reset it
//! to the value it ships with. Writing a flag refreshes this replica's gate immediately; other
//! replicas pick it up on their next refresh tick.

use crate::audit::audit;
use crate::error::{ApiError, ApiResult};
use crate::openapi::ADMIN_FLAGS_TAG;
use crate::state::{AppState, AuthUser};
use axum::Json;
use axum::extract::{Path, State};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::str::FromStr;
use tankovault_domain::{Feature, FeatureGroup, Permission};
use utoipa::ToSchema;

/// One feature as the control plane shows it.
///
/// The four booleans are not a struct that wants splitting: they answer four different
/// questions the page asks side by side — is it on, is that the shipped value, did someone
/// decide it, and can it be changed at all — and grouping any of them would only move the
/// flattening into the client.
#[expect(
    clippy::struct_excessive_bools,
    reason = "four independent facts the control plane displays together"
)]
#[derive(Debug, Serialize, ToSchema)]
pub struct FlagView {
    pub key: Feature,
    pub group: FeatureGroup,
    pub title: &'static str,
    /// What switching it off actually does, written to be read immediately before someone
    /// flips a production switch.
    pub description: &'static str,
    /// The effective state: the override if there is one, else the shipped default.
    pub enabled: bool,
    /// What this feature ships as, so the page can show "changed from default" without the
    /// client hard-coding a copy of the registry.
    pub default_enabled: bool,
    /// Whether an operator has explicitly decided this one. `false` means it is following the
    /// shipped default.
    pub overridden: bool,
    /// Whether the feature refuses to be switched off — the deployment's recovery paths. The
    /// UI disables the control; the API refuses the write regardless.
    pub locked: bool,
    /// Why the switch was last flipped, if the operator said.
    pub note: Option<String>,
    /// Username of the operator who last changed it; `None` once that account is erased.
    pub updated_by: Option<String>,
    /// When it was last changed. Absent while the feature is at its default.
    pub updated_at: Option<String>,
}

/// List feature flags
///
/// Every feature this build defines, with its effective state, its shipped default, and who
/// last changed it. Served from the compiled registry joined to the stored overrides, so the
/// page can never list a feature that does nothing or omit one that does.
#[utoipa::path(
    get,
    path = "/v1/admin/feature-flags",
    tag = ADMIN_FLAGS_TAG,
    security(("bearer_auth" = [])),
    responses(
        (status = 200, description = "Every feature and its current state", body = Vec<FlagView>),
        (status = 401, description = "authentication required", body = crate::error::ProblemDetails),
        (status = 403, description = "no second factor is enrolled, or the caller does not hold the required permission", body = crate::error::ProblemDetails),
    )
)]
pub async fn list_flags(
    State(state): State<AppState>,
    user: AuthUser,
) -> ApiResult<Json<Vec<FlagView>>> {
    user.require(Permission::FlagsRead).await?;
    Ok(Json(flag_views(&state).await?))
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct SetFlag {
    pub enabled: bool,
    /// Why. Optional, but it is the thing the next operator needs and the audit record does not
    /// surface on this page.
    #[serde(default)]
    pub note: Option<String>,
}

/// Set a feature flag
///
/// Records an explicit decision for one feature and applies it immediately on this replica.
/// Writing the value a feature already has is meaningful: it pins the feature against a future
/// change of the shipped default.
#[utoipa::path(
    put,
    path = "/v1/admin/feature-flags/{key}",
    tag = ADMIN_FLAGS_TAG,
    params(("key" = String, Path, description = "Feature key, e.g. `sync.auto_push`")),
    request_body = SetFlag,
    security(("bearer_auth" = [])),
    responses(
        (status = 200, description = "Every feature and its state after the change", body = Vec<FlagView>),
        (status = 400, description = "unknown feature, or an attempt to disable a locked one", body = crate::error::ProblemDetails),
        (status = 401, description = "authentication required", body = crate::error::ProblemDetails),
        (status = 403, description = "no second factor is enrolled, a step-up is required, or the caller does not hold the required permission", body = crate::error::ProblemDetails),
    )
)]
pub async fn set_flag(
    State(state): State<AppState>,
    user: AuthUser,
    Path(key): Path<String>,
    Json(body): Json<SetFlag>,
) -> ApiResult<Json<Vec<FlagView>>> {
    user.require(Permission::FlagsWrite).await?;
    let feature = parse_feature(&key)?;

    // Refused here as well as ignored by the gate. The gate ignoring a stored override is a
    // safety net for a row that should not exist; this is the door it should not get through.
    if feature.is_locked() && !body.enabled {
        return Err(ApiError::BadRequest(format!(
            "\"{}\" cannot be switched off: {}",
            feature.title(),
            feature.description()
        )));
    }

    let note = body
        .note
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty());
    tankovault_db::repo::flags::set_override(
        &state.pool,
        feature.key(),
        body.enabled,
        note,
        user.user_id,
    )
    .await?;

    audit(
        &state,
        &user,
        "flag.set",
        feature.key(),
        &serde_json::json!({ "enabled": body.enabled, "note": note }),
    )
    .await;

    // Before responding, so the caller's next request already behaves the new way.
    state.features.refresh().await;
    tracing::info!(
        feature = %feature,
        enabled = body.enabled,
        actor = %user.user_id.as_uuid(),
        "feature flag changed"
    );

    Ok(Json(flag_views(&state).await?))
}

/// Reset a feature flag
///
/// Drops the stored override so the feature follows the value it ships with. Distinct from
/// setting it to that value, which records an operator decision that would survive a future
/// change of the default.
#[utoipa::path(
    delete,
    path = "/v1/admin/feature-flags/{key}",
    tag = ADMIN_FLAGS_TAG,
    params(("key" = String, Path, description = "Feature key, e.g. `sync.auto_push`")),
    security(("bearer_auth" = [])),
    responses(
        (status = 200, description = "Every feature and its state after the reset", body = Vec<FlagView>),
        (status = 400, description = "unknown feature", body = crate::error::ProblemDetails),
        (status = 401, description = "authentication required", body = crate::error::ProblemDetails),
        (status = 403, description = "no second factor is enrolled, a step-up is required, or the caller does not hold the required permission", body = crate::error::ProblemDetails),
    )
)]
pub async fn reset_flag(
    State(state): State<AppState>,
    user: AuthUser,
    Path(key): Path<String>,
) -> ApiResult<Json<Vec<FlagView>>> {
    user.require(Permission::FlagsWrite).await?;
    let feature = parse_feature(&key)?;

    let cleared = tankovault_db::repo::flags::clear_override(&state.pool, feature.key()).await?;
    audit(
        &state,
        &user,
        "flag.reset",
        feature.key(),
        &serde_json::json!({ "cleared": cleared, "default": feature.default_enabled() }),
    )
    .await;

    state.features.refresh().await;
    Ok(Json(flag_views(&state).await?))
}

/// Pairs the compiled registry with stored overrides; iterating the registry (not the table)
/// keeps a removed feature's stale override from inventing a row nothing enforces.
async fn flag_views(state: &AppState) -> ApiResult<Vec<FlagView>> {
    let overrides = tankovault_db::repo::flags::list_overrides(&state.pool).await?;
    let by_key: HashMap<&str, &tankovault_db::repo::flags::OverrideRow> = overrides
        .iter()
        .map(|row| (row.feature_key.as_str(), row))
        .collect();

    Ok(Feature::all()
        .iter()
        .map(|feature| {
            let stored = by_key.get(feature.key()).copied();
            // A locked feature reports its default regardless of any stored row, matching what
            // the gate actually enforces — the page must not claim a state the runtime ignores.
            let enabled = if feature.is_locked() {
                feature.default_enabled()
            } else {
                stored.map_or_else(|| feature.default_enabled(), |row| row.enabled)
            };
            FlagView {
                key: *feature,
                group: feature.group(),
                title: feature.title(),
                description: feature.description(),
                enabled,
                default_enabled: feature.default_enabled(),
                overridden: stored.is_some() && !feature.is_locked(),
                locked: feature.is_locked(),
                note: stored.and_then(|row| row.note.clone()),
                updated_by: stored.and_then(|row| row.updated_by.clone()),
                updated_at: stored.and_then(|row| {
                    row.updated_at
                        .format(&time::format_description::well_known::Rfc3339)
                        .ok()
                }),
            }
        })
        .collect())
}

fn parse_feature(key: &str) -> ApiResult<Feature> {
    Feature::from_str(key).map_err(|_| ApiError::BadRequest(format!("unknown feature: {key}")))
}
