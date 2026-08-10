//! The reader's global provider order — which source a series should open on when no per-series
//! pin says otherwise.
//!
//! Deliberately ungated: it shapes outbound links and nothing else, has no delivery cost behind
//! it, and switching it off would leave readers with an order they cannot change. The per-series
//! half lives on the watchlist entry ([`super::watchlist::put_source_pin`]) and inherits that
//! surface's `tracking.watchlist` gate.

use crate::error::{ApiError, ApiResult};
use crate::openapi::ME_ACCOUNT_TAG;
use crate::state::{AppState, AuthUser};
use axum::Json;
use axum::extract::State;
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use tankovault_domain::ProviderId;
use utoipa::ToSchema;

/// One provider in the reader's order.
#[derive(Debug, Serialize, ToSchema)]
pub struct PreferredProvider {
    pub id: ProviderId,
    /// The provider slug — the same stable client-side key the source lists are drawn with.
    pub slug: String,
    pub name: String,
}

/// The reader's provider order, most preferred first, plus their early-access opt-ins.
#[derive(Debug, Serialize, ToSchema)]
pub struct SourcePreferences {
    /// Ranked providers only. An absent provider is "no opinion", not "last": series carried by
    /// nobody on this list resolve by the objective richest-source order instead.
    pub providers: Vec<PreferredProvider>,
    /// Providers whose paid early-access chapters this reader wants counted.
    ///
    /// Off by default and per provider, because paying one scanlator for early access says
    /// nothing about any other: applying it globally would put chapters the reader cannot open
    /// back into their unread count, which is the failure the early-access model exists to fix.
    pub early_access_providers: Vec<PreferredProvider>,
}

/// A replacement provider order, and optionally a replacement early-access set.
#[derive(Debug, Deserialize, ToSchema)]
pub struct SourcePreferencesUpdate {
    /// Provider ids, most preferred first. Must be distinct and must all exist; an empty list
    /// clears the preference.
    pub provider_ids: Vec<ProviderId>,
    /// Provider ids whose early-access chapters should count for this reader.
    ///
    /// **Omitted leaves the current set untouched**; `[]` clears it. The distinction matters
    /// because this field was added after the endpoint shipped — treating an absent field as an
    /// empty list would silently revoke the opt-in of every client that has not been updated.
    #[serde(default)]
    pub early_access_provider_ids: Option<Vec<ProviderId>>,
}

/// Get source preferences
///
/// The caller's provider order. Providers that have since been disabled are dropped rather than
/// returned — they carry nothing a reader can open, so ranking them would be a preference that
/// can never apply.
#[utoipa::path(
    get,
    path = "/v1/me/source-preferences",
    tag = ME_ACCOUNT_TAG,
    security(("bearer_auth" = [])),
    responses(
        (status = 200, description = "The caller's provider order", body = SourcePreferences),
        (status = 401, description = "authentication required", body = crate::error::ProblemDetails),
    )
)]
pub async fn source_preferences(
    State(state): State<AppState>,
    user: AuthUser,
) -> ApiResult<Json<SourcePreferences>> {
    let ranked =
        tankovault_db::repo::users::get_provider_priority(&state.pool, user.user_id).await?;
    let early = tankovault_db::repo::users::get_early_access_providers(&state.pool, user.user_id)
        .await?;

    // Resolved against the public list so a provider that has since been disabled drops out of
    // both answers, exactly as the ranked half already documents.
    let public = tankovault_db::repo::providers::list_public(&state.pool).await?;
    let early_set: HashSet<uuid::Uuid> = early.into_iter().collect();
    let early_access_providers = public
        .into_iter()
        .filter(|p| early_set.contains(&p.id))
        .map(|p| PreferredProvider {
            id: ProviderId::from_uuid(p.id),
            slug: p.slug,
            name: p.name,
        })
        .collect();

    Ok(Json(SourcePreferences {
        providers: ranked
            .into_iter()
            .map(|p| PreferredProvider {
                id: ProviderId::from_uuid(p.id),
                slug: p.slug,
                name: p.name,
            })
            .collect(),
        early_access_providers,
    }))
}

/// Replace source preferences
///
/// Replaces the order wholesale: the list *is* the preference, so a provider left out of the
/// body is unranked afterwards.
#[utoipa::path(
    put,
    path = "/v1/me/source-preferences",
    tag = ME_ACCOUNT_TAG,
    request_body = SourcePreferencesUpdate,
    security(("bearer_auth" = [])),
    responses(
        (status = 200, description = "The stored order, resolved to names", body = SourcePreferences),
        (status = 400, description = "a duplicate or unknown provider id", body = crate::error::ProblemDetails),
        (status = 401, description = "authentication required", body = crate::error::ProblemDetails),
    )
)]
pub async fn put_source_preferences(
    State(state): State<AppState>,
    user: AuthUser,
    Json(body): Json<SourcePreferencesUpdate>,
) -> ApiResult<Json<SourcePreferences>> {
    // Both checks are the caller's mistake, not the database's: without them a duplicate lands
    // as a primary-key violation and an unknown id as a foreign-key violation, and a 500 tells
    // nobody which id was wrong. Validating against the public list also keeps a disabled
    // provider — invisible everywhere else — from being ranked.
    let known = tankovault_db::repo::providers::list_public(&state.pool).await?;
    let known: HashSet<uuid::Uuid> = known.into_iter().map(|p| p.id).collect();
    let mut seen = HashSet::with_capacity(body.provider_ids.len());
    for id in &body.provider_ids {
        if !known.contains(&id.as_uuid()) {
            return Err(ApiError::BadRequest(format!("unknown provider {id}")));
        }
        if !seen.insert(id.as_uuid()) {
            return Err(ApiError::BadRequest(format!("duplicate provider {id}")));
        }
    }

    // The early-access set is validated against the same public list: an opt-in for a provider
    // the reader cannot see would be a preference that can never apply, and it feeds a
    // predicate rather than only a link, so a stale id there changes unread counts.
    if let Some(early) = &body.early_access_provider_ids {
        let mut seen = HashSet::with_capacity(early.len());
        for id in early {
            if !known.contains(&id.as_uuid()) {
                return Err(ApiError::BadRequest(format!("unknown provider {id}")));
            }
            if !seen.insert(id.as_uuid()) {
                return Err(ApiError::BadRequest(format!("duplicate provider {id}")));
            }
        }
    }

    tankovault_db::repo::users::set_provider_priority(
        &state.pool,
        user.user_id,
        &body.provider_ids,
    )
    .await?;
    if let Some(early) = &body.early_access_provider_ids {
        tankovault_db::repo::users::set_early_access_providers(
            &state.pool,
            user.user_id,
            early,
        )
        .await?;
    }
    source_preferences(State(state), user).await
}
