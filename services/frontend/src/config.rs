//! What the `frontend` binary reads from its configuration.
//!
//! Public, and in a library rather than beside `main`, because it is the root
//! `config-contract` describes for this image: the contract has to be generated from the very
//! type the binary deserialises, or it is a claim about something else.

use serde::Deserialize;
use tankovault_config::{BrandingConfig, MetricsConfig, TelemetryConfig};
use terrace_config::schema::Describe;

/// Top-level frontend config.
#[derive(Debug, Deserialize, Describe)]
pub struct Config {
    /// The public listener, serving both the SPA bundle and the `/v1/*` proxy.
    #[serde(default = "default_bind")]
    pub bind_addr: String,
    /// Log filter, log format and Sentry reporting.
    #[config(nested)]
    pub telemetry: TelemetryConfig,
    /// Where the bundle is read from, where `/v1/*` is forwarded, and what this hop accepts.
    #[serde(default)]
    #[config(nested)]
    pub frontend: FrontendConfig,
    /// Prometheus metrics, with the same `TANKOVAULT_METRICS__*` surface as every service —
    /// including the isolated scrape port, so the scrape never shares the public listener.
    #[serde(default)]
    #[config(nested)]
    pub metrics: MetricsConfig,
    /// What this deployment calls itself. Written into the served shell's `<title>` and
    /// description so the tab is named before the WASM bundle boots — the SPA takes over from
    /// `/v1/branding` once it has.
    #[serde(default)]
    #[config(nested)]
    pub branding: BrandingConfig,
}

/// Non-privileged: the `scratch` image runs as a numeric nonroot user, which can't bind
/// reserved port 80; the compose stack maps host `3000` here instead.
fn default_bind() -> String {
    "0.0.0.0:3000".to_owned()
}

/// Where the bundle is read from, where `/v1/*` is forwarded, and what this hop accepts.
#[derive(Debug, Clone, Deserialize, Describe)]
pub struct FrontendConfig {
    /// Directory the built SPA bundle is served from. Baked into the image at `/srv/www`.
    #[serde(default = "FrontendConfig::default_static_dir")]
    pub static_dir: String,
    /// The generated third-party licence notices, served at `/third-party-notices`.
    ///
    /// Points at the image's own copy (`/THIRD-PARTY-NOTICES`, beside `/LICENSE`) rather than a
    /// second one inside the bundle: it is 300-odd KB, and two copies is the arrangement where
    /// one of them goes stale. Configurable because the path only exists inside the image — a
    /// developer running `dx serve` and the tests point it at their own checkout.
    #[serde(default = "FrontendConfig::default_notices_path")]
    pub notices_path: String,
    /// The same notices as the structured inventory the `/licenses` screen renders, served at
    /// `/third-party-notices.json`.
    ///
    /// Written into the image by `xtask notices --json` during the build rather than committed:
    /// it is a second representation of the document above, and the two come out of one merge, so
    /// committing it would double the repository's generated weight for a drift gate the
    /// plain-text `--check` already provides. A deployment whose image predates it, or a
    /// developer who has not run the command, serves nothing here and the screen says so.
    #[serde(default = "FrontendConfig::default_notices_json_path")]
    pub notices_json_path: String,
    /// Base origin the `/v1/*` proxy targets, e.g. `http://api:8080`. No trailing slash.
    #[serde(default = "FrontendConfig::default_api_upstream")]
    pub api_upstream: String,
    /// Largest request body accepted on this hop.
    ///
    /// Enforced twice: the shared stack's `DefaultBodyLimit` rejects it before buffering (see
    /// `stack_security` in `main.rs`), and the proxy handler passes the same number to
    /// `to_bytes` so the two cannot drift.
    #[serde(default = "FrontendConfig::default_max_body_bytes")]
    pub max_body_bytes: usize,
    /// Connection-establishment timeout for the upstream — not a whole-request timeout, since
    /// an SSE stream stays open indefinitely.
    #[serde(default = "FrontendConfig::default_connect_timeout_secs")]
    pub connect_timeout_secs: u64,
    /// Opt-ins for running behind Cloudflare. See [`CloudflareConfig`].
    #[serde(default)]
    #[config(nested)]
    pub cloudflare: CloudflareConfig,
}

/// What this tier's Content-Security-Policy concedes to Cloudflare, one flag per product.
///
/// Both default off, and both stay off for a deployment that is not behind Cloudflare: each one
/// admits something the policy otherwise refuses, and an unused concession is only a weakness.
#[derive(Debug, Clone, Default, Deserialize, Describe)]
pub struct CloudflareConfig {
    /// Send a freshly minted `'nonce-…'` in `script-src` on every response.
    ///
    /// Cloudflare's bot products (Bot Fight Mode, JavaScript Detections, the challenge platform)
    /// inject an inline `<script>` into the served HTML at the edge, after this service has
    /// hashed the shell — so `script-src` refuses it and the detection silently never runs.
    /// Cloudflare's documented answer is a nonce: it parses the `Content-Security-Policy`
    /// *response header* and copies the nonce onto what it injects. That is why nothing is
    /// stamped into the shell here — the header is the whole contract, and the shell's own
    /// inline scripts keep running by hash either way.
    ///
    /// The concession is real but narrow: an injected script that can already run could read
    /// this header back off a same-origin fetch and admit further inline script. It cannot
    /// forge one ahead of time (128 CSPRNG bits, minted per response), and it still cannot
    /// reach `'unsafe-eval'` or an off-origin host. Load-bearing for that: the shell is never
    /// served from a stored copy, which is why it is `Cache-Control: no-store` with no validator
    /// and not `no-cache` — see `FixedResponse::shell` in `main.rs`. A stored shell would pin
    /// one nonce across every reader for the lifetime of the entry, which is `'unsafe-inline'`
    /// with extra steps.
    #[serde(default)]
    pub script_nonce: bool,
    /// Admit `https://challenges.cloudflare.com` in `script-src` and `frame-src`, for an
    /// embedded Turnstile widget.
    ///
    /// Only for a widget rendered *in* a page this service serves; a managed-challenge
    /// interstitial is a Cloudflare-served document carrying its own policy, and needs nothing
    /// here. The origin is Turnstile's only host and `'self'` can never cover it.
    #[serde(default)]
    pub turnstile: bool,
}

impl FrontendConfig {
    fn default_static_dir() -> String {
        "/srv/www".to_owned()
    }
    fn default_notices_path() -> String {
        "/THIRD-PARTY-NOTICES".to_owned()
    }
    fn default_notices_json_path() -> String {
        "/THIRD-PARTY-NOTICES.json".to_owned()
    }
    fn default_api_upstream() -> String {
        "http://api:8080".to_owned()
    }
    fn default_max_body_bytes() -> usize {
        10 * 1024 * 1024
    }
    fn default_connect_timeout_secs() -> u64 {
        10
    }
}

impl Default for FrontendConfig {
    fn default() -> Self {
        Self {
            static_dir: Self::default_static_dir(),
            notices_path: Self::default_notices_path(),
            notices_json_path: Self::default_notices_json_path(),
            api_upstream: Self::default_api_upstream(),
            max_body_bytes: Self::default_max_body_bytes(),
            connect_timeout_secs: Self::default_connect_timeout_secs(),
            cloudflare: CloudflareConfig::default(),
        }
    }
}
