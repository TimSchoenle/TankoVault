//! Togglable Sentry error reporting and performance tracing.
//!
//! Off unless `telemetry.sentry.enabled` is set, and then only with a DSN: a client that
//! reports nowhere is the failure mode `telemetry.otlp_endpoint` was removed for, so the
//! combination is refused at boot rather than logged and ignored.
//!
//! Three sinks, all fed from the one client this module installs:
//! - **`tracing`** — [`tracing_layer`] turns records into issues and breadcrumbs, under the
//!   thresholds in [`SentryConfig`].
//! - **panics** — the SDK's own hook, added by `sentry::init`. Complementary to the
//!   `CatchPanicLayer` in [`crate::http`], which turns the same panic into a 500: one reports
//!   it, the other keeps the replica serving.
//! - **HTTP** — [`http_layers`], mounted by [`crate::HttpStack`].
//!
//! The extern crate is always spelled `::sentry`; the bare path is ambiguous with this module.

use std::sync::OnceLock;
use std::time::Duration;

use ::sentry::integrations::tracing::{EventFilter, SentryLayer, default_span_filter};
use secrecy::ExposeSecret as _;
use tankovault_config::{SentryConfig, SentryLevel};
use tracing::Level;
use tracing_subscriber::registry::LookupSpan;

use crate::ServiceError;

/// What [`crate::HttpStack`] mounts, decided once at boot.
///
/// Process-global because the client it describes is: `sentry::init` binds one client to
/// `Hub::main()` for the lifetime of the process, and a per-stack copy would be a second
/// source of truth for a single global. Unset until [`init`] runs, which every service does
/// before it builds a router.
static HTTP: OnceLock<HttpOptions> = OnceLock::new();

/// The two independent halves of the HTTP integration.
#[derive(Debug, Clone, Copy)]
struct HttpOptions {
    /// A client is bound, so requests get their own hub and their request metadata.
    active: bool,
    /// Additionally start one transaction per request. Whether that transaction is *kept* is
    /// the sampler's decision, not this one.
    transactions: bool,
}

/// Install the process-wide Sentry client, or nothing when it is switched off.
///
/// Returns the guard that flushes queued events on drop; the caller must hold it for the
/// lifetime of the process, which is why [`crate::init_tracing`] hands it back rather than
/// leaking it into a static.
///
/// # Errors
/// [`ServiceError::Sentry`] when `enabled` is set without a DSN, when the DSN does not parse,
/// or when a sample rate is outside `0.0..=1.0`. All three are configuration mistakes whose
/// only other outcome is a service that silently reports nothing.
pub(crate) fn init(
    cfg: &SentryConfig,
    service_name: &str,
) -> Result<Option<::sentry::ClientInitGuard>, ServiceError> {
    if !cfg.enabled {
        record_http(HttpOptions {
            active: false,
            transactions: false,
        });
        return Ok(None);
    }

    // Empty is absent, not a value. `TANKOVAULT_TELEMETRY__SENTRY__DSN=""` is what a
    // compose pass-through or an unfilled chart value produces, and it has to land on the
    // message below rather than on the parse error, which would send an operator looking
    // at their URL.
    let dsn = cfg
        .dsn
        .as_ref()
        .map(|dsn| dsn.expose_secret().trim())
        .filter(|dsn| !dsn.is_empty());
    let Some(dsn) = dsn else {
        return Err(ServiceError::Sentry(
            "telemetry.sentry.enabled is set but telemetry.sentry.dsn is empty; nothing would \
             be reported. Set the DSN or turn the section off."
                .to_owned(),
        ));
    };
    // Parsed here rather than through `ClientOptions::dsn`, which panics on a malformed value.
    // The error deliberately does not quote the DSN: it is a credential, and this message
    // reaches the log stream.
    let dsn = dsn.parse::<::sentry::types::Dsn>().map_err(|e| {
        ServiceError::Sentry(format!(
            "telemetry.sentry.dsn is not a valid Sentry DSN ({e}); expected \
             https://<key>@<host>/<project>"
        ))
    })?;

    check_rate("sample_rate", cfg.sample_rate)?;
    check_rate("traces_sample_rate", cfg.traces_sample_rate)?;

    let environment = cfg.environment.clone().unwrap_or_else(|| {
        if tankovault_config::is_production() {
            "production".to_owned()
        } else {
            "development".to_owned()
        }
    });
    // One release across the tier, not one per service: the nine images are cut from a single
    // workspace version and deployed together, so a per-service release would split one
    // regression across nine of them.
    let release = cfg
        .release
        .clone()
        .unwrap_or_else(|| format!("tankovault@{}", env!("CARGO_PKG_VERSION")));

    let mut options = ::sentry::ClientOptions::new()
        .debug(cfg.debug)
        .sample_rate(cfg.sample_rate)
        .traces_sample_rate(cfg.traces_sample_rate)
        .max_breadcrumbs(cfg.max_breadcrumbs)
        .attach_stacktrace(cfg.attach_stacktraces)
        .send_default_pii(cfg.send_default_pii)
        .shutdown_timeout(Duration::from_secs(cfg.shutdown_timeout_secs))
        .environment(environment)
        .release(release)
        // Marks our own frames as application code, so a stack trace opens on the handler
        // rather than on an axum internal.
        .in_app_include(vec!["tankovault"]);
    options.dsn = Some(dsn);
    if let Some(server) = cfg.server_name.clone() {
        options = options.server_name(server);
    }

    // Every field `apply_defaults` would otherwise fill from `SENTRY_DSN`, `SENTRY_RELEASE` or
    // `SENTRY_ENVIRONMENT` is set above. That is the point: those variables are a second
    // configuration channel that bypasses the layered loader and its shadow-key rejection, and
    // an already-set field is one it cannot reach.
    let guard = ::sentry::init(options);

    // Inherited by the per-request hubs `NewSentryLayer` clones, so every event says which of
    // the nine binaries raised it.
    ::sentry::configure_scope(|scope| scope.set_tag("service", service_name));

    record_http(HttpOptions {
        active: true,
        transactions: cfg.http_transactions,
    });

    Ok(Some(guard))
}

/// The `tracing` layer feeding the client, or `None` when Sentry is off.
///
/// Sits under the subscriber's `EnvFilter`, which is the one surprise worth knowing: a record
/// `telemetry.log_filter` (or `RUST_LOG`) drops never reaches this layer, so tightening the log
/// filter to `warn` silently removes every `info` breadcrumb.
pub(crate) fn tracing_layer<S>(cfg: &SentryConfig) -> Option<SentryLayer<S>>
where
    S: tracing::Subscriber + for<'a> LookupSpan<'a>,
{
    if !cfg.enabled {
        return None;
    }

    let capture = cfg.capture_level;
    let breadcrumb = cfg.breadcrumb_level;

    let mut layer = ::sentry::integrations::tracing::layer()
        .event_filter(move |metadata| {
            let level = *metadata.level();
            if accepts(capture, level) {
                EventFilter::Event
            } else if accepts(breadcrumb, level) {
                EventFilter::Breadcrumb
            } else {
                EventFilter::Ignore
            }
        })
        // Not additionally gated on `traces_sample_rate`. Whether a span is *recorded* is the
        // sampler's decision, and it is the one that can honour an inherited one: a service at
        // rate `0.0` starts no trace of its own but still continues a trace it was handed,
        // which is the whole of what makes one reader action readable across nine binaries.
        // Gating span creation locally would cut that trace at the first such service.
        .span_filter(default_span_filter);

    if cfg.span_attributes {
        layer = layer.enable_span_attributes();
    }
    Some(layer)
}

/// The per-request hub and the request-metadata layer, or `None` when Sentry is off.
///
/// The hub layer is not optional decoration: without a hub per request, breadcrumbs from
/// concurrently served requests all land on the main hub and every issue arrives with a trail
/// belonging to whoever else was in flight.
pub(crate) fn http_layers() -> Option<(
    ::sentry::integrations::tower::NewSentryLayer<axum::extract::Request>,
    ::sentry::integrations::tower::SentryHttpLayer,
)> {
    let options = HTTP.get().copied()?;
    if !options.active {
        return None;
    }

    // `SentryHttpLayer::new` reads `send_default_pii` off the bound client to decide whether to
    // redact sensitive request headers, so it must be built after `init`.
    let http = ::sentry::integrations::tower::SentryHttpLayer::new();
    let http = if options.transactions {
        http.enable_transaction()
    } else {
        http
    };
    Some((
        ::sentry::integrations::tower::NewSentryLayer::new_from_top(),
        http,
    ))
}

/// The trace-continuation headers for the request currently in scope: `sentry-trace`, and
/// whatever else the SDK adds to that set later.
///
/// Empty when Sentry is off, so a caller can attach the result unconditionally.
///
/// This is the half of distributed tracing the inbound layer cannot do. `SentryHttpLayer`
/// *continues* a trace it is handed; without these headers on the way out, `api`'s call to
/// `sync` starts a second, unrelated trace and the fan-out of one user action is unreadable.
#[must_use]
pub fn trace_headers() -> Vec<(&'static str, String)> {
    let mut headers = Vec::new();
    // `configure_scope` returns `()`, so the iterator has to be drained into a binding the
    // closure captures rather than returned through it.
    ::sentry::configure_scope(|scope| headers.extend(scope.iter_trace_propagation_headers()));
    headers
}

/// Overwrite the trace-continuation headers of an outbound request with this process's.
///
/// Overwrite rather than merge: on a proxy hop the inbound value is the *client's* claim about
/// the trace, and the span the next service should continue is this one's.
pub fn propagate_trace(headers: &mut axum::http::HeaderMap) {
    for (name, value) in trace_headers() {
        let (Ok(name), Ok(value)) = (
            axum::http::HeaderName::from_bytes(name.as_bytes()),
            axum::http::HeaderValue::from_str(&value),
        ) else {
            continue;
        };
        headers.insert(name, value);
    }
}

/// Carry the caller's trace onto a `tokio::spawn`ed future.
///
/// The Sentry hub is thread-local and a spawned task starts with a fresh one, so work detached
/// from a request — the targeted sync push, a transactional email, an operator-triggered sweep
/// — otherwise reports as an orphan with nothing above it, which is exactly the request that
/// explains why it ran.
///
/// Must be called on the **spawning** task: it binds the hub that is current *now*, so
/// `spawn(in_current_trace(work))` carries the trace and `spawn(async { in_current_trace(work)
/// .await })` does not.
///
/// A no-op beyond one clone while Sentry is off.
pub fn in_current_trace<F>(fut: F) -> impl std::future::Future<Output = F::Output>
where
    F: std::future::Future,
{
    ::sentry::SentryFutureExt::bind_hub(fut, ::sentry::Hub::current())
}

/// Whether a record at `level` is at least as severe as `threshold`.
///
/// `tracing::Level` orders `ERROR` lowest, so "at least as severe" is `<=`.
fn accepts(threshold: SentryLevel, level: Level) -> bool {
    let threshold = match threshold {
        SentryLevel::Off => return false,
        SentryLevel::Error => Level::ERROR,
        SentryLevel::Warn => Level::WARN,
        SentryLevel::Info => Level::INFO,
        SentryLevel::Debug => Level::DEBUG,
        SentryLevel::Trace => Level::TRACE,
    };
    level <= threshold
}

fn check_rate(name: &str, rate: f32) -> Result<(), ServiceError> {
    if (0.0..=1.0).contains(&rate) {
        Ok(())
    } else {
        Err(ServiceError::Sentry(format!(
            "telemetry.sentry.{name} must be between 0.0 and 1.0, got {rate}"
        )))
    }
}

/// First writer wins, matching the client itself: a second `init` in one process is a test
/// harness, not a reconfiguration.
fn record_http(options: HttpOptions) {
    let _ = HTTP.set(options);
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `tracing::Level` sorts `ERROR` *below* `TRACE`, so a severity threshold reads as `<=`
    /// and not `>=`. Inverting it turned `capture_level = "error"` into "capture everything",
    /// which is a bill rather than a compile error.
    #[test]
    fn a_threshold_accepts_only_levels_at_least_as_severe() {
        assert!(accepts(SentryLevel::Error, Level::ERROR));
        assert!(!accepts(SentryLevel::Error, Level::WARN));
        assert!(!accepts(SentryLevel::Error, Level::TRACE));

        assert!(accepts(SentryLevel::Info, Level::ERROR));
        assert!(accepts(SentryLevel::Info, Level::WARN));
        assert!(accepts(SentryLevel::Info, Level::INFO));
        assert!(!accepts(SentryLevel::Info, Level::DEBUG));

        for level in [
            Level::ERROR,
            Level::WARN,
            Level::INFO,
            Level::DEBUG,
            Level::TRACE,
        ] {
            assert!(!accepts(SentryLevel::Off, level));
            assert!(accepts(SentryLevel::Trace, level));
        }
    }

    /// With no client bound there is no trace to continue, and the caller attaches the result
    /// unconditionally — so this has to be empty rather than a header with an empty value,
    /// which downstream would parse as a malformed trace and log about on every request.
    #[test]
    fn trace_headers_are_empty_without_a_client() {
        assert!(trace_headers().is_empty());
    }

    #[test]
    fn a_sample_rate_outside_the_unit_interval_is_refused() {
        assert!(check_rate("sample_rate", 0.0).is_ok());
        assert!(check_rate("sample_rate", 1.0).is_ok());
        assert!(check_rate("sample_rate", -0.1).is_err());
        assert!(check_rate("sample_rate", 1.1).is_err());
    }

    /// The disabled path must install no client at all — not a client with an empty DSN,
    /// which still starts a transport thread and still queues events.
    #[test]
    fn disabled_installs_no_client() {
        let cfg = SentryConfig::default();
        assert!(!cfg.enabled);
        assert!(tracing_layer::<tracing_subscriber::Registry>(&cfg).is_none());
    }

    #[test]
    fn enabled_without_a_dsn_is_a_boot_failure() {
        let cfg = SentryConfig {
            enabled: true,
            ..SentryConfig::default()
        };
        let Err(err) = init(&cfg, "test") else {
            panic!("a client with no DSN reports nowhere and must not be installed")
        };
        assert!(err.to_string().contains("dsn"), "{err}");
    }

    /// A pass-through that resolved to nothing — `TANKOVAULT_TELEMETRY__SENTRY__DSN=""`
    /// from the compose file, an unfilled chart value — must read as *absent*, not as a
    /// DSN that fails to parse. The two produce very different messages, and only one of
    /// them sends the operator to the right place.
    #[test]
    fn an_empty_dsn_reads_as_absent_rather_than_malformed() {
        let cfg = SentryConfig {
            enabled: true,
            dsn: Some("   ".into()),
            ..SentryConfig::default()
        };
        let Err(err) = init(&cfg, "test") else {
            panic!("a blank DSN reports nowhere either")
        };
        assert!(err.to_string().contains("is empty"), "{err}");
    }
}
