//! Append-only audit trail settings.

use serde::Deserialize;

/// Append-only audit trail for privileged and privacy-relevant actions (design §16).
#[derive(Debug, Clone, Deserialize)]
pub struct AuditConfig {
    /// Write audit records; `false` installs a no-op sink instead of branching at call sites.
    #[serde(default = "crate::default_true")]
    pub enabled: bool,
    /// Record the client IP. Off by default: an IP is personal data under GDPR Art. 4(1).
    #[serde(default)]
    pub record_ip: bool,
    /// Record the client `User-Agent` alongside each event.
    #[serde(default)]
    pub record_user_agent: bool,
    /// Days to retain audit records before the sweep deletes them. `0` disables the sweep
    /// (rarely right under GDPR Art. 5(1)(e), storage limitation).
    #[serde(default = "AuditConfig::default_retention_days")]
    pub retention_days: u32,
    /// Hours between retention sweeps. Ignored when [`Self::retention_days`] is `0`.
    #[serde(default = "AuditConfig::default_sweep_interval_hours")]
    pub sweep_interval_hours: u64,
}

impl AuditConfig {
    fn default_retention_days() -> u32 {
        365
    }
    fn default_sweep_interval_hours() -> u64 {
        24
    }

    /// Whether the background retention sweep should run.
    #[must_use]
    pub fn retention_enabled(&self) -> bool {
        self.enabled && self.retention_days > 0
    }
}

impl Default for AuditConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            record_ip: false,
            record_user_agent: false,
            retention_days: Self::default_retention_days(),
            sweep_interval_hours: Self::default_sweep_interval_hours(),
        }
    }
}
