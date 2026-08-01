//! Runtime feature-flag plumbing (not the flag values themselves).

use serde::Deserialize;

/// Runtime feature flags (`tankovault_domain::Feature`); only the *plumbing* — flag values
/// are set at runtime from the control plane, not deployed here.
#[derive(Debug, Clone, Deserialize)]
pub struct FeaturesConfig {
    /// Seconds between refreshes of a service's cached flag snapshot — the propagation delay
    /// to *other* replicas (the one that served the change applies it immediately).
    #[serde(default = "FeaturesConfig::default_refresh_secs")]
    pub refresh_secs: u64,
}

impl FeaturesConfig {
    fn default_refresh_secs() -> u64 {
        15
    }

    /// The refresh interval, clamped to at least a second to avoid a busy-spin on `0`.
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
