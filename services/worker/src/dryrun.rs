//! The adapter dry-run: `POST /internal/providers/{id}/test`, proxied from the API. Runs
//! through [`Engine::provider_context`] so it shares the provider's cached fetch stack and
//! rate limiter with scans, rather than getting a private request budget.

use std::sync::Arc;
use std::time::Duration;

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::{Json, Router, routing::post};
use serde::Deserialize;
use tankovault_domain::ProviderId;
use tankovault_service::problem::{IntoProblem, Problem};

use crate::engine::Engine;

/// How long a dry run may take before it is abandoned.
///
/// Shorter than the API's own upstream request timeout, so a hung provider surfaces here as a
/// timeout with a useful message rather than there as an opaque gateway error.
const DRY_RUN_TIMEOUT: Duration = Duration::from_secs(25);

/// The dry run's optional input: a relative series path to fetch metadata and chapters for.
#[derive(Debug, Deserialize, Default)]
pub(crate) struct TestAdapterRequest {
    #[serde(default)]
    pub(crate) path: Option<String>,
}

/// Everything that can go wrong before a sample exists.
///
/// An adapter failure is not one of these — that's what the operator ran this to find
/// out, so it's reported in the `200` body as `{"ok": false, "error": …}`.
#[derive(Debug, thiserror::Error)]
pub(crate) enum DryRunError {
    #[error("provider not found")]
    NotFound,
    #[error("adapter build failed: {0}")]
    BuildFailed(String),
    #[error("adapter test timed out")]
    TimedOut,
    #[error(transparent)]
    Internal(#[from] anyhow::Error),
}

impl IntoProblem for DryRunError {
    fn into_problem(self) -> Problem {
        match self {
            Self::NotFound => Problem::new(StatusCode::NOT_FOUND, "not_found", self.to_string()),
            Self::BuildFailed(_) | Self::TimedOut => {
                Problem::new(StatusCode::BAD_REQUEST, "bad_request", self.to_string())
            }
            Self::Internal(e) => {
                tracing::error!(error = %e, "adapter dry run failed");
                Problem::internal()
            }
        }
    }
}

impl IntoResponse for DryRunError {
    fn into_response(self) -> Response {
        self.into_problem().into_response()
    }
}

/// The dry-run route, merged into the worker's internally-authenticated router — not
/// into `ops_router`, which stays reachable without the shared secret.
pub(crate) fn router(engine: Arc<Engine>) -> Router {
    Router::new()
        .route("/internal/providers/{id}/test", post(test_adapter))
        .with_state(engine)
}

/// Run the provider's adapter against the live site and return whatever it parsed.
///
/// The body is deliberately free-form: it is a diagnostic dump whose shape follows what the
/// adapter managed to read, and the console renders it verbatim.
async fn test_adapter(
    State(engine): State<Arc<Engine>>,
    Path(id): Path<ProviderId>,
    body: Option<Json<TestAdapterRequest>>,
) -> Result<Json<serde_json::Value>, DryRunError> {
    let req = body.map(|b| b.0).unwrap_or_default();
    let provider = tankovault_db::repo::providers::get(&engine.pool, id)
        .await
        .map_err(|e| match e {
            tankovault_db::DbError::NotFound => DryRunError::NotFound,
            other => DryRunError::Internal(other.into()),
        })?;

    let (adapter, ctx) = engine
        .provider_context(&provider)
        .map_err(|e| DryRunError::BuildFailed(e.to_string()))?;

    let sample = tokio::time::timeout(DRY_RUN_TIMEOUT, async {
        let latest = match adapter.list_latest(&ctx).await {
            Ok(items) => serde_json::json!({
                "ok": true,
                "items": items.iter().take(10).map(|u| serde_json::json!({
                    "path": u.path, "title": u.title, "latest_chapter": u.latest_chapter,
                })).collect::<Vec<_>>(),
            }),
            Err(e) => serde_json::json!({ "ok": false, "error": e.to_string() }),
        };
        let series = match req.path.as_deref() {
            Some(path) => {
                let meta = match adapter.fetch_series(&ctx, path).await {
                    Ok(m) => serde_json::json!({
                        "ok": true, "title": m.title, "alt_titles": m.alt_titles,
                        "description": m.description, "cover_url": m.cover_url, "tags": m.tags,
                        "status": m.status.as_str(), "content_type": m.content_type.as_str(),
                    }),
                    Err(e) => serde_json::json!({ "ok": false, "error": e.to_string() }),
                };
                let chapters = match adapter.fetch_chapters(&ctx, path).await {
                    Ok(list) => serde_json::json!({
                        "ok": true, "count": list.len(),
                        "sample": list.iter().take(10).map(|c| serde_json::json!({
                            "number": c.number, "title": c.title, "path": c.path,
                        })).collect::<Vec<_>>(),
                    }),
                    Err(e) => serde_json::json!({ "ok": false, "error": e.to_string() }),
                };
                Some(serde_json::json!({ "meta": meta, "chapters": chapters }))
            }
            None => None,
        };
        serde_json::json!({
            "provider": provider.slug,
            "base_url": provider.base_url,
            "latest": latest,
            "series": series,
        })
    })
    .await
    .map_err(|_| DryRunError::TimedOut)?;

    Ok(Json(sample))
}

#[cfg(test)]
mod tests {
    use super::DryRunError;
    use axum::http::StatusCode;
    use tankovault_service::problem::IntoProblem as _;

    /// The status must mean "the request was wrong", never "the adapter was" — otherwise
    /// the console shows a red banner for the answer the operator asked for.
    #[test]
    fn only_a_request_level_failure_is_an_error_status() {
        assert_eq!(
            DryRunError::NotFound.into_problem().status,
            StatusCode::NOT_FOUND
        );
        assert_eq!(
            DryRunError::BuildFailed("bad selector".to_owned())
                .into_problem()
                .status,
            StatusCode::BAD_REQUEST
        );
        assert_eq!(
            DryRunError::TimedOut.into_problem().status,
            StatusCode::BAD_REQUEST
        );
    }

    /// An internal failure must not put its `Display` on the wire: `DbError` can carry
    /// connection strings and SQL.
    #[test]
    fn an_internal_failure_discloses_nothing() {
        let problem = DryRunError::Internal(anyhow::anyhow!(
            "connection to postgres://user:hunter2@db/tankovault failed"
        ))
        .into_problem();
        assert_eq!(problem.status, StatusCode::INTERNAL_SERVER_ERROR);
        assert!(!problem.detail.contains("hunter2"));
    }
}
