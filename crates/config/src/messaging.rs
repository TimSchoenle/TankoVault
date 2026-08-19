//! Redis and NATS connection settings.

use secrecy::SecretString;
use serde::Deserialize;
use terrace_config::schema::Describe;

/// Redis connection settings (cache, rate-limit counters, locks, solved sessions).
#[derive(Debug, Clone, Deserialize, Describe)]
pub struct RedisConfig {
    /// e.g. `redis://redis:6379`.
    ///
    /// A [`SecretString`]: compose uses a credential-free URL, but `redis://:password@host`
    /// is a supported form.
    #[config(secret)]
    pub url: SecretString,
}

/// NATS `JetStream` connection settings.
#[derive(Debug, Clone, Deserialize, Describe)]
pub struct NatsConfig {
    /// e.g. `nats://nats:4222`. [`SecretString`]: `nats://user:pass@host` is supported.
    #[config(secret)]
    pub url: SecretString,
}
