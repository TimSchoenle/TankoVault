//! Sentry error reporting and performance tracing, per service.

use secrecy::SecretString;
use serde::Deserialize;
use terrace_config::schema::Describe;

/// How much of the `tracing` stream one Sentry sink takes.
///
/// A threshold names the least severe record it accepts, so `warn` takes `error` and `warn`.
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
/// Off by default, and in every published compose file, because a DSN is an egress destination
/// for whatever a log line carries. When [`Self::enabled`] is set, the service refuses to boot
/// without a usable [`Self::dsn`].
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
    /// Required while [`Self::enabled`] is set.
    // A `SecretString` because the embedded key is a bearer credential for the project's ingest
    // endpoint, and `TelemetryConfig` is logged with `?`.
    #[config(secret)]
    #[serde(default)]
    pub dsn: Option<SecretString>,
    /// Environment tag on every event. Defaults to `production` under
    /// `TANKOVAULT_PROFILE=production` and `development` otherwise.
    #[serde(default)]
    pub environment: Option<String>,
    /// Release tag on every event. Unset, it is the workspace version the binary was built
    /// from, which ties an issue to a deploy.
    #[serde(default)]
    pub release: Option<String>,
    /// Host tag on every event. Unset, Sentry reports none.
    #[serde(default)]
    pub server_name: Option<String>,
    /// Fraction of captured events sent, `0.0`-`1.0`, decided at random per event. A lower rate
    /// undercounts every issue and can miss a rare one entirely, so leave it at `1.0` unless
    /// quota forces it lower.
    #[config(range(min = 0.0, max = 1.0))]
    #[serde(default = "SentryConfig::default_sample_rate")]
    pub sample_rate: f32,
    /// Fraction of traces this service **starts** that are recorded, `0.0`-`1.0`.
    ///
    /// `0.0`, the default, starts none. The service still continues a trace that arrives already
    /// sampled on a request or a broker message. Set the same rate on every service, since the
    /// service that starts a trace decides whether it exists; `0.05`-`0.2` is an ordinary
    /// production figure.
    #[config(range(min = 0.0, max = 1.0))]
    #[serde(default)]
    pub traces_sample_rate: f32,
    /// Least severe `tracing` level reported as a Sentry **issue**.
    #[config(values)]
    #[serde(default)]
    pub capture_level: SentryLevel,
    /// Least severe `tracing` level kept as a **breadcrumb**, the trail attached to the next
    /// issue. Records at or above [`Self::capture_level`] become issues instead.
    #[config(values)]
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
    /// **Off, and worth leaving off.** A crash report does not need a reader's IP address or
    /// session cookie, and Sentry is a third party under the deployment's data policy. On, it
    /// also stops `sentry-tower` redacting sensitive headers in the HTTP middleware.
    #[serde(default)]
    pub send_default_pii: bool,
    /// Record one Sentry transaction per request through the shared HTTP stack, named by the
    /// matched route, so an id in the path does not become a transaction name.
    ///
    /// [`Self::traces_sample_rate`] decides whether a transaction is kept. Off, the HTTP layer
    /// starts no transactions; a broker delivery still starts one through the `tracing` layer.
    #[serde(default = "crate::default_true")]
    pub http_transactions: bool,
    /// Copy `tracing` span fields onto the Sentry span as attributes. Off by default, because span
    /// fields carry ids and user-supplied titles and a transaction is kept longer than a log line.
    #[serde(default)]
    pub span_attributes: bool,
    /// How long process exit waits for queued events to drain.
    #[serde(default = "SentryConfig::default_shutdown_timeout_secs")]
    pub shutdown_timeout_secs: u64,
    /// Print the SDK's own diagnostics to stderr, to prove a DSN works.
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

    /// A deployment that says nothing about Sentry boots with no client and no egress.
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

    /// The loader reaches the two-level `TANKOVAULT_TELEMETRY__SENTRY__*` keys, and a DSN mounted
    /// as a file is read like any other secret.
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
