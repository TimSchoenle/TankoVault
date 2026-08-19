//! What the `sync` binary reads from its configuration.
//!
//! Public, and in a library rather than beside `main`, because it is the root
//! `config-contract` describes for this image: the contract has to be generated from the very
//! type the binary deserialises, or it is a claim about something else.

use secrecy::SecretString;
use serde::Deserialize;
use tankovault_config::{DatabaseConfig, TelemetryConfig};
use tankovault_contracts::sync::ConflictPolicy;
use tankovault_domain::MetadataPriority;
use terrace_config::schema::Describe;

/// Default `AniList` GraphQL endpoint.
pub const DEFAULT_GRAPHQL_URL: &str = "https://graphql.anilist.co";
/// Default `AniList` OAuth base (authorize + token live under here).
pub const DEFAULT_OAUTH_BASE: &str = "https://anilist.co/api/v2/oauth";

#[derive(Debug, Deserialize, Describe)]
pub struct Config {
    #[config(nested)]
    pub database: DatabaseConfig,
    #[config(nested)]
    pub telemetry: TelemetryConfig,
    #[config(nested)]
    pub anilist: AniListConfig,
    #[serde(default)]
    #[config(nested)]
    pub metadata: MetadataConfig,
    #[serde(default = "default_bind")]
    pub bind_addr: String,
    /// Interval (seconds) between scheduled reconciliation ticks. `0` disables the loop.
    #[serde(default = "default_reconcile_interval")]
    pub reconcile_interval_secs: u64,
    /// Edge hardening for this internal service.
    #[serde(default)]
    #[config(nested)]
    pub security: tankovault_config::SecurityConfig,
    /// Inbound rate limiting; pull/push routes draw from the tighter "expensive" budget.
    #[serde(default)]
    #[config(nested)]
    pub rate_limit: tankovault_config::RateLimitConfig,
    /// Prometheus metrics. Togglable; disabling installs no recorder.
    #[serde(default)]
    #[config(nested)]
    pub metrics: tankovault_config::MetricsConfig,
    /// Runtime feature flags — how often this replica re-reads the operator's decisions.
    #[serde(default)]
    #[config(nested)]
    pub features: tankovault_config::FeaturesConfig,
    /// Shared secret every caller must present: this whole contract is privileged, naming the
    /// subject user in the path or body.
    #[serde(default)]
    #[config(nested)]
    pub internal: tankovault_config::InternalAuthConfig,
    /// The confidence policy for resolving a remote entry onto a local series. Shared with the
    /// worker's ingest canonicalisation so the two paths can't disagree on a match.
    #[serde(default)]
    #[config(nested)]
    pub matching: tankovault_config::MatchingConfig,
}

/// Metadata-priority + tokenless enrichment-worker settings.
// `Clone` because the config now lives behind an `Arc` shared with the reload supervisor and
// so cannot be moved out of.
#[derive(Debug, Clone, Deserialize, Describe)]
pub struct MetadataConfig {
    /// Per-field source authority order (default: `AniList` before the adapters).
    // A leaf rather than `#[config(nested)]`, for the reason
    // `tankovault_config::MetadataPriorityConfig::priority` gives: the type is the leaf domain
    // crate's, and describing it would put figment there.
    #[serde(default)]
    pub priority: MetadataPriority,
    /// Which scraped "genres" intake refuses. Shared with the worker via
    /// [`tankovault_config::TagIntakeConfig`], because both write the same `tags` vocabulary.
    #[serde(default)]
    #[config(nested)]
    pub tags: tankovault_config::TagIntakeConfig,
    /// Whether the background enrichment worker runs. On by default.
    #[serde(default = "default_enrich_enabled")]
    pub enrich_enabled: bool,
    /// Seconds between enrichment sweeps.
    #[serde(default = "default_enrich_interval_secs")]
    pub enrich_interval_secs: u64,
    /// Series fetched per DB page during a sweep.
    #[serde(default = "default_enrich_batch")]
    pub enrich_batch: i64,
    /// Upper bound on series processed per sweep (paces `AniList`'s rate limit).
    #[serde(default = "default_enrich_max")]
    pub enrich_max_series: usize,
}

#[derive(Debug, Clone, Deserialize, Describe)]
pub struct AniListConfig {
    #[serde(deserialize_with = "string_or_number")]
    pub client_id: String,
    /// The `OAuth2` client secret; lets anyone mint tokens as this app.
    #[config(secret)]
    pub client_secret: SecretString,
    pub redirect_uri: String,
    /// Base64 32-byte data-encryption key for tokens at rest — opens every user's stored
    /// `AniList` access and refresh token.
    #[config(secret)]
    pub token_encryption_key: SecretString,
    #[serde(default = "default_graphql_url")]
    pub graphql_url: String,
    #[serde(default = "default_oauth_base")]
    pub oauth_base: String,
    // A leaf rather than `#[config(values)]`: the type is `tankovault-contracts`', which is a
    // wire-shape crate and has no business linking figment. The contract publishes the key with
    // no constraint, which says what is true — the key exists, and nothing here can check it.
    #[serde(default)]
    pub default_conflict_policy: ConflictPolicy,
    #[serde(default = "default_min_interval_ms")]
    pub min_request_interval_ms: u64,
}

fn default_bind() -> String {
    "0.0.0.0:8083".to_owned()
}

fn default_graphql_url() -> String {
    DEFAULT_GRAPHQL_URL.to_owned()
}

fn default_oauth_base() -> String {
    DEFAULT_OAUTH_BASE.to_owned()
}

fn default_min_interval_ms() -> u64 {
    700
}

fn default_reconcile_interval() -> u64 {
    900
}

fn default_enrich_enabled() -> bool {
    true
}

fn default_enrich_interval_secs() -> u64 {
    3600
}

fn default_enrich_batch() -> i64 {
    200
}

fn default_enrich_max() -> usize {
    // Must stay comfortably inside one sweep interval at `min_request_interval_ms` pacing, or
    // sweeps overlap; too low and metadata visibly lags for days.
    2_000
}

/// `figment`'s `Env` provider infers numeric-looking values (e.g. `TANKOVAULT_ANILIST__CLIENT_ID`)
/// as numbers rather than strings, so accept either and coerce to `String`.
fn string_or_number<'de, D>(deserializer: D) -> Result<String, D::Error>
where
    D: serde::Deserializer<'de>,
{
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum StringOrNumber {
        String(String),
        Int(i64),
        UInt(u64),
        Float(f64),
    }

    Ok(match StringOrNumber::deserialize(deserializer)? {
        StringOrNumber::String(s) => s,
        StringOrNumber::Int(i) => i.to_string(),
        StringOrNumber::UInt(u) => u.to_string(),
        StringOrNumber::Float(f) => f.to_string(),
    })
}

impl Default for MetadataConfig {
    fn default() -> Self {
        Self {
            priority: MetadataPriority::default(),
            tags: tankovault_config::TagIntakeConfig::default(),
            enrich_enabled: default_enrich_enabled(),
            enrich_interval_secs: default_enrich_interval_secs(),
            enrich_batch: default_enrich_batch(),
            enrich_max_series: default_enrich_max(),
        }
    }
}
