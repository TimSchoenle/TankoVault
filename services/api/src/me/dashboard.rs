//! Reading dashboard reads: continue-reading and lifetime stats.
//!
//! Recommendations moved to [`super::recommendations`] when they stopped being a stub.

use crate::error::ApiResult;
use crate::openapi::ME_DASHBOARD_TAG;
use crate::state::{AppState, AuthUser};
use crate::views::IntoView;
use axum::Json;
use axum::extract::State;
use serde::Serialize;
use tankovault_domain::SeriesId;
use utoipa::ToSchema;

#[derive(Debug, Serialize, ToSchema)]
pub struct ContinueItem {
    pub series_id: SeriesId,
    pub series_title: String,
    pub cover_url: Option<String>,
    pub last_read_number: f64,
    /// The lowest unread chapter number above the user's progress, if any.
    pub next_number: Option<f64>,
    pub unread: i64,
}

/// Get continue-reading cards
///
/// Continue-reading cards for Home / the Series CTA (frontend §9.3): tracked, in-progress
/// series that have unread chapters, freshest activity first.
#[utoipa::path(
    get,
    path = "/v1/me/continue",
    tag = ME_DASHBOARD_TAG,
    security(("bearer_auth" = [])),
    responses(
        (status = 200, description = "All continue-reading cards", body = Vec<ContinueItem>),
        (status = 401, description = "authentication required", body = crate::error::ProblemDetails),
    )
)]
pub async fn continue_reading(
    State(state): State<AppState>,
    user: AuthUser,
) -> ApiResult<Json<Vec<ContinueItem>>> {
    let cards = tankovault_db::repo::tracking::continue_reading(&state.pool, user.user_id).await?;
    let out = cards
        .into_iter()
        .map(|c| ContinueItem {
            series_id: c.series_id,
            series_title: c.series_title,
            cover_url: c.cover_url,
            last_read_number: c.last_read_number,
            next_number: c.next_number,
            unread: c.unread,
        })
        .collect();
    Ok(Json(out))
}

/// Get lifetime tracking stats
///
/// *Stub*: lifetime tracking stats for the Home / Profile headline (frontend §9.3). See
/// [`tankovault_db::repo::tracking::MeStats`] for the honest definition of `chapters_read` and
/// why no "streak" is returned.
#[utoipa::path(
    get,
    path = "/v1/me/stats",
    tag = ME_DASHBOARD_TAG,
    security(("bearer_auth" = [])),
    responses(
        (status = 200, description = "Lifetime stats", body = tankovault_contracts::me::MeStatsView),
        (status = 401, description = "authentication required", body = crate::error::ProblemDetails),
    )
)]
pub async fn stats(
    State(state): State<AppState>,
    user: AuthUser,
) -> ApiResult<Json<tankovault_contracts::me::MeStatsView>> {
    Ok(Json(
        tankovault_db::repo::tracking::me_stats(&state.pool, user.user_id)
            .await?
            .into_view(),
    ))
}
