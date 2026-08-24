//! What the `control-plane` binary reads from its configuration.
//!
//! Public, and in the library rather than beside `main`, because it is the root
//! `config-contract` describes for this image: the contract has to be generated from the very
//! type the binary deserialises, or it is a claim about something else.

use serde::Deserialize;
use terrace_config::schema::Describe;

/// Top-level control-plane config.
#[derive(Debug, Deserialize, Describe)]
pub struct Config {
    /// Where the catalogue lives, and how many connections this service may hold open.
    #[config(nested)]
    pub database: tankovault_config::DatabaseConfig,
    /// The task broker every scheduled sweep publishes into. Required.
    #[config(nested)]
    pub nats: tankovault_config::NatsConfig,
    /// Log filter, log format and Sentry reporting.
    #[config(nested)]
    pub telemetry: tankovault_config::TelemetryConfig,
    /// Optional Redis endpoint used for singleton-scheduler leader election. When absent,
    /// this replica is treated as the sole leader (single-instance / local dev).
    #[serde(default)]
    #[config(nested)]
    pub redis: Option<tankovault_config::RedisConfig>,
    /// How often each sweep runs, and how much work each one takes on.
    #[serde(default)]
    #[config(nested)]
    pub scheduler: SchedulerConfig,
    /// Listen address for `/internal/*` and the probes. Not a public listener.
    #[serde(default = "default_bind")]
    pub bind_addr: String,
    /// Edge hardening for the internal trigger endpoint.
    #[serde(default)]
    #[config(nested)]
    pub security: tankovault_config::SecurityConfig,
    /// Inbound rate limiting on `/internal/scans`, so a stuck caller cannot fan out
    /// unbounded scan runs.
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
    /// Shared secret every caller must present on `/internal/*`. Triggering scan runs is an
    /// operator action; the endpoint's name is not an access control.
    #[serde(default)]
    #[config(nested)]
    pub internal: tankovault_config::InternalAuthConfig,
    /// The confidence policy for matching series, shared with the worker's ingest and
    /// external sync so no two paths disagree; the duplicate sweep applies it to
    /// existing series.
    #[serde(default)]
    #[config(nested)]
    pub matching: tankovault_config::MatchingConfig,
}

fn default_bind() -> String {
    "0.0.0.0:8081".to_owned()
}

/// How often each standing sweep runs, and how much work one takes on.
///
/// Every interval is in seconds and every one of them accepts `0`, which disables that sweep
/// and only that sweep.
#[derive(Debug, Clone, Deserialize, Describe)]
pub struct SchedulerConfig {
    /// Seconds between fast-scan sweeps of all active providers. 0 disables.
    #[serde(default = "default_fast_interval")]
    pub fast_interval_secs: u64,
    /// Seconds between full-scan sweeps. 0 disables (full scans are usually on demand).
    #[serde(default)]
    pub full_interval_secs: u64,
    /// Seconds between full duplicate-reconciliation sweeps. 0 disables.
    ///
    /// Hourly by default: this is the cadence of *discovery*, which blocks the whole catalogue on
    /// the compact title key and so costs the same whether anything changed or not.
    #[serde(default = "default_merge_sweep_interval")]
    pub merge_sweep_interval_secs: u64,
    /// Seconds between rotation-only sweeps — the open queue and the recheck set, no discovery.
    /// 0 disables.
    ///
    /// Four times an hour by default. These two shortlists are index scans bounded by their own
    /// budgets, and re-scoring is how a pair that was genuinely ambiguous in January becomes an
    /// automatic merge once both sides have been enriched; tying that to the discovery cadence
    /// meant a queue of thousands took most of a day to turn over once.
    #[serde(default = "default_merge_sweep_rotation_interval")]
    pub merge_sweep_rotation_interval_secs: u64,
    /// Newly-blocked duplicate pairs shortlisted per sweep.
    #[serde(default = "default_merge_sweep_pairs")]
    pub merge_sweep_pairs: i64,
    /// Open queue rows re-scored per sweep, least-recently-scored first.
    #[serde(default = "default_merge_sweep_requeue")]
    pub merge_sweep_requeue: i64,
    /// Previously-distinct pairs reconsidered per sweep, least-recently-scored first.
    #[serde(default = "default_merge_sweep_recheck")]
    pub merge_sweep_recheck: i64,
    /// Seconds between incremental recommendation-model builds. 0 disables.
    ///
    /// Frequent by default: an incremental pass re-embeds only what changed and inserts into a
    /// live HNSW index, so the cost is proportional to catalogue churn rather than to the
    /// catalogue.
    #[serde(default = "default_recsys_incremental_interval")]
    pub recsys_incremental_interval_secs: u64,
    /// Seconds between full recommendation-model rebuilds. 0 disables.
    ///
    /// A full build re-solves the projection from the whole catalogue, which is what keeps the
    /// idf and the embedding space current. Weekly: the incremental pass covers changed series,
    /// so this exists for vocabulary drift rather than for freshness.
    #[serde(default = "default_recsys_full_interval")]
    pub recsys_full_interval_secs: u64,
    /// Series per streamed batch in a model build.
    #[serde(default = "default_recsys_batch")]
    pub recsys_batch: i64,
    /// Ceiling on how many series one incremental build may touch.
    #[serde(default = "default_recsys_incremental_max")]
    pub recsys_incremental_max: i64,
    /// Automatic merges permitted in a single sweep — the only bound on a destructive
    /// background action. Without it, a bad threshold or normalization rule could collapse
    /// the whole catalogue between two scheduler ticks.
    #[serde(default = "default_merge_sweep_max_auto_merges")]
    pub merge_sweep_max_auto_merges: i64,
    /// How long an unfinished run keeps suppressing new runs of the same provider and mode.
    ///
    /// The suppression itself has no expiry condition other than this one, so the value is the
    /// only thing standing between a run that can never settle — a task persisted but never
    /// published — and a provider that is never scanned again.
    #[serde(default = "default_run_stale_after")]
    pub run_stale_after_secs: u64,
    /// Ceiling on how long a provider whose runs keep failing is skipped by the sweeps. 0
    /// disables the backoff entirely, restoring the unconditional sweep.
    ///
    /// Six hours by default. The sweep is what turns one broken provider into a steady stream of
    /// requests at a site that is answering none of them, and the cooldown is the only thing that
    /// stops it; the cost of the ceiling being *too high* is that a provider which recovered
    /// waits up to this long to be noticed, which is why it is hours and not days.
    #[serde(default = "default_failure_backoff_max")]
    pub failure_backoff_max_secs: u64,
    /// Seconds between passes that reconcile `JetStream` against `scan_tasks`. 0 disables.
    ///
    /// This is a repair, not a schedule: it exists because dispatch and the task table can come
    /// apart with no failure on either side, and nothing else notices when they do. Disabling it
    /// leaves a lost message costing a run — and, until `run_stale_after_secs`, the provider's
    /// next run of that mode as well.
    #[serde(default = "default_reconcile_interval")]
    pub reconcile_interval_secs: u64,
}

fn default_fast_interval() -> u64 {
    300
}

fn default_merge_sweep_interval() -> u64 {
    3600
}

fn default_merge_sweep_rotation_interval() -> u64 {
    900
}

fn default_merge_sweep_pairs() -> i64 {
    500
}

fn default_merge_sweep_requeue() -> i64 {
    500
}

fn default_merge_sweep_recheck() -> i64 {
    500
}

/// The per-run ceiling on automatic merges, read by the test that pins the rotation's share.
pub fn default_merge_sweep_max_auto_merges() -> i64 {
    200
}

/// An hour — comfortably longer than any healthy scan of one provider, and short enough that a
/// run which can never settle costs at most one hour of that provider's schedule.
const fn default_run_stale_after() -> u64 {
    3600
}

/// Six hours: four attempts a day at a provider that is down, against 288 at the default fast
/// cadence, while still finding it within a working day of it coming back.
const fn default_failure_backoff_max() -> u64 {
    21_600
}

/// Five minutes: frequent enough that a lost message costs one scan cycle rather than the hour
/// `run_stale_after_secs` would otherwise take to release the provider, and rare enough that the
/// pass — one broker call per lane with open work, and nothing at all when there is none — is
/// invisible next to a sweep.
const fn default_reconcile_interval() -> u64 {
    300
}

const fn default_recsys_incremental_interval() -> u64 {
    900
}

const fn default_recsys_full_interval() -> u64 {
    604_800
}

const fn default_recsys_batch() -> i64 {
    512
}

const fn default_recsys_incremental_max() -> i64 {
    20_000
}

impl Default for SchedulerConfig {
    fn default() -> Self {
        Self {
            fast_interval_secs: default_fast_interval(),
            full_interval_secs: 0,
            merge_sweep_interval_secs: default_merge_sweep_interval(),
            merge_sweep_rotation_interval_secs: default_merge_sweep_rotation_interval(),
            merge_sweep_pairs: default_merge_sweep_pairs(),
            merge_sweep_requeue: default_merge_sweep_requeue(),
            merge_sweep_recheck: default_merge_sweep_recheck(),
            merge_sweep_max_auto_merges: default_merge_sweep_max_auto_merges(),
            run_stale_after_secs: default_run_stale_after(),
            failure_backoff_max_secs: default_failure_backoff_max(),
            reconcile_interval_secs: default_reconcile_interval(),
            recsys_incremental_interval_secs: default_recsys_incremental_interval(),
            recsys_full_interval_secs: default_recsys_full_interval(),
            recsys_batch: default_recsys_batch(),
            recsys_incremental_max: default_recsys_incremental_max(),
        }
    }
}
