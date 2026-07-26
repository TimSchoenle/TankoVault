//! Structured logging installation.
//!
//! Metrics live in [`crate::metrics`]; the two were previously initialised by one
//! `observability::init` call, which meant a service could not have logs without also
//! taking a metrics recorder. Splitting them is what makes metrics genuinely togglable.

use crate::ServiceError;
use tankovault_config::TelemetryConfig;
use tracing_subscriber::EnvFilter;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;

/// Install the global `tracing` subscriber.
///
/// Emits JSON logs when [`TelemetryConfig::json_logs`] is set (production), otherwise
/// human-readable logs (development). The filter comes from
/// [`TelemetryConfig::log_filter`], overridable at runtime via `RUST_LOG`.
///
/// # Errors
/// Returns [`ServiceError::Tracing`] if a subscriber is already set.
pub fn init_tracing(cfg: &TelemetryConfig) -> Result<(), ServiceError> {
    let filter = EnvFilter::try_from_default_env()
        .or_else(|_| EnvFilter::try_new(&cfg.log_filter))
        .map_err(|e| ServiceError::Tracing(e.to_string()))?;

    let registry = tracing_subscriber::registry().with(filter);

    if cfg.json_logs {
        let json_layer = tracing_subscriber::fmt::layer()
            .json()
            .with_current_span(true)
            .with_span_list(false);
        registry
            .with(json_layer)
            .try_init()
            .map_err(|e| ServiceError::Tracing(e.to_string()))?;
    } else {
        let fmt_layer = tracing_subscriber::fmt::layer().with_target(true);
        registry
            .with(fmt_layer)
            .try_init()
            .map_err(|e| ServiceError::Tracing(e.to_string()))?;
    }

    if cfg.otlp_endpoint.is_some() {
        tracing::info!(
            service = %cfg.service_name,
            "OTLP endpoint configured; collector export is pending. Local tracing is active."
        );
    }

    Ok(())
}
