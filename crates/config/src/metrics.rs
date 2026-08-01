//! Prometheus metrics facility.

use serde::Deserialize;

/// Prometheus metrics facility.
///
/// A real off switch, not a filter: [`Self::enabled`] gates installing the recorder itself,
/// so `metrics::counter!` calls compile to a no-op rather than merely being filtered.
#[derive(Debug, Clone, Deserialize)]
pub struct MetricsConfig {
    /// Install the Prometheus recorder and serve the scrape endpoint. When `false` the
    /// scrape route answers `404` and no measurements are retained.
    #[serde(default = "crate::default_true")]
    pub enabled: bool,
    /// Path the scrape endpoint is mounted at.
    #[serde(default = "MetricsConfig::default_route")]
    pub route: String,
    /// Address the scrape endpoint binds to on its **own** listener (default `0.0.0.0:9090`),
    /// isolated from the service's public port. `None` merges it onto the primary port.
    #[serde(default = "MetricsConfig::default_listen")]
    pub listen: Option<String>,
    /// Also record per-request HTTP metrics. Separate from [`Self::enabled`]: the request
    /// histogram is the expensive, high-cardinality part.
    #[serde(default = "crate::default_true")]
    pub http_requests: bool,
}

impl MetricsConfig {
    fn default_route() -> String {
        "/metrics".to_owned()
    }

    // Must return `Option<String>` to match the field's serde-default signature; unwrapping
    // as clippy suggests would break that.
    #[expect(
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
