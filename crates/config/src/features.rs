//! Runtime feature-flag plumbing (not the flag values themselves).

use serde::Deserialize;

/// Runtime feature flags (`tankovault_domain::Feature`).
///
/// Only the *plumbing* is configured here — which features are on is an operator decision made
/// from the control plane at runtime and stored in `feature_flag_overrides`, not a deployment
/// setting. Putting the flag values in config would defeat the point: the whole reason flags
/// exist alongside the wiring-time toggles (metrics, audit, rate limiting) is that they change
/// without a redeploy.
#[derive(Debug, Clone, Deserialize)]
pub struct FeaturesConfig {
    /// Seconds between refreshes of a service's cached flag snapshot.
    ///
    /// This is the bound on how long a flag change takes to reach *other* replicas; the
    /// replica that served the change applies it immediately. Trading a few seconds of
    /// staleness for not hitting the database on every request is the right trade for a
    /// deployment-wide switch — but the window has to be short enough that an operator
    /// switching something off during an incident does not sit and wonder.
    #[serde(default = "FeaturesConfig::default_refresh_secs")]
    pub refresh_secs: u64,
}

impl FeaturesConfig {
    fn default_refresh_secs() -> u64 {
        15
    }

    /// The refresh interval, clamped to at least a second so a misconfigured `0` cannot turn
    /// the refresh loop into a busy spin against the database.
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
