//! Redis and NATS connection settings.

use crate::ConfigError;
use secrecy::SecretString;
use serde::Deserialize;
use std::time::Duration;
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
    /// Longest the events stream keeps an event no consumer has acked, in seconds.
    #[serde(default = "NatsConfig::default_events_max_age_secs")]
    #[config(range(min = 1))]
    pub events_max_age_secs: u64,
}

impl NatsConfig {
    fn default_events_max_age_secs() -> u64 {
        7 * 24 * 60 * 60
    }

    /// [`Self::events_max_age_secs`] as a [`Duration`].
    #[must_use]
    pub fn events_max_age(&self) -> Duration {
        Duration::from_secs(self.events_max_age_secs)
    }

    /// Refuse a zero `events_max_age_secs`.
    ///
    /// `JetStream` reads a zero `max_age` as "no limit", so zero would silently restore the
    /// unbounded events stream this setting exists to prevent.
    ///
    /// # Errors
    /// [`ConfigError::Invalid`] when `events_max_age_secs` is zero.
    //
    // The bound is also published by `#[config(range(min = 1))]`; change the two together.
    pub fn validate(&self) -> Result<(), ConfigError> {
        if self.events_max_age_secs == 0 {
            return Err(ConfigError::Invalid(
                "nats.events_max_age_secs must be greater than zero; JetStream reads zero as \
                 unlimited"
                    .to_owned(),
            ));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config(events_max_age_secs: u64) -> NatsConfig {
        NatsConfig {
            url: SecretString::from("nats://nats:4222"),
            events_max_age_secs,
        }
    }

    /// Zero is `JetStream`'s "unlimited", which is the unbounded events stream this bound was
    /// added to stop.
    #[test]
    fn a_zero_events_max_age_is_refused() {
        assert!(config(0).validate().is_err());
        assert!(config(1).validate().is_ok());
        assert!(
            config(NatsConfig::default_events_max_age_secs())
                .validate()
                .is_ok()
        );
    }
}
