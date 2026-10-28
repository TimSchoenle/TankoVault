//! Pins the header name shared by the broker hop and the Sentry SDK.
//!
//! A separate test binary, because `sentry::init` binds a process-global client.

use tankovault_config::{SentryConfig, TelemetryConfig};

/// [`tankovault_bus::TRACE_HEADER`] is the header the SDK emits.
///
/// Nothing else links them: `crates/bus` spells the name as a constant, and
/// `tankovault_service::trace_headers` gets it from the SDK. If the SDK renamed the header,
/// publishers would send the new name while consumers looked for the old one, and every trace
/// would stop at the broker with no error anywhere.
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
