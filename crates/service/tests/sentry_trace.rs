//! The outbound half of distributed tracing, with a client actually bound.
//!
//! Its own test binary, and one test in it, because both halves are process-global: the unit
//! tests in `src/sentry.rs` pin that `trace_headers()` is empty with **no** client, and
//! `init_tracing` installs a subscriber that can only be installed once.

use tankovault_config::{SentryConfig, TelemetryConfig};
use tracing::Instrument as _;

/// A DSN that parses and points nowhere. Nothing here captures an event, so the transport
/// never has anything to send and the guard's flush returns immediately.
fn telemetry() -> TelemetryConfig {
    TelemetryConfig {
        service_name: "trace-test".to_owned(),
        log_filter: "info".to_owned(),
        json_logs: false,
        sentry: SentryConfig {
            enabled: true,
            dsn: Some("https://0123456789abcdef@127.0.0.1/1".into()),
            traces_sample_rate: 1.0,
            shutdown_timeout_secs: 0,
            ..SentryConfig::default()
        },
    }
}

/// The claims the cross-service trace rests on, none of which the type system checks.
///
/// Everything runs inside an instrumented span, because that is what gives this task a trace
/// of its *own* — a transaction — rather than the process-wide propagation context every task
/// would otherwise share. Without that, each assertion below passes for the wrong reason.
#[tokio::test]
async fn a_trace_survives_every_hop_that_has_to_carry_it() {
    let _telemetry = tankovault_service::init_tracing(&telemetry()).expect("telemetry installs");
    hops().instrument(tracing::info_span!("unit_of_work")).await;
}

async fn hops() {
    // 1. A bound client can always name the trace it is in. Without this, the API's call to
    //    `sync` and the scan task the control-plane queues each start a trace of their own,
    //    and one reader action is unreadable across nine binaries.
    let here = trace().expect("a span under a bound client is a trace that can be handed on");
    assert!(!here.is_empty(), "an empty header is not a trace");

    // 2. `propagate_trace` writes what `trace_headers` returns, *replacing* what was there.
    //    The two are used on different hops — a `reqwest` builder, a proxied `HeaderMap` — so
    //    a divergence would break exactly one of them, silently. The replacement is what the
    //    frontend proxy relies on: an inbound value is a client's claim about the trace.
    let mut map = axum::http::HeaderMap::new();
    map.insert(
        "sentry-trace",
        "a-client-supplied-claim".parse().expect("ascii"),
    );
    tankovault_service::propagate_trace(&mut map);
    assert_eq!(
        map.get("sentry-trace").and_then(|v| v.to_str().ok()),
        Some(here.as_str()),
        "an inbound value must be replaced by this process's, not merged with it"
    );

    // 3. `in_current_trace` survives a `tokio::spawn`. The Sentry hub is thread-local and a
    //    spawned task starts with a fresh one, so detached work — the targeted sync push, a
    //    transactional email — falls back to the process-wide context and reports under a
    //    trace nothing else is in. This failing means every detached hop became an orphan.
    let carried = tokio::spawn(tankovault_service::in_current_trace(async { trace() }))
        .await
        .expect("the task ran");
    assert_eq!(
        carried.as_deref(),
        Some(here.as_str()),
        "detached work must stay in the trace that spawned it"
    );

    // ...and the other half of the same claim, which is what makes the wrapper load-bearing
    // rather than decorative: a bare spawn does *not* keep it. Deterministic rather than racy
    // — `Instrumented` exits the span before returning `Pending`, so the spawned task is polled
    // with no span on the thread whichever runtime this is.
    let dropped = tokio::spawn(async { trace() }).await.expect("the task ran");
    assert_ne!(
        dropped.as_deref(),
        Some(here.as_str()),
        "a bare spawn is supposed to lose the hub; if it no longer does, this test has stopped          proving anything about `in_current_trace`"
    );
}

/// The `sentry-trace` value for whatever hub is current on this task.
fn trace() -> Option<String> {
    tankovault_service::trace_headers()
        .into_iter()
        .find(|(name, _)| *name == "sentry-trace")
        .map(|(_, value)| value)
}
