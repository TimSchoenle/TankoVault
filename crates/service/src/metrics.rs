//! Togglable Prometheus metrics.
//!
//! [`MetricsRegistry`] is the on/off switch: when disabled, no recorder is installed and
//! `metrics::counter!` calls dispatch to a no-op, so domain code needs no `if`.

use crate::ServiceError;
use axum::extract::{MatchedPath, Request};
use axum::middleware::Next;
use axum::response::Response;
use metrics_exporter_prometheus::{Matcher, PrometheusBuilder, PrometheusHandle};
use std::sync::Arc;
use std::time::Instant;
use tankovault_config::MetricsConfig;

/// Histogram bucket boundaries for HTTP request latency, in seconds. Tight resolution
/// around the 10ms-1s band where most handlers live.
const LATENCY_BUCKETS: &[f64] = &[
    0.001, 0.005, 0.010, 0.025, 0.050, 0.100, 0.250, 0.500, 1.0, 2.5, 5.0, 10.0,
];

/// Total HTTP requests, labelled by method, matched route and the exact status code.
const HTTP_REQUESTS: &str = "http_requests_total";
/// HTTP request latency in seconds, labelled by method and matched route.
const HTTP_DURATION: &str = "http_request_duration_seconds";
/// HTTP requests currently being served.
const HTTP_IN_FLIGHT: &str = "http_requests_in_flight";

/// A cheap, cloneable handle onto the process's metrics facility — or onto nothing, when
/// metrics are switched off.
#[derive(Clone)]
pub struct MetricsRegistry {
    /// `None` when disabled. Its presence *is* the enabled flag; there is no separate
    /// boolean that could disagree with whether a recorder was actually installed.
    handle: Option<PrometheusHandle>,
    http_requests: bool,
    route: Arc<str>,
    /// When `Some`, the scrape endpoint is served on this dedicated address instead of the
    /// service's primary port. `None` keeps it merged onto the main port.
    listen: Option<Arc<str>>,
}

impl MetricsRegistry {
    /// Install the process-wide Prometheus recorder if `cfg` enables it.
    ///
    /// Uses explicit latency buckets, not the exporter's default quantiles: quantiles
    /// cannot be averaged across replicas by a recording rule, buckets can.
    ///
    /// # Errors
    /// Returns [`ServiceError::Metrics`] if a recorder is already installed.
    pub fn install(cfg: &MetricsConfig) -> Result<Self, ServiceError> {
        if !cfg.enabled {
            tracing::info!("metrics disabled by configuration; no recorder installed");
            return Ok(Self::disabled());
        }

        let handle = PrometheusBuilder::new()
            .set_buckets_for_metric(Matcher::Full(HTTP_DURATION.to_owned()), LATENCY_BUCKETS)
            .map_err(|e| ServiceError::Metrics(e.to_string()))?
            .install_recorder()
            .map_err(|e| ServiceError::Metrics(e.to_string()))?;

        tracing::info!(
            route = %cfg.route,
            http_requests = cfg.http_requests,
            "metrics enabled"
        );
        Ok(Self {
            handle: Some(handle),
            http_requests: cfg.http_requests,
            route: Arc::from(cfg.route.as_str()),
            listen: cfg.listen.as_deref().map(Arc::from),
        })
    }

    /// A registry that records nothing — for tests and for services that opt out.
    #[must_use]
    pub fn disabled() -> Self {
        Self {
            handle: None,
            http_requests: false,
            route: Arc::from("/metrics"),
            listen: None,
        }
    }

    /// A disabled registry with an explicit scrape `route` and optional `listen` address,
    /// for exercising the router-wiring decisions without installing a global recorder.
    #[cfg(test)]
    pub(crate) fn disabled_with_listen(route: &str, listen: Option<&str>) -> Self {
        Self {
            handle: None,
            http_requests: false,
            route: Arc::from(route),
            listen: listen.map(Arc::from),
        }
    }

    /// Whether a recorder is installed and the scrape endpoint should serve.
    #[must_use]
    pub fn is_enabled(&self) -> bool {
        self.handle.is_some()
    }

    /// Whether the per-request HTTP middleware should be mounted.
    #[must_use]
    pub fn records_http_requests(&self) -> bool {
        self.handle.is_some() && self.http_requests
    }

    /// Path the scrape endpoint is mounted at.
    #[must_use]
    pub fn route(&self) -> &str {
        &self.route
    }

    /// The dedicated address the scrape endpoint should bind to, when it is isolated to its
    /// own port. `None` means the scrape stays merged onto the service's primary port.
    #[must_use]
    pub fn listen(&self) -> Option<&str> {
        self.listen.as_deref()
    }

    /// Render the Prometheus exposition text, or `None` when metrics are disabled.
    #[must_use]
    pub fn render(&self) -> Option<String> {
        self.handle.as_ref().map(PrometheusHandle::render)
    }
}

/// Holds one unit of [`HTTP_IN_FLIGHT`] for as long as it is alive.
///
/// Decrement lives in `Drop`, not at the end of [`track_request`]: a client disconnect
/// drops the service future mid-`await`, skipping any statement placed after it, which
/// previously leaked one unit per disconnect on every SSE stream.
struct InFlightGuard;

impl InFlightGuard {
    fn enter() -> Self {
        metrics::gauge!(HTTP_IN_FLIGHT).increment(1.0);
        Self
    }
}

impl Drop for InFlightGuard {
    fn drop(&mut self) {
        metrics::gauge!(HTTP_IN_FLIGHT).decrement(1.0);
    }
}

/// Record per-request metrics around the rest of the stack.
///
/// Labels use axum's [`MatchedPath`], not the concrete URI, so cardinality stays bounded by
/// the route table; unmatched (attacker-controlled) paths fold into `unmatched`. An abandoned
/// request is counted in [`HTTP_IN_FLIGHT`] but not [`HTTP_REQUESTS`]/[`HTTP_DURATION`], which
/// record from a response a dropped future never produces.
pub async fn track_request(req: Request, next: Next) -> Response {
    let method = req.method().clone();
    let route: String = req
        .extensions()
        .get::<MatchedPath>()
        .map_or_else(|| "unmatched".to_owned(), |p| p.as_str().to_owned());

    let _in_flight = InFlightGuard::enter();
    let started = Instant::now();
    let response = next.run(req).await;
    let elapsed = started.elapsed();

    let status = response.status().as_u16().to_string();
    metrics::counter!(
        HTTP_REQUESTS,
        "method" => method.to_string(),
        "route" => route.clone(),
        "status" => status,
    )
    .increment(1);
    metrics::histogram!(
        HTTP_DURATION,
        "method" => method.to_string(),
        "route" => route,
    )
    .record(elapsed.as_secs_f64());

    response
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn disabled_registry_renders_nothing() {
        let registry = MetricsRegistry::disabled();
        assert!(!registry.is_enabled());
        assert!(!registry.records_http_requests());
        assert!(registry.render().is_none());
    }

    #[test]
    fn disabled_config_installs_no_recorder() {
        let cfg = MetricsConfig {
            enabled: false,
            ..MetricsConfig::default()
        };
        let registry = MetricsRegistry::install(&cfg).expect("disabled install cannot fail");
        assert!(!registry.is_enabled());
    }

    /// Bug pinned: an inline decrement after `next.run(req).await` never ran on a dropped
    /// future, leaking one unit per abandoned SSE stream. Do not move it back inline.
    #[test]
    fn in_flight_gauge_is_released_when_the_request_future_is_dropped() {
        // A local recorder, so this test observes real gauge values without installing the
        // process-wide one the sibling tests assert is absent.
        let recorder = PrometheusBuilder::new().build_recorder();
        let handle = recorder.handle();

        metrics::with_local_recorder(&recorder, || {
            let guard = InFlightGuard::enter();
            assert_eq!(in_flight_value(&handle.render()), Some(1.0));
            // Standing in for the future being dropped mid-`await`: the guard goes out of
            // scope without `track_request` ever reaching its final statement.
            drop(guard);
            assert_eq!(in_flight_value(&handle.render()), Some(0.0));
        });
    }

    /// The unlabelled `http_requests_in_flight` sample from a Prometheus exposition body.
    fn in_flight_value(exposition: &str) -> Option<f64> {
        exposition
            .lines()
            .find_map(|line| line.strip_prefix("http_requests_in_flight "))
            .and_then(|value| value.trim().parse().ok())
    }

    #[test]
    fn http_request_tracking_requires_the_recorder() {
        // `http_requests` alone must not mount the layer: recording into a registry with
        // no installed recorder is pure overhead with nothing to scrape.
        let cfg = MetricsConfig {
            enabled: false,
            http_requests: true,
            ..MetricsConfig::default()
        };
        let registry = MetricsRegistry::install(&cfg).expect("disabled install cannot fail");
        assert!(!registry.records_http_requests());
    }
}
