//! How a service refreshes its runtime feature flags.

use serde::Deserialize;
use terrace_config::schema::Describe;

/// How a service refreshes its runtime feature flags (`tankovault_domain::Feature`).
///
/// The flag values are set at runtime from the control plane, not in configuration.
#[derive(Debug, Clone, Deserialize, Describe)]
pub struct FeaturesConfig {
    /// Seconds between refreshes of a service's cached flag snapshot. It is how long a change
    /// takes to reach the other replicas; the replica that served the change applies it at once.
    #[serde(default = "FeaturesConfig::default_refresh_secs")]
    pub refresh_secs: u64,
}

impl FeaturesConfig {
    fn default_refresh_secs() -> u64 {
        15
    }

    /// Returns the refresh interval, at least one second, so `0` cannot busy-spin.
    #[must_use]
    pub fn refresh_interval(&self) -> std::time::Duration {
        std::time::Duration::from_secs(self.refresh_secs.max(1))
    }
}

impl Default for FeaturesConfig {
    fn default() -> Self {
        Self {
            refresh_secs: Self::default_refresh_secs(),
        }
    }
}
