//! Observability settings shared by every service.

use serde::Deserialize;
use terrace_config::schema::Describe;

use crate::sentry::SentryConfig;

/// Observability settings shared by every service.
#[derive(Debug, Clone, Deserialize, Describe)]
pub struct TelemetryConfig {
    /// Service name reported in traces and metrics.
    pub service_name: String,
    /// `tracing` filter directives, such as `info,tankovault=debug`. `RUST_LOG` wins when set.
    #[serde(default = "TelemetryConfig::default_log_filter")]
    pub log_filter: String,
    /// Emit structured JSON logs, for production, in place of the human-readable format.
    #[serde(default)]
    pub json_logs: bool,
    /// Sentry error reporting and performance tracing, off unless configured.
    ///
    /// Sentry reads the same `tracing` stream as the logs, so a record `log_filter` drops never
    /// reaches it.
    #[serde(default)]
    #[config(nested)]
    pub sentry: SentryConfig,
}

impl TelemetryConfig {
    fn default_log_filter() -> String {
        "info".to_owned()
    }
}
