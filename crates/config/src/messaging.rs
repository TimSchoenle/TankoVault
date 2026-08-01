//! Redis and NATS connection settings.
//!
//! Two one-field aggregates sharing a file rather than two three-line modules: they are the
//! same kind of thing (a broker URL this process dials) and neither has anywhere to grow
//! without the other noticing.

use secrecy::SecretString;
use serde::Deserialize;

/// Redis connection settings (cache, rate-limit counters, locks, solved sessions).
#[derive(Debug, Clone, Deserialize)]
pub struct RedisConfig {
    /// e.g. `redis://redis:6379`.
    ///
    /// A [`SecretString`] for the same reason as [`crate::DatabaseConfig::url`]: the compose
    /// deployment happens to use a credential-free URL, but `redis://:password@host` is the
    /// supported form and the type must describe what the field may hold, not what one
    /// deployment currently puts in it.
    pub url: SecretString,
}

/// NATS `JetStream` connection settings.
#[derive(Debug, Clone, Deserialize)]
pub struct NatsConfig {
    /// e.g. `nats://nats:4222`. [`SecretString`] because `nats://user:pass@host` is a
    /// supported form — see [`RedisConfig::url`].
    pub url: SecretString,
}
