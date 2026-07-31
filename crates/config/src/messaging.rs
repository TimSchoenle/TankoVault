//! Redis and NATS connection settings.
//!
//! Two one-field aggregates sharing a file rather than two three-line modules: they are the
//! same kind of thing (a broker URL this process dials) and neither has anywhere to grow
//! without the other noticing.

use serde::Deserialize;

/// Redis connection settings (cache, rate-limit counters, locks, solved sessions).
#[derive(Debug, Clone, Deserialize)]
pub struct RedisConfig {
    /// e.g. `redis://redis:6379`.
    pub url: String,
}

/// NATS `JetStream` connection settings.
#[derive(Debug, Clone, Deserialize)]
pub struct NatsConfig {
    /// e.g. `nats://nats:4222`.
    pub url: String,
}
