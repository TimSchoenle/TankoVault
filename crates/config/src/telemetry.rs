//! Observability settings shared by every service.

use serde::Deserialize;

/// Observability / telemetry settings shared by every service.
#[derive(Debug, Clone, Deserialize)]
pub struct TelemetryConfig {
    /// Logical service name reported in traces/metrics.
    pub service_name: String,
    /// **`TANKOVAULT_TELEMETRY__OTLP_ENDPOINT` is removed** (it silently did nothing); setting
    /// it is now a hard config error. Re-add only with a real `OpenTelemetryLayer`.
    ///
    /// `RUST_LOG`-style filter (e.g. `info,tankovault=debug`).
    #[serde(default = "TelemetryConfig::default_log_filter")]
    pub log_filter: String,
    /// Emit structured JSON logs (production) vs. pretty logs (dev).
    #[serde(default)]
    pub json_logs: bool,
}

impl TelemetryConfig {
    fn default_log_filter() -> String {
        "info".to_owned()
    }
}
