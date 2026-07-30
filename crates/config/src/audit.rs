//! Append-only audit trail settings.

use serde::Deserialize;

/// Append-only audit trail for privileged and privacy-relevant actions (design §16).
#[derive(Debug, Clone, Deserialize)]
pub struct AuditConfig {
    /// Write audit records. When `false` a no-op sink is installed and call sites stay
    /// unchanged — auditing is a wiring decision, never an `if` in a handler.
    #[serde(default = "crate::default_true")]
    pub enabled: bool,
    /// Record the client IP alongside each event. Off by default: an IP is personal data
    /// under GDPR Art. 4(1), so retaining it is an explicit operator decision.
    #[serde(default)]
    pub record_ip: bool,
    /// Record the client `User-Agent` alongside each event.
    #[serde(default)]
    pub record_user_agent: bool,
    /// Days to retain audit records before the retention sweep deletes them. `0` disables
    /// the sweep and keeps records forever, which is rarely what a GDPR-scoped deployment
    /// wants (storage limitation, Art. 5(1)(e)).
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
