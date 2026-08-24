//! What the `notifier` binary reads from its configuration.
//!
//! Public, and in a library rather than beside `main`, because it is the root
//! `config-contract` describes for this image: the contract has to be generated from the very
//! type the binary deserialises, or it is a claim about something else.

use secrecy::SecretString;
use serde::Deserialize;
use terrace_config::schema::Describe;

/// Top-level notifier config.
#[derive(Debug, Deserialize, Describe)]
pub struct Config {
    /// Where the catalogue lives, and how many connections this service may hold open.
    #[config(nested)]
    pub database: tankovault_config::DatabaseConfig,
    /// The broker this service consumes chapter events from. Required.
    #[config(nested)]
    pub nats: tankovault_config::NatsConfig,
    /// Log filter, log format and Sentry reporting.
    #[config(nested)]
    pub telemetry: tankovault_config::TelemetryConfig,
    /// Where an alert is delivered. Every channel is optional, and all of them may be absent:
    /// that leaves the in-app notification as the only delivery.
    #[serde(default)]
    #[config(nested)]
    pub channels: ChannelsConfig,
    /// The shared `TANKOVAULT_EMAIL__*` relay configuration, identical to the API's — must
    /// stay shared, or the envelope-sender policy silently diverges from the API's mail.
    #[serde(default)]
    #[config(nested)]
    pub email: tankovault_config::EmailConfig,
    /// Ops listener binding: probes and the metrics scrape, no delivery contract.
    #[serde(default = "default_bind")]
    pub bind_addr: String,
    /// Edge hardening for the ops listener.
    #[serde(default)]
    #[config(nested)]
    pub security: tankovault_config::SecurityConfig,
    /// Prometheus metrics. Togglable; disabling installs no recorder.
    #[serde(default)]
    #[config(nested)]
    pub metrics: tankovault_config::MetricsConfig,
    /// Runtime feature flags — how often this replica re-reads the operator's decisions.
    #[serde(default)]
    #[config(nested)]
    pub features: tankovault_config::FeaturesConfig,
    /// Internal-tier identity. This service serves no internal route and calls no peer, so it
    /// reads only one thing from here: the certificate material for its broker connection under
    /// `identity = "mtls"`. It is still resolved in full, so a malformed internal section is
    /// refused here exactly as it is everywhere else.
    #[serde(default)]
    #[config(nested)]
    pub internal: tankovault_config::InternalAuthConfig,
}

fn default_bind() -> String {
    "0.0.0.0:8082".to_owned()
}

/// Operator-configured external channel endpoints. All optional; a channel is only
/// constructed when its URL is present and non-empty.
#[derive(Debug, Deserialize, Describe)]
pub struct ChannelsConfig {
    /// A Discord "Incoming Webhook" URL. Receives the Discord message/embed shape.
    ///
    /// A [`SecretString`], not a URL: Discord's form embeds the token in the path
    /// (`https://discord.com/api/webhooks/{id}/{token}`), and this struct derives `Debug`.
    #[config(secret)]
    #[serde(default)]
    pub discord_webhook_url: Option<SecretString>,
    /// A generic HTTP endpoint. Receives the notifier's JSON webhook payload as a `POST` body.
    ///
    /// Wrapped for the same reason as [`Self::discord_webhook_url`]: an embedded token in the
    /// path or query is the ordinary way these endpoints authenticate.
    #[config(secret)]
    #[serde(default)]
    pub webhook_url: Option<SecretString>,
    /// Recipients of a new-chapter alert email. Empty disables the email channel.
    ///
    /// The relay, credentials and `From` address come from the shared `TANKOVAULT_EMAIL__*`
    /// config the API uses, so one deployment has one SMTP configuration.
    #[serde(default)]
    pub email_to: Vec<String>,
    /// Per-request timeout for channel deliveries.
    #[serde(default = "default_timeout_secs")]
    pub timeout_secs: u64,
}

fn default_timeout_secs() -> u64 {
    10
}

impl Default for ChannelsConfig {
    fn default() -> Self {
        Self {
            discord_webhook_url: None,
            webhook_url: None,
            email_to: Vec::new(),
            timeout_secs: default_timeout_secs(),
        }
    }
}
