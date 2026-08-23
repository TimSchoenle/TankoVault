//! Structured logging installation, and the Sentry client that shares its record stream.
//!
//! Metrics live in [`crate::metrics`]; the two were previously initialised by one
//! `observability::init` call, which meant a service could not have logs without also
//! taking a metrics recorder. Splitting them is what makes metrics genuinely togglable.

use crate::ServiceError;
use tankovault_config::TelemetryConfig;
use tracing_subscriber::EnvFilter;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;

/// Keeps the Sentry client alive, and flushes what it has queued on drop.
///
/// Returned by [`init_tracing`] rather than stashed in a static because a static is never
/// dropped: the flush that gets the last events of a shutting-down replica out of the process
/// happens here, bounded by `telemetry.sentry.shutdown_timeout_secs`. Bind it (`let _telemetry
/// = …`) for the lifetime of `main`; `let _ = …` drops it immediately and closes the client
/// before the service has served anything.
#[must_use = "dropping the guard closes the Sentry client and stops reporting"]
pub struct TelemetryGuard(Option<sentry::ClientInitGuard>);

impl std::fmt::Debug for TelemetryGuard {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_tuple("TelemetryGuard")
            .field(&self.0.is_some())
            .finish()
    }
}

/// Install the global `tracing` subscriber, and the Sentry client when one is configured.
///
/// Emits JSON logs when [`TelemetryConfig::json_logs`] is set (production), otherwise
/// human-readable logs (development). The filter comes from
/// [`TelemetryConfig::log_filter`], overridable at runtime via `RUST_LOG`, and it governs the
/// Sentry layer too — a record the filter drops is not reported.
///
/// # Errors
/// Returns [`ServiceError::Tracing`] if a subscriber is already set, or
/// [`ServiceError::Sentry`] if `telemetry.sentry` is switched on but unusable.
pub fn init_tracing(cfg: &TelemetryConfig) -> Result<TelemetryGuard, ServiceError> {
    let filter = EnvFilter::try_from_default_env()
        .or_else(|_| EnvFilter::try_new(&cfg.log_filter))
        .map_err(|e| ServiceError::Tracing(e.to_string()))?;

    // Before the subscriber: the layer below reports onto the client this installs, and the
    // SDK's panic hook should be in place for anything the subscriber build itself does.
    let guard = crate::sentry::init(&cfg.sentry, &cfg.service_name)?;

    let registry = tracing_subscriber::registry()
        .with(filter)
        .with(crate::sentry::tracing_layer(&cfg.sentry));

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

    // After `try_init`, not beside `sentry::init`: a record emitted before the subscriber
    // exists goes nowhere, and "is Sentry actually on in this pod" is the first question
    // an operator asks.
    if guard.is_some() {
        tracing::info!(
            service = cfg.service_name,
            traces_sample_rate = cfg.sentry.traces_sample_rate,
            send_default_pii = cfg.sentry.send_default_pii,
            "Sentry reporting enabled"
        );
    }

    Ok(TelemetryGuard(guard))
}
