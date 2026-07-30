//! Prometheus metrics facility.

use serde::Deserialize;

/// Prometheus metrics facility.
///
/// Disabling this is a real off switch, not a filter: [`Self::enabled`] gates installation
/// of the process-wide recorder itself, so with metrics off no counter/histogram storage is
/// allocated and `metrics::counter!` calls compile down to a no-op dispatch against the
/// default (dropping) recorder.
#[derive(Debug, Clone, Deserialize)]
pub struct MetricsConfig {
    /// Install the Prometheus recorder and serve the scrape endpoint. When `false` the
    /// scrape route answers `404` and no measurements are retained.
    #[serde(default = "crate::default_true")]
    pub enabled: bool,
    /// Path the scrape endpoint is mounted at.
    #[serde(default = "MetricsConfig::default_route")]
    pub route: String,
    /// Address the Prometheus scrape endpoint binds to on its **own** listener, isolating
    /// it from the service's public HTTP port. When `Some`, the scrape route is removed
    /// from the main app and served only here (defaults to `0.0.0.0:9090`), so metrics can
    /// be kept on an internal-only interface and never share the request-facing port. When
    /// `None` the scrape stays merged onto the service's primary port (the historical
    /// behaviour).
    #[serde(default = "MetricsConfig::default_listen")]
    pub listen: Option<String>,
    /// Also record per-request HTTP metrics (`http_requests_total`,
    /// `http_request_duration_seconds`) from the middleware stack. Separate from
    /// [`Self::enabled`] because the request histogram is the expensive part: a service can
    /// keep cheap domain counters while dropping per-route cardinality.
    #[serde(default = "crate::default_true")]
    pub http_requests: bool,
}

impl MetricsConfig {
    fn default_route() -> String {
        "/metrics".to_owned()
    }

    // The `Option` is not redundant: this is the `#[serde(default = ..)]` provider for an
    // `Option<String>` field, so its return type must match the field's. Unwrapping it as
    // clippy suggests would stop it compiling as a serde default.
    #[allow(
        clippy::unnecessary_wraps,
        reason = "must match the Option<String> field it defaults"
    )]
    fn default_listen() -> Option<String> {
        Some("0.0.0.0:9090".to_owned())
    }
}

impl Default for MetricsConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            route: Self::default_route(),
            listen: Self::default_listen(),
            http_requests: true,
        }
    }
}
