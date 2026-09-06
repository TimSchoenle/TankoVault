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

/// Top-level sync config.
#[derive(Debug, Deserialize, Describe)]
pub struct Config {
    /// Where the catalogue lives, and how many connections this service may hold open.
    #[config(nested)]
    pub database: DatabaseConfig,
    /// Log filter, log format and Sentry reporting.
    #[config(nested)]
    pub telemetry: TelemetryConfig,
    /// The `AniList` application this deployment syncs through. Required: there is no second
    /// tracker to fall back to.
    #[config(nested)]
    pub anilist: AniListConfig,
    /// Which source owns each metadata field, and whether the background enrichment worker
    /// runs at all.
    #[serde(default)]
    #[config(nested)]
    pub metadata: MetadataConfig,
    /// Listen address for the internally-authenticated sync contract and the probes. Not a
    /// public listener; the API proxies `/v1/me/sync/*` to it.
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
    // `nested`, through `tankovault-domain`'s `schema` feature — the same subtree
    // `tankovault_config::MetadataPriorityConfig::priority` publishes, so the two services'
    // contracts describe one key one way.
    #[config(nested)]
    #[serde(default)]
    pub priority: MetadataPriority,
    /// Which scraped "genres" intake refuses. Shared with the worker via
    /// [`tankovault_config::TermBlocklistConfig`], because both write the same `tags` vocabulary.
    /// The classifier half of `metadata.tags` is the worker's and the API's; the enrichment
    /// writer takes `is_adult` from `AniList`, so declaring it here would put a key in this
    /// image's contract that its binary never reads.
    #[serde(default)]
    #[config(nested)]
    pub tags: tankovault_config::TermBlocklistConfig,
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

/// The `AniList` application this deployment syncs through, and how hard it may push.
#[derive(Debug, Clone, Deserialize, Describe)]
pub struct AniListConfig {
    /// The `OAuth2` client id, from the application `AniList` issued. Accepted as a number
    /// too, because the environment provider infers one.
    #[serde(deserialize_with = "string_or_number")]
    pub client_id: String,
    /// The `OAuth2` client secret; lets anyone mint tokens as this app.
    #[config(secret)]
    pub client_secret: SecretString,
    /// Where `AniList` returns the reader after they authorise. Registered on the `AniList`
    /// application, and refused if the two disagree by a single character.
    pub redirect_uri: String,
    /// Base64 32-byte data-encryption key for tokens at rest — opens every user's stored
    /// `AniList` access and refresh token.
    #[config(secret)]
    pub token_encryption_key: SecretString,
    /// The `GraphQL` endpoint every read and write goes to. Overridable so a test can point
    /// it at a stub.
    #[serde(default = "default_graphql_url")]
    pub graphql_url: String,
    /// Base URL the `authorize` and `token` paths hang off.
    #[serde(default = "default_oauth_base")]
    pub oauth_base: String,
    /// Which side wins a two-sided change for a reader who has expressed no preference. Each
    /// reader may override it on their own link.
    // `values_from` rather than `values`: the type is `tankovault-contracts`', which is a
    // wire-shape crate and has no business linking figment, so there is no `Values` impl on it
    // to read. [`ConflictPolicyDef`] is the mirror the compiler holds to it.
    #[config(values_from = "ConflictPolicyDef")]
    #[serde(default)]
    pub default_conflict_policy: ConflictPolicy,
    /// Shortest gap between two requests to `AniList`, in milliseconds. It paces this
    /// deployment's whole traffic, not one reader's: `AniList` rate-limits per application.
    #[serde(default = "default_min_interval_ms")]
    pub min_request_interval_ms: u64,
}

/// The four spellings [`ConflictPolicy`] accepts, published by
/// [`AniListConfig::default_conflict_policy`].
///
/// A `#[serde(remote)]` mirror rather than a literal `#[config(values(…))]` list, because the
/// two cannot drift: serde's remote derive constructs `ConflictPolicy`'s own variants, so a
/// spelling that stopped matching the real enum would fail to compile rather than reach the
/// contract. `ConflictPolicy` deserialises through serde's own derive — no `try_from`, no
/// `from` — so this renamed variant list *is* the accepted set. Nothing deserialises through
/// the mirror; the field keeps `ConflictPolicy`'s own `Deserialize`.
#[derive(Deserialize, Describe)]
#[serde(remote = "ConflictPolicy", rename_all = "snake_case")]
#[expect(
    dead_code,
    reason = "the remote derive constructs each `ConflictPolicy` variant, which is what holds \
              this mirror to the real enum; `values_from` reads only the names, so the \
              constructing code is never called"
)]
enum ConflictPolicyDef {
    /// Local progress/status is authoritative.
    LocalWins,
    /// The remote (`AniList`) value is authoritative.
    RemoteWins,
    /// Whichever side was updated most recently wins.
    NewestWins,
    /// Genuine conflicts are queued for the user to resolve rather than auto-picked.
    AskMe,
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
            tags: tankovault_config::TermBlocklistConfig::default(),
            enrich_enabled: default_enrich_enabled(),
            enrich_interval_secs: default_enrich_interval_secs(),
            enrich_batch: default_enrich_batch(),
            enrich_max_series: default_enrich_max(),
        }
    }
}
