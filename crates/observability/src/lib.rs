//! # tankovault-observability
//!
//! One-call initialization of structured logging and Prometheus metrics for every
//! service. Distributed tracing spans are emitted via `tracing`; trace-context
//! propagation over NATS/HTTP is carried in message headers (see `tankovault-contracts`).
//!
//! OTLP export to an OpenTelemetry collector is planned for the hardening phase; the
//! [`tankovault_config::TelemetryConfig::otlp_endpoint`] field is honoured by logging a
//! notice until that exporter is wired. Local tracing and metrics are fully functional.

use tankovault_config::TelemetryConfig;
use metrics_exporter_prometheus::PrometheusBuilder;
pub use metrics_exporter_prometheus::PrometheusHandle;
use tracing_subscriber::EnvFilter;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;

/// Errors during telemetry initialization.
#[derive(Debug, thiserror::Error)]
pub enum ObservabilityError {
    /// The tracing subscriber was already installed (called twice).
    #[error("failed to install tracing subscriber: {0}")]
    Tracing(String),
    /// The Prometheus recorder could not be installed.
    #[error("failed to install metrics recorder: {0}")]
    Metrics(String),
}

/// Install the global `tracing` subscriber.
///
/// Emits JSON logs when [`TelemetryConfig::json_logs`] is set (production), otherwise
/// human-readable logs (development). The filter comes from
/// [`TelemetryConfig::log_filter`], overridable at runtime via `RUST_LOG`.
///
/// # Errors
/// Returns [`ObservabilityError::Tracing`] if a subscriber is already set.
pub fn init_tracing(cfg: &TelemetryConfig) -> Result<(), ObservabilityError> {
    let filter = EnvFilter::try_from_default_env()
        .or_else(|_| EnvFilter::try_new(&cfg.log_filter))
        .map_err(|e| ObservabilityError::Tracing(e.to_string()))?;

    let registry = tracing_subscriber::registry().with(filter);

    if cfg.json_logs {
        let json_layer = tracing_subscriber::fmt::layer()
            .json()
            .with_current_span(true)
            .with_span_list(false);
        registry
            .with(json_layer)
            .try_init()
            .map_err(|e| ObservabilityError::Tracing(e.to_string()))?;
    } else {
        let fmt_layer = tracing_subscriber::fmt::layer().with_target(true);
        registry
            .with(fmt_layer)
            .try_init()
            .map_err(|e| ObservabilityError::Tracing(e.to_string()))?;
    }

    if cfg.otlp_endpoint.is_some() {
        tracing::info!(
            service = %cfg.service_name,
            "OTLP endpoint configured; collector export is pending (hardening phase). \
             Local tracing is active."
        );
    }

    Ok(())
}

/// Install the process-wide Prometheus recorder and return a handle whose
/// [`PrometheusHandle::render`] produces the `/metrics` exposition text.
///
/// Services mount the handle on a `/metrics` route rather than opening a second
/// listener, keeping one port per service.
///
/// # Errors
/// Returns [`ObservabilityError::Metrics`] if a recorder is already installed.
pub fn init_metrics() -> Result<PrometheusHandle, ObservabilityError> {
    PrometheusBuilder::new()
        .install_recorder()
        .map_err(|e| ObservabilityError::Metrics(e.to_string()))
}

/// Convenience: initialize both tracing and metrics, returning the metrics handle.
///
/// # Errors
/// Propagates the first initialization failure.
pub fn init(cfg: &TelemetryConfig) -> Result<PrometheusHandle, ObservabilityError> {
    init_tracing(cfg)?;
    init_metrics()
}
