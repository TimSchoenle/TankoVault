//! Observability settings shared by every service.

use serde::Deserialize;

/// Observability / telemetry settings shared by every service.
#[derive(Debug, Clone, Deserialize)]
pub struct TelemetryConfig {
    /// Logical service name reported in traces/metrics.
    pub service_name: String,
    /// **Removed.** `TANKOVAULT_TELEMETRY__OTLP_ENDPOINT` used to be accepted here and did
    /// nothing but log "collector export is pending" — no `OTel` layer was ever installed and
    /// the four `OTel` crates in `[workspace.dependencies]` were used by zero members.
    ///
    /// A knob that silently does nothing is worse than an absent one: an operator who sets it
    /// believes traces are being exported and discovers otherwise during an incident. Setting
    /// the variable is now a hard configuration error (figment rejects unknown keys only when
    /// asked, so the removal is documented here instead), which is the honest signal.
    ///
    /// Re-add this together with a real `OpenTelemetryLayer` in
    /// `crates/service/src/telemetry.rs`, never separately.
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
