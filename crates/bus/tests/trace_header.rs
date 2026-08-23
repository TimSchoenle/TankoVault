//! The one invariant that connects the broker hop to the SDK.
//!
//! Its own test binary because `sentry::init` binds a process-global client.

use tankovault_config::{SentryConfig, TelemetryConfig};

/// [`tankovault_bus::TRACE_HEADER`] must be the header the SDK actually emits.
///
/// The two are written in different crates and nothing links them: `crates/bus` spells the name
/// as a constant, and `tankovault_service::trace_headers` gets it from the Sentry SDK. If the
/// SDK ever renames or adds to that set, every publish would keep carrying the new name while
/// every consumer kept looking for the old one — and the failure is a trace that silently stops
/// at the broker, with no error anywhere and every service still reporting normally.
#[test]
fn the_broker_header_is_the_one_the_sdk_emits() {
    let cfg = TelemetryConfig {
        service_name: "bus-trace-test".to_owned(),
        log_filter: "info".to_owned(),
        json_logs: false,
        sentry: SentryConfig {
            enabled: true,
            dsn: Some("https://0123456789abcdef@127.0.0.1/1".into()),
            traces_sample_rate: 1.0,
            shutdown_timeout_secs: 0,
            ..SentryConfig::default()
        },
    };
    let _telemetry = tankovault_service::init_tracing(&cfg).expect("telemetry installs");

    let emitted = tankovault_service::trace_headers();
    assert!(
        emitted
            .iter()
            .any(|(name, _)| *name == tankovault_bus::TRACE_HEADER),
        "the SDK emits {:?}, and this crate publishes and reads `{}`",
        emitted.iter().map(|(name, _)| *name).collect::<Vec<_>>(),
        tankovault_bus::TRACE_HEADER,
    );
}
