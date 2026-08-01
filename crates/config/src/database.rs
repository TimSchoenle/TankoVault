//! `PostgreSQL` connection settings.

use secrecy::SecretString;
use serde::Deserialize;

/// `PostgreSQL` connection settings.
#[derive(Debug, Clone, Deserialize)]
pub struct DatabaseConfig {
    /// e.g. `postgres://user:pass@host:5432/tankovault`.
    ///
    /// A [`SecretString`], because a DSN is a credential — the password is *in* it. This
    /// struct derives `Debug` and is nested inside every service's config aggregate, which is
    /// in turn nested inside state that gets recorded with `?`; the wrapper is what makes
    /// that safe rather than merely untested.
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
