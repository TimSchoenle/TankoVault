//! Sentry error reporting and performance tracing, per service.

use secrecy::SecretString;
use serde::Deserialize;
use terrace_config::schema::Describe;

/// How much of the `tracing` stream one Sentry sink takes.
///
/// Ordered by severity, so a threshold names the *least* severe record it accepts:
/// `warn` means `error` and `warn`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize, Describe)]
#[serde(rename_all = "lowercase")]
pub enum SentryLevel {
    /// Take nothing.
    Off,
    /// `error` only.
    #[default]
    Error,
    /// `error` and `warn`.
    Warn,
    /// Down to `info`.
    Info,
    /// Down to `debug`.
    Debug,
    /// Everything.
    Trace,
}

/// Sentry error reporting and performance tracing.
///
/// Off by default and off in every published compose default: a DSN is an egress destination
/// for whatever a log line happens to carry, so turning it on is an operator's decision made
/// once per deployment. When [`Self::enabled`] is set the service refuses to boot without a
/// usable [`Self::dsn`] rather than starting with a reporter that reports nothing — the same
/// rule that removed `telemetry.otlp_endpoint`.
#[derive(Debug, Clone, Deserialize, Describe)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "independent operator toggles, one per TANKOVAULT_TELEMETRY__SENTRY__* variable"
)]
pub struct SentryConfig {
    /// Initialise the Sentry client. `false` installs no client, no panic hook and no layer,
    /// so every other key here is inert and nothing is sent anywhere.
    #[serde(default)]
    pub enabled: bool,
    /// Ingest URL, `https://<key>@<host>/<project>`.
    ///
    /// A [`SecretString`]: the embedded key is a bearer credential for the project's ingest
    /// endpoint, and this struct is nested in a `TelemetryConfig` that is logged with `?`.
    /// Absent while [`Self::enabled`] is set is a boot failure, not a silent no-op.
    #[config(secret)]
    #[serde(default)]
    pub dsn: Option<SecretString>,
    /// Environment tag on every event. Defaults to `production` under
    /// `TANKOVAULT_PROFILE=production` and `development` otherwise.
    #[serde(default)]
    pub environment: Option<String>,
    /// Release tag on every event; defaults to the workspace version the binary was built
    /// from, which is what makes a regression attributable to a deploy.
    #[serde(default)]
    pub release: Option<String>,
    /// Host tag on every event. Left unset, Sentry reports none: the hostname of a replica is
    /// infrastructure detail that [`Self::send_default_pii`] would otherwise gate.
    #[serde(default)]
    pub server_name: Option<String>,
    /// Fraction of captured events actually sent, `0.0`-`1.0`. A blunt volume cap — it drops
    /// whole issues, not repetitions of one — so leave it at `1.0` unless quota forces it.
    #[serde(default = "SentryConfig::default_sample_rate")]
    pub sample_rate: f32,
    /// Fraction of traces sampled, `0.0`-`1.0`. `0.0` (the default) disables performance
    /// tracing entirely: spans are not sampled and no transaction is started per request.
    #[serde(default)]
    pub traces_sample_rate: f32,
    /// Least severe `tracing` level reported as a Sentry **issue**.
    #[serde(default)]
    pub capture_level: SentryLevel,
    /// Least severe `tracing` level kept as a **breadcrumb** — the trail attached to the next
    /// issue. Records at or above [`Self::capture_level`] become issues instead.
    #[serde(default = "SentryConfig::default_breadcrumb_level")]
    pub breadcrumb_level: SentryLevel,
    /// How many breadcrumbs one event carries.
    #[serde(default = "SentryConfig::default_max_breadcrumbs")]
    pub max_breadcrumbs: usize,
    /// Attach a stack trace to events that carry none of their own.
    #[serde(default = "crate::default_true")]
    pub attach_stacktraces: bool,
    /// Send personally identifying data with every event: the client IP, the full request
    /// header set (`Authorization` and `Cookie` included) and the resolved user.
    ///
    /// **Off, and worth leaving off.** A reader's IP address and session cookie are exactly
    /// what a crash report does not need in order to be actionable, and Sentry is a third
    /// party for the purposes of the data policy this deployment publishes. On, it also
    /// widens what the HTTP middleware records, because `sentry-tower` reads this same flag
    /// to decide whether to redact sensitive headers.
    #[serde(default)]
    pub send_default_pii: bool,
    /// Record request spans through the shared HTTP stack: one Sentry transaction per
    /// request, named by the *matched route* rather than the URI, so an id in the path does
    /// not become its own transaction name. Inert while [`Self::traces_sample_rate`] is `0.0`.
    #[serde(default = "crate::default_true")]
    pub http_transactions: bool,
    /// Copy `tracing` span fields onto the Sentry span as attributes. Off: span fields in
    /// this workspace routinely carry ids and user-supplied titles, and a transaction is
    /// stored under a longer retention than a log line.
    #[serde(default)]
    pub span_attributes: bool,
    /// How long process exit waits for queued events to drain.
    #[serde(default = "SentryConfig::default_shutdown_timeout_secs")]
    pub shutdown_timeout_secs: u64,
    /// Print the SDK's own diagnostics to stderr. For proving a DSN works, not for running.
    #[serde(default)]
    pub debug: bool,
}

impl SentryConfig {
    fn default_sample_rate() -> f32 {
        1.0
    }
    fn default_breadcrumb_level() -> SentryLevel {
        SentryLevel::Info
    }
    fn default_max_breadcrumbs() -> usize {
        100
    }
    fn default_shutdown_timeout_secs() -> u64 {
        2
    }
}

impl Default for SentryConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            dsn: None,
            environment: None,
            release: None,
            server_name: None,
            sample_rate: Self::default_sample_rate(),
            traces_sample_rate: 0.0,
            capture_level: SentryLevel::Error,
            breadcrumb_level: Self::default_breadcrumb_level(),
            max_breadcrumbs: Self::default_max_breadcrumbs(),
            attach_stacktraces: true,
            send_default_pii: false,
            http_transactions: true,
            span_attributes: false,
            shutdown_timeout_secs: Self::default_shutdown_timeout_secs(),
            debug: false,
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::{SentryLevel, TelemetryConfig, load};
    use secrecy::ExposeSecret as _;
    use serde::Deserialize;
    use terrace_config::testing::Harness;

    #[derive(Debug, Deserialize)]
    struct Sample {
        telemetry: TelemetryConfig,
    }

    fn harness() -> Harness {
        Harness::over(crate::terrace())
    }

    /// A deployment that says nothing about Sentry gets no client and no egress. The whole
    /// block is `#[serde(default)]` twice over — once on `telemetry.sentry`, once per key — so
    /// a missing section must still materialise, rather than failing the boot of every service
    /// that has not been told about this feature.
    #[test]
    fn an_unmentioned_section_is_off() {
        harness().run(|jail| {
            jail.env_key("telemetry.service_name", "api");

            let cfg: Sample = load()?;
            assert!(!cfg.telemetry.sentry.enabled);
            assert!(cfg.telemetry.sentry.dsn.is_none());
            assert!((cfg.telemetry.sentry.traces_sample_rate - 0.0).abs() < f32::EPSILON);
            assert!(!cfg.telemetry.sentry.send_default_pii);
            assert_eq!(cfg.telemetry.sentry.capture_level, SentryLevel::Error);
            assert_eq!(cfg.telemetry.sentry.breadcrumb_level, SentryLevel::Info);
            Ok(())
        });
    }

    /// The keys are two levels deep, which is one level deeper than every other block: the
    /// loader has to reach `TANKOVAULT_TELEMETRY__SENTRY__*`, and a DSN mounted as a file has
    /// to outrank the environment the same way a database URL does.
    #[test]
    fn the_nested_keys_resolve_through_the_dialect() {
        harness().run(|jail| {
            jail.env_key("telemetry.service_name", "api");
            jail.env_key("telemetry.sentry.enabled", true);
            jail.env_key("telemetry.sentry.traces_sample_rate", "0.25");
            jail.env_key("telemetry.sentry.capture_level", "warn");
            jail.secret_key("telemetry.sentry.dsn", "https://key@sentry.example/42\n")?;

            let cfg: Sample = load()?;
            let sentry = &cfg.telemetry.sentry;
            assert!(sentry.enabled);
            assert_eq!(
                sentry
                    .dsn
                    .as_ref()
                    .expect("the mounted DSN is read")
                    .expose_secret(),
                "https://key@sentry.example/42"
            );
            assert!((sentry.traces_sample_rate - 0.25).abs() < f32::EPSILON);
            assert_eq!(sentry.capture_level, SentryLevel::Warn);
            Ok(())
        });
    }
}
