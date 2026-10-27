//! `PostgreSQL` connection settings.

use secrecy::SecretString;
use serde::Deserialize;
use terrace_config::schema::Describe;

/// `PostgreSQL` connection settings.
#[derive(Debug, Clone, Deserialize, Describe)]
pub struct DatabaseConfig {
    /// The connection URL, such as `postgres://user:pass@host:5432/tankovault`.
    // A `SecretString` because the URL carries the password and this struct's `Debug` is nested in
    // state that is logged with `?`.
    #[config(secret)]
    pub url: SecretString,
    /// Upper bound on the connection pool.
    #[serde(default = "DatabaseConfig::default_max_connections")]
    pub max_connections: u32,
    /// How long a caller waits for a pooled connection before failing, in seconds.
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
