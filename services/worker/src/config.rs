//! What the `worker` binary reads from its configuration.
//!
//! Public, and in a library rather than beside `main`, because it is the root
//! `config-contract` describes for this image: the contract has to be generated from the very
//! type the binary deserialises, or it is a claim about something else.

use serde::Deserialize;
use tankovault_config::{DatabaseConfig, NatsConfig, TelemetryConfig};
use terrace_config::schema::Describe;

#[derive(Debug, Deserialize, Describe)]
pub struct Config {
    #[config(nested)]
    pub database: DatabaseConfig,
    #[config(nested)]
    pub nats: NatsConfig,
    #[config(nested)]
    pub telemetry: TelemetryConfig,
    #[serde(default)]
    #[config(nested)]
    pub worker: WorkerConfig,
    /// Ops listener binding. The worker has no HTTP contract of its own, but an
    /// orchestrator still needs somewhere to send liveness/readiness probes and a
    /// scrape target — previously it exposed neither, so a wedged worker was invisible.
    #[serde(default = "default_bind")]
    pub bind_addr: String,
    /// Edge hardening for the ops listener.
    #[serde(default)]
    #[config(nested)]
    pub security: tankovault_config::SecurityConfig,
    /// Prometheus metrics. Togglable; disabling installs no recorder.
    #[serde(default)]
    #[config(nested)]
    pub metrics: tankovault_config::MetricsConfig,
    /// Shared secret presented to `challenge-solver`. The worker exposes no contract of its
    /// own, so this is outbound-only.
    #[serde(default)]
    #[config(nested)]
    pub internal: tankovault_config::InternalAuthConfig,
    /// The confidence policy for canonicalising a scanned series onto an existing one. Shared
    /// with external sync so the two paths cannot disagree about whether two series are the
    /// same (ARCH-16).
    #[serde(default)]
    #[config(nested)]
    pub matching: tankovault_config::MatchingConfig,
    /// Which source owns each metadata field. Shared with external sync, which writes the same
    /// columns: a scan that ignored it overwrote every enriched description on its next pass.
    #[serde(default)]
    #[config(nested)]
    pub metadata: tankovault_config::MetadataPriorityConfig,
    /// Which scraped chapter numbers a scan refuses to index. Sources publish stray entries
    /// numbered from dates, years and title text; left in, one of them becomes the series'
    /// latest chapter.
    #[serde(default)]
    #[config(nested)]
    pub chapter_outliers: tankovault_config::ChapterOutlierConfig,
    /// The deployment's identity. The worker reads one field of it — the identifiable crawler
    /// user-agent — so a fork does not announce this project to every site it crawls.
    #[serde(default)]
    #[config(nested)]
    pub branding: tankovault_config::BrandingConfig,
}

fn default_bind() -> String {
    "0.0.0.0:8085".to_owned()
}

#[derive(Debug, Deserialize, Describe)]
pub struct WorkerConfig {
    #[serde(default = "default_solver_endpoint")]
    pub challenge_solver_endpoint: String,
    #[serde(default = "default_max_pages")]
    pub max_catalog_pages: u32,
    /// How often the round-robin queue re-reads the provider list, in seconds.
    ///
    /// This is the delay before a provider added (or renamed) while the pool is running has
    /// its task lane opened and starts being scanned — a one-off at provider onboarding, so
    /// a minute costs nothing and keeps the query off the hot path.
    #[serde(default = "default_provider_refresh_secs")]
    pub provider_refresh_secs: u64,
    /// How many providers this worker scans at once.
    ///
    /// A worker runs at most one task per provider, so this is both the task concurrency and
    /// the count of distinct providers in flight. Crawl politeness is unaffected: `rps` and
    /// `concurrency` are enforced by a fetch stack cached per provider, which every task for
    /// that provider shares. The database pool is *not* — size `database.max_connections`
    /// for this many concurrent scans, or tasks queue on `acquire` and report as timeouts
    /// that read like a database fault.
    #[serde(default = "default_max_concurrent_providers")]
    pub max_concurrent_providers: usize,
}

impl WorkerConfig {
    /// The concurrency limit, floored at one.
    ///
    /// Zero does not disable scanning, it deadlocks: the loop would never be under the limit,
    /// so it would never claim a task and never spawn one to get back under it. The worker
    /// would sit idle against a full queue with nothing in the logs to say why. `active` on
    /// the provider is how scanning is turned off.
    #[must_use]
    pub fn max_concurrent_providers(&self) -> usize {
        self.max_concurrent_providers.max(1)
    }
}

fn default_solver_endpoint() -> String {
    "http://challenge-solver:8090".to_owned()
}

fn default_provider_refresh_secs() -> u64 {
    60
}

fn default_max_concurrent_providers() -> usize {
    // Providers, not requests: each still crawls under its own `rps`/`concurrency` budget.
    // Four is sized to keep the blocking pool (one parse per in-flight task) and the database
    // pool comfortable on the shipped container, not to any provider-side limit.
    4
}

fn default_max_pages() -> u32 {
    // Purely a runaway-paginator backstop (real termination is the adapter's `has_next`
    // marker) — some providers legitimately paginate into the thousands (e.g. kunmanga's
    // ~6866-page catalogue), so this must sit well above any real catalogue size.
    20_000
}

impl Default for WorkerConfig {
    fn default() -> Self {
        Self {
            challenge_solver_endpoint: default_solver_endpoint(),
            max_catalog_pages: default_max_pages(),
            provider_refresh_secs: default_provider_refresh_secs(),
            max_concurrent_providers: default_max_concurrent_providers(),
        }
    }
}
