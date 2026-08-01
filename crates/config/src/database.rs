//! `PostgreSQL` connection settings.

use secrecy::SecretString;
use serde::Deserialize;

/// `PostgreSQL` connection settings.
#[derive(Debug, Clone, Deserialize)]
pub struct DatabaseConfig {
    /// e.g. `postgres://user:pass@host:5432/tankovault`.
    ///
    /// A [`SecretString`]: the DSN carries the password, and this struct's `Debug` is nested
    /// inside state routinely logged with `?`.
    pub url: SecretString,
    /// Upper bound on the connection pool.
    #[serde(default = "DatabaseConfig::default_max_connections")]
    pub max_connections: u32,
    /// Statement/acquire timeout, seconds.
    #[serde(default = "DatabaseConfig::default_acquire_timeout_secs")]
    pub acquire_timeout_secs: u64,
}

impl DatabaseConfig {
    fn default_max_connections() -> u32 {
        16
    }
    fn default_acquire_timeout_secs() -> u64 {
        10
    }
}
