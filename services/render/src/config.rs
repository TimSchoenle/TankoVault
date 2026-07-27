//! Service configuration (loaded via `tankovault-config`: defaults → TOML → `TANKOVAULT_*`).

use serde::Deserialize;

/// Top-level render-service config.
#[derive(Debug, Deserialize)]
pub(crate) struct Config {
    #[serde(default = "default_bind")]
    pub(crate) bind_addr: String,
    pub(crate) telemetry: tankovault_config::TelemetryConfig,
    #[serde(default)]
    pub(crate) render: RenderConfig,
    /// Edge hardening: body cap, request timeout, security headers. CORS stays off —
    /// this is an internal service, not a browser-facing one.
    #[serde(default)]
    pub(crate) security: tankovault_config::SecurityConfig,
    /// Inbound rate limiting. Especially load-bearing here: every request costs a browser
    /// tab, so an unbounded caller exhausts the pool long before it exhausts the CPU.
    #[serde(default)]
    pub(crate) rate_limit: tankovault_config::RateLimitConfig,
    /// Prometheus metrics. Togglable; disabling installs no recorder.
    #[serde(default)]
    pub(crate) metrics: tankovault_config::MetricsConfig,
}

fn default_bind() -> String {
    "0.0.0.0:8084".to_owned()
}

/// Headless-browser + rendering knobs.
#[derive(Debug, Clone, Deserialize)]
pub(crate) struct RenderConfig {
    /// Explicit Chrome/Chromium executable path. When `None`, `chromiumoxide` auto-detects.
    #[serde(default)]
    pub(crate) chrome_path: Option<String>,
    /// Run the browser headless (the default; set `false` only for local debugging).
    #[serde(default = "default_true")]
    pub(crate) headless: bool,
    /// Pass `--no-sandbox` (required to run Chromium as root inside the runtime container).
    #[serde(default = "default_true")]
    pub(crate) no_sandbox: bool,
    /// Per-navigation time budget handed to the CDP client (ms).
    #[serde(default = "default_nav_timeout")]
    pub(crate) nav_timeout_ms: u64,
    /// Extra settle delay applied after navigation on every `/v1/render` (ms).
    #[serde(default)]
    pub(crate) default_wait_ms: u64,
    /// User-agent override. When set, it is applied to the page and reported back so a
    /// solved `cf_clearance` cookie stays paired with a stable UA (design §9).
    #[serde(default)]
    pub(crate) user_agent: Option<String>,
    /// TTL attached to a solved session when acting as a `ChallengeSolver` back-end (s).
    #[serde(default = "default_ttl")]
    pub(crate) session_ttl_secs: u64,
    /// Extra settle time to let a bot-management challenge clear during `/v1/solve` (ms).
    #[serde(default = "default_challenge_wait")]
    pub(crate) challenge_wait_ms: u64,
}

impl Default for RenderConfig {
    fn default() -> Self {
        Self {
            chrome_path: None,
            headless: default_true(),
            no_sandbox: default_true(),
            nav_timeout_ms: default_nav_timeout(),
            default_wait_ms: 0,
            user_agent: None,
            session_ttl_secs: default_ttl(),
            challenge_wait_ms: default_challenge_wait(),
        }
    }
}

fn default_true() -> bool {
    true
}
fn default_nav_timeout() -> u64 {
    30_000
}
fn default_ttl() -> u64 {
    900
}
fn default_challenge_wait() -> u64 {
    5_000
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn render_config_defaults_are_container_safe() {
        let cfg = RenderConfig::default();
        assert!(cfg.headless);
        assert!(cfg.no_sandbox);
        assert_eq!(cfg.nav_timeout_ms, 30_000);
        assert_eq!(cfg.session_ttl_secs, 900);
        assert_eq!(cfg.challenge_wait_ms, 5_000);
        assert_eq!(cfg.default_wait_ms, 0);
        assert!(cfg.chrome_path.is_none());
        assert!(cfg.user_agent.is_none());
    }

    #[test]
    fn render_config_partial_json_fills_defaults() {
        let cfg: RenderConfig =
            serde_json::from_str(r#"{ "headless": false, "nav_timeout_ms": 5000 }"#).unwrap();
        assert!(!cfg.headless);
        assert_eq!(cfg.nav_timeout_ms, 5_000);
        // Unspecified keys still fall back to their defaults.
        assert!(cfg.no_sandbox);
        assert_eq!(cfg.session_ttl_secs, 900);
    }
}
