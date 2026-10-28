//! Prometheus metrics facility.

use serde::Deserialize;
use terrace_config::schema::Describe;

/// Prometheus metrics facility.
///
/// [`Self::enabled`] decides whether the recorder is installed at all; without it every
/// `metrics::counter!` call does nothing.
#[derive(Debug, Clone, Deserialize, Describe)]
pub struct MetricsConfig {
    /// Install the Prometheus recorder and serve the scrape endpoint. When `false` the
    /// scrape route answers `404` and no measurements are retained.
    #[serde(default = "crate::default_true")]
    pub enabled: bool,
    /// Path the scrape endpoint is mounted at.
    #[serde(default = "MetricsConfig::default_route")]
    pub route: String,
    /// Address the scrape endpoint binds on its **own** listener, apart from the service's
    /// public port (default `0.0.0.0:9090`). `null` serves it on the primary port.
    #[serde(default = "MetricsConfig::default_listen")]
    pub listen: Option<String>,
    /// Also record per-request HTTP metrics. The request histogram is the expensive,
    /// high-cardinality part, so it has its own switch.
    #[serde(default = "crate::default_true")]
    pub http_requests: bool,
}

impl MetricsConfig {
    fn default_route() -> String {
        "/metrics".to_owned()
    }

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
