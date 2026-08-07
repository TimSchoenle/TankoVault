//! Togglable Prometheus metrics, and the catalogue of everything this workspace measures.
//!
//! [`MetricsRegistry`] is the on/off switch: when disabled, no recorder is installed and
//! `metrics::counter!` calls dispatch to a no-op, so domain code needs no `if`.
//!
//! [`CATALOGUE`] is the vocabulary, and it lives here — above the services, rather than beside
//! each call site — for two reasons that decide where a *new* metric goes. `crates/fetch` and
//! `crates/solver` sit below this crate and cannot import it, so a per-crate table could never
//! be the whole list. And `# HELP`, `# TYPE` and the unit come from a `describe_*` call that
//! must run at start-up, before the first measurement, or every panel built on the series has
//! no documentation on it. `xtask repo-lint` holds every `counter!`/`gauge!`/`histogram!` call
//! site in the workspace to this table.

use crate::ServiceError;
use axum::extract::{MatchedPath, Request};
use axum::middleware::Next;
use axum::response::Response;
use metrics::Unit;
use metrics_exporter_prometheus::{Matcher, PrometheusBuilder, PrometheusHandle};
use names::{HTTP_DURATION, HTTP_IN_FLIGHT, HTTP_REQUESTS};
use std::sync::Arc;
use std::time::Instant;
use tankovault_config::MetricsConfig;

/// Histogram bucket boundaries for HTTP request latency, in seconds. Tight resolution
/// around the 10ms-1s band where most handlers live.
const LATENCY_BUCKETS: &[f64] = &[
    0.001, 0.005, 0.010, 0.025, 0.050, 0.100, 0.250, 0.500, 1.0, 2.5, 5.0, 10.0,
];

/// Bucket boundaries for *background work*: a scan task, a scheduler sweep, a notification
/// fan-out, one provider fetch.
///
/// Separate from [`LATENCY_BUCKETS`], which tops out at 10s. Every one of these routinely runs
/// longer than that, so on the HTTP buckets they would all land in `+Inf` — which makes a
/// quantile unanswerable rather than merely coarse.
const WORK_BUCKETS: &[f64] = &[0.05, 0.25, 1.0, 5.0, 15.0, 60.0, 300.0, 900.0, 3600.0];

/// Every metric name in the workspace, so a call site and [`CATALOGUE`] cannot drift apart by
/// a typo.
///
/// `crates/fetch` and `crates/solver` are below this crate in the dependency graph and spell
/// their two names as literals; `repo-lint` is what keeps those honest.
pub mod names {
    /// Total HTTP requests, labelled by method, matched route and the exact status code.
    pub const HTTP_REQUESTS: &str = "http_requests_total";
    /// HTTP request latency in seconds, labelled by method and matched route.
    pub const HTTP_DURATION: &str = "http_request_duration_seconds";
    /// HTTP requests currently being served.
    pub const HTTP_IN_FLIGHT: &str = "http_requests_in_flight";
    /// Requests refused by the inbound rate limiter, by route class.
    pub const HTTP_RATE_LIMITED: &str = "http_rate_limited_total";
    /// Rate-limit counter-store failures, by backend.
    pub const RATE_LIMIT_STORE_ERRORS: &str = "rate_limit_store_errors_total";
    /// Requests refused because their feature flag is off.
    pub const FEATURE_DISABLED: &str = "http_feature_disabled_total";

    /// Constant `1`, carrying the version and service name as labels.
    pub const BUILD_INFO: &str = "tankovault_build_info";
    /// `1` when the last readiness probe passed, `0` otherwise.
    pub const SERVICE_READY: &str = "service_ready";
    /// Per-dependency readiness, `1` up / `0` down.
    pub const DEPENDENCY_UP: &str = "service_dependency_up";
    /// Postgres pool connections, by state.
    pub const DB_POOL_CONNECTIONS: &str = "db_pool_connections";
    /// The pool's configured ceiling, so saturation is a ratio rather than a guess.
    pub const DB_POOL_MAX: &str = "db_pool_max_connections";

    /// Provider HTTP requests, by provider and response class.
    pub const PROVIDER_FETCH: &str = "provider_fetch_total";
    /// Provider HTTP request duration.
    pub const PROVIDER_FETCH_DURATION: &str = "provider_fetch_duration_seconds";
    /// Server-directed backoffs (`429`/`503`), by provider and status.
    pub const PROVIDER_BACKOFF_WAITS: &str = "provider_backoff_waits_total";
    /// How long each of those backoffs parked the caller.
    pub const PROVIDER_BACKOFF_WAIT_SECONDS: &str = "provider_backoff_wait_seconds";

    /// Scan tasks handed to a worker, by provider and tier.
    pub const SCAN_TASKS_SERVED: &str = "scan_tasks_served_total";
    /// Scan tasks that reached a terminal disposition, by outcome.
    pub const SCAN_TASKS_SETTLED: &str = "scan_tasks_settled_total";
    /// How long one scan task took.
    pub const SCAN_TASK_DURATION: &str = "scan_task_duration_seconds";
    /// Scan tasks a worker is executing right now — one per provider in flight.
    pub const SCAN_TASKS_INFLIGHT: &str = "scan_tasks_inflight";
    /// Chapters a scan accepted and recorded as new.
    pub const CHAPTERS_DISCOVERED: &str = "chapters_discovered_total";
    /// Listing entries refused as implausible chapter numbers.
    pub const CHAPTERS_REJECTED: &str = "chapters_rejected_total";
    /// Catalogue walks cut short by the page budget.
    pub const CATALOG_PAGES_TRUNCATED: &str = "scan_catalog_pages_truncated_total";

    /// Scan runs the scheduler planned, by provider, tier and result.
    pub const SCAN_RUNS_PLANNED: &str = "scan_runs_planned_total";
    /// How long one scheduler sweep took, by tier.
    pub const SCHEDULER_SWEEP_DURATION: &str = "scheduler_sweep_duration_seconds";
    /// `1` on the replica currently holding scheduler leadership.
    pub const SCHEDULER_LEADER: &str = "scheduler_leader";
    /// Duplicate-sweep verdicts, by action.
    pub const MERGE_SWEEP_ACTIONS: &str = "merge_sweep_actions_total";
    /// How long one duplicate sweep took.
    pub const MERGE_SWEEP_DURATION: &str = "merge_sweep_duration_seconds";

    /// Series processed by a recommendation-model build, by stage and result.
    pub const RECSYS_BUILD_SERIES: &str = "recsys_build_series_total";
    /// How long one recommendation-model build took, by stage.
    pub const RECSYS_BUILD_DURATION: &str = "recsys_build_duration_seconds";
    /// Rows the recommendation model currently holds, by table.
    pub const RECSYS_MODEL_SERIES: &str = "recsys_model_series";
    /// Time to serve one personalised shelf, by whether it was cached.
    pub const RECSYS_SERVE_DURATION: &str = "recsys_serve_duration_seconds";
    /// How many series a served shelf contained.
    pub const RECSYS_SHELF_SIZE: &str = "recsys_shelf_size";

    /// Chapter-discovered events the notifier consumed, by result.
    pub const NOTIFICATION_EVENTS: &str = "notification_events_total";
    /// How long one event's fan-out took.
    pub const NOTIFICATION_FANOUT_DURATION: &str = "notification_fanout_duration_seconds";
    /// Durable in-app notification rows written.
    pub const NOTIFICATIONS_CREATED: &str = "notifications_created_total";
    /// External-channel delivery attempts, by channel and result.
    pub const NOTIFICATIONS_DELIVERED: &str = "notifications_delivered_total";
    /// How long one external channel took to accept or refuse a delivery.
    pub const NOTIFICATION_CHANNEL_DURATION: &str = "notification_channel_duration_seconds";

    /// `AniList` API calls, by operation and result.
    pub const ANILIST_REQUESTS: &str = "anilist_requests_total";
    /// `AniList` API call duration, by operation.
    pub const ANILIST_REQUEST_DURATION: &str = "anilist_request_duration_seconds";

    /// Authentication attempts, by operation and outcome.
    pub const AUTH_ATTEMPTS: &str = "auth_attempts_total";
    /// Currently open `/v1/me/stream` SSE connections.
    pub const SSE_STREAMS_ACTIVE: &str = "sse_streams_active";
    /// Live notification frames pushed to an SSE subscriber, by result.
    pub const SSE_EVENTS_PUSHED: &str = "sse_events_pushed_total";

    /// Challenge solve attempts, by result. Emitted by `crates/solver`.
    pub const SOLVE_ATTEMPTS: &str = "solve_attempts_total";
    /// Headless render requests, by result. Emitted by `services/render`.
    pub const RENDER_REQUESTS: &str = "render_requests_total";
}

/// How a metric is recorded — its `# TYPE`, and for a histogram its bucket boundaries.
#[derive(Clone, Copy)]
pub enum Kind {
    /// Monotonic; read with `rate()`.
    Counter,
    /// Instantaneous value.
    Gauge,
    /// Explicit buckets, never the exporter's default summary quantiles: a quantile cannot be
    /// averaged across replicas by a recording rule, a bucket can.
    Histogram(&'static [f64]),
}

/// One metric's published metadata.
pub struct Metric {
    /// The exposition name, from [`names`].
    pub name: &'static str,
    /// Counter, gauge or histogram.
    pub kind: Kind,
    /// Published as the exposition's unit hint.
    pub unit: Unit,
    /// Which services emit it. Documentation, and the answer to "does my alert have coverage
    /// on this job" — the column `docs/OBSERVABILITY.md` previously got wrong by hand.
    pub emitted_by: &'static str,
    /// The `# HELP` line, written for whoever reads it on a panel mid-incident: what the
    /// number means and what moves it, not what the code does.
    pub help: &'static str,
}

/// Every metric the workspace emits.
///
/// Adding a `counter!`/`gauge!`/`histogram!` call without adding its row here fails
/// `xtask repo-lint`, which is the only thing that can catch it — the compiler sees a string.
pub const CATALOGUE: &[Metric] = &[
    Metric {
        name: names::HTTP_REQUESTS,
        kind: Kind::Counter,
        unit: Unit::Count,
        emitted_by: "every service",
        help: "HTTP requests that produced a response, by method, matched route and exact status code.",
    },
    Metric {
        name: names::HTTP_DURATION,
        kind: Kind::Histogram(LATENCY_BUCKETS),
        unit: Unit::Seconds,
        emitted_by: "every service",
        help: "Time from entering the middleware stack to the response, including any wait on the rate limiter.",
    },
    Metric {
        name: names::HTTP_IN_FLIGHT,
        kind: Kind::Gauge,
        unit: Unit::Count,
        emitted_by: "every service",
        help: "Requests currently being served. Includes long-lived SSE streams, so on api and frontend this partly counts connected browsers.",
    },
    Metric {
        name: names::HTTP_RATE_LIMITED,
        kind: Kind::Counter,
        unit: Unit::Count,
        emitted_by: "services with a limiter",
        help: "Requests refused with 429 by the inbound rate limiter, by route class.",
    },
    Metric {
        name: names::RATE_LIMIT_STORE_ERRORS,
        kind: Kind::Counter,
        unit: Unit::Count,
        emitted_by: "services on the Redis limiter backend",
        help: "Rate-limit counter-store failures. The limiter fails open, so any non-zero rate means limits are not being enforced.",
    },
    Metric {
        name: names::FEATURE_DISABLED,
        kind: Kind::Counter,
        unit: Unit::Count,
        emitted_by: "api, sync",
        help: "Requests answered 404 because the feature flag gating the route is off.",
    },
    Metric {
        name: names::BUILD_INFO,
        kind: Kind::Gauge,
        unit: Unit::Count,
        emitted_by: "every service",
        help: "Always 1. Carries the service name and version as labels; join on it to annotate a panel with the running build.",
    },
    Metric {
        name: names::SERVICE_READY,
        kind: Kind::Gauge,
        unit: Unit::Count,
        emitted_by: "every service",
        help: "1 when the last readiness probe passed. Only updated while something probes /ready.",
    },
    Metric {
        name: names::DEPENDENCY_UP,
        kind: Kind::Gauge,
        unit: Unit::Count,
        emitted_by: "every service",
        help: "Per-dependency readiness (postgres, nats, redis): 1 up, 0 down. Names which dependency failed without parsing the /ready body.",
    },
    Metric {
        name: names::DB_POOL_CONNECTIONS,
        kind: Kind::Gauge,
        unit: Unit::Count,
        emitted_by: "api, control-plane, worker, notifier, sync",
        help: "Postgres pool connections by state (in_use, idle). Sustained in_use at the ceiling is pool saturation, which shows up as latency long before /ready fails.",
    },
    Metric {
        name: names::DB_POOL_MAX,
        kind: Kind::Gauge,
        unit: Unit::Count,
        emitted_by: "api, control-plane, worker, notifier, sync",
        help: "The pool's configured maximum, so saturation is a ratio against an observed number rather than a hardcoded one.",
    },
    Metric {
        name: names::PROVIDER_FETCH,
        kind: Kind::Counter,
        unit: Unit::Count,
        emitted_by: "worker",
        help: "Outbound provider HTTP requests, by provider and response class (2xx/3xx/4xx/5xx/error). A provider that has started refusing us shows here first.",
    },
    Metric {
        name: names::PROVIDER_FETCH_DURATION,
        kind: Kind::Histogram(WORK_BUCKETS),
        unit: Unit::Seconds,
        emitted_by: "worker",
        help: "Time for one provider HTTP request, excluding rate-limiter and backoff waits above it.",
    },
    Metric {
        name: names::PROVIDER_BACKOFF_WAITS,
        kind: Kind::Counter,
        unit: Unit::Count,
        emitted_by: "worker",
        help: "Times a provider answered 429/503 and the crawler backed off, by provider and status. Scans get slower, never fail, so this is the only signal.",
    },
    Metric {
        name: names::PROVIDER_BACKOFF_WAIT_SECONDS,
        kind: Kind::Histogram(WORK_BUCKETS),
        unit: Unit::Seconds,
        emitted_by: "worker",
        help: "How long one backoff parked the crawler. Sum over a provider is time that provider has throttled us out of.",
    },
    Metric {
        name: names::SCAN_TASKS_SERVED,
        kind: Kind::Counter,
        unit: Unit::Count,
        emitted_by: "worker",
        help: "Scan tasks handed to a worker, by provider and tier. A redelivery counts again, so pair with scan_tasks_settled_total to separate throughput from retries.",
    },
    Metric {
        name: names::SCAN_TASKS_SETTLED,
        kind: Kind::Counter,
        unit: Unit::Count,
        emitted_by: "worker",
        help: "Scan tasks that reached a terminal state, by outcome (completed, requeued, failed). failed means the delivery budget was exhausted or the error was not retryable.",
    },
    Metric {
        name: names::SCAN_TASK_DURATION,
        kind: Kind::Histogram(WORK_BUCKETS),
        unit: Unit::Seconds,
        emitted_by: "worker",
        help: "Wall time for one scan task, by provider, tier and task kind.",
    },
    Metric {
        name: names::SCAN_TASKS_INFLIGHT,
        kind: Kind::Gauge,
        unit: Unit::Count,
        emitted_by: "worker",
        help: "Scan tasks a worker is executing right now. A worker runs at most one task per provider, so this is also the number of providers being crawled concurrently, and it is capped by worker.max_concurrent_providers. Sitting at the cap means the queue is the constraint; sitting below it while tasks are queued means every remaining provider already has a task in flight.",
    },
    Metric {
        name: names::CHAPTERS_DISCOVERED,
        kind: Kind::Counter,
        unit: Unit::Count,
        emitted_by: "worker",
        help: "Chapters a scan recorded as new, by provider. The pipeline's output: a healthy fleet with this flat is doing work and finding nothing.",
    },
    Metric {
        name: names::CHAPTERS_REJECTED,
        kind: Kind::Counter,
        unit: Unit::Count,
        emitted_by: "worker",
        help: "Listing entries refused because the source cannot plausibly have released them. A trickle is normal; a step change on one provider is an adapter or a junk-slug problem.",
    },
    Metric {
        name: names::CATALOG_PAGES_TRUNCATED,
        kind: Kind::Counter,
        unit: Unit::Count,
        emitted_by: "worker",
        help: "Catalogue walks cut short by max_catalog_pages, by provider. Non-zero means part of that provider's catalogue was never seen.",
    },
    Metric {
        name: names::SCAN_RUNS_PLANNED,
        kind: Kind::Counter,
        unit: Unit::Count,
        emitted_by: "control-plane",
        help: "Scan runs the scheduler planned, by provider, tier and result (planned, duplicate, error). The upstream half of the scan pipeline.",
    },
    Metric {
        name: names::SCHEDULER_SWEEP_DURATION,
        kind: Kind::Histogram(WORK_BUCKETS),
        unit: Unit::Seconds,
        emitted_by: "control-plane",
        help: "Time for one scheduler sweep over all active providers, by tier.",
    },
    Metric {
        name: names::SCHEDULER_LEADER,
        kind: Kind::Gauge,
        unit: Unit::Count,
        emitted_by: "control-plane",
        help: "1 on the replica holding scheduler leadership. Summed across the job it must be exactly 1; 0 means nothing is planning scans.",
    },
    Metric {
        name: names::MERGE_SWEEP_ACTIONS,
        kind: Kind::Counter,
        unit: Unit::Count,
        emitted_by: "control-plane",
        help: "Duplicate-sweep verdicts by action (examined, auto_merged, queued, withdrawn, deferred). auto_merged is destructive and bounded by the merge ceiling.",
    },
    Metric {
        name: names::MERGE_SWEEP_DURATION,
        kind: Kind::Histogram(WORK_BUCKETS),
        unit: Unit::Seconds,
        emitted_by: "control-plane",
        help: "Time for one duplicate sweep.",
    },
    Metric {
        name: names::RECSYS_BUILD_SERIES,
        kind: Kind::Counter,
        unit: Unit::Count,
        emitted_by: "control-plane",
        help: "Series processed by a recommendation-model build, labelled by stage (full/incremental) and result. A `failed` increment means the model stopped updating; pair it with recsys_model_series going flat.",
    },
    Metric {
        name: names::RECSYS_BUILD_DURATION,
        kind: Kind::Histogram(WORK_BUCKETS),
        unit: Unit::Seconds,
        emitted_by: "control-plane",
        help: "Time for one recommendation-model build, by stage. A full build re-solves the projection over the whole catalogue and is expected in minutes; an incremental one touches only what changed.",
    },
    Metric {
        name: names::RECSYS_MODEL_SERIES,
        kind: Kind::Gauge,
        unit: Unit::Count,
        emitted_by: "control-plane",
        help: "Series the recommendation model currently covers, by table. Flat while the catalogue grows means builds are failing or the scheduler is not leading anywhere.",
    },
    Metric {
        name: names::RECSYS_SERVE_DURATION,
        kind: Kind::Histogram(LATENCY_BUCKETS),
        unit: Unit::Seconds,
        emitted_by: "api",
        help: "Time to serve one personalised shelf, labelled by whether it came from the cache. The computed path runs one ANN search per seed, so its tail is where the seed count shows up.",
    },
    Metric {
        name: names::RECSYS_SHELF_SIZE,
        kind: Kind::Histogram(&[0.0, 1.0, 3.0, 6.0, 12.0, 24.0, 60.0]),
        unit: Unit::Count,
        emitted_by: "api",
        help: "Series in a served shelf. A rising share of zeroes is the earliest symptom of a broken or unbuilt model, and is invisible in the request latency.",
    },
    Metric {
        name: names::NOTIFICATION_EVENTS,
        kind: Kind::Counter,
        unit: Unit::Count,
        emitted_by: "notifier",
        help: "Chapter-discovered events consumed, by result (ok, error). The notifier's throughput.",
    },
    Metric {
        name: names::NOTIFICATION_FANOUT_DURATION,
        kind: Kind::Histogram(WORK_BUCKETS),
        unit: Unit::Seconds,
        emitted_by: "notifier",
        help: "Time to fan one event out to every watcher and channel. The consumer acks only after this, so a hanging channel shows here before the backlog moves.",
    },
    Metric {
        name: names::NOTIFICATIONS_CREATED,
        kind: Kind::Counter,
        unit: Unit::Count,
        emitted_by: "notifier",
        help: "Durable in-app notification rows written, after read-progress filtering and cross-provider dedup.",
    },
    Metric {
        name: names::NOTIFICATIONS_DELIVERED,
        kind: Kind::Counter,
        unit: Unit::Count,
        emitted_by: "notifier",
        help: "External-channel delivery attempts by channel and result (ok, error, skipped). A relay that fails fast is invisible everywhere else: the notifier logs, acks and moves on.",
    },
    Metric {
        name: names::NOTIFICATION_CHANNEL_DURATION,
        kind: Kind::Histogram(WORK_BUCKETS),
        unit: Unit::Seconds,
        emitted_by: "notifier",
        help: "Time one external channel took to accept or refuse a delivery. An SMTP relay that hangs is a long tail here.",
    },
    Metric {
        name: names::ANILIST_REQUESTS,
        kind: Kind::Counter,
        unit: Unit::Count,
        emitted_by: "sync",
        help: "AniList API calls by operation and result (ok, error, rate_limited). rate_limited is AniList throttling us, which widens every later request's gap.",
    },
    Metric {
        name: names::ANILIST_REQUEST_DURATION,
        kind: Kind::Histogram(WORK_BUCKETS),
        unit: Unit::Seconds,
        emitted_by: "sync",
        help: "AniList call duration by operation, including the paced wait before the request goes out.",
    },
    Metric {
        name: names::AUTH_ATTEMPTS,
        kind: Kind::Counter,
        unit: Unit::Count,
        emitted_by: "api",
        help: "Authentication attempts by operation (login, register, refresh, passkey) and outcome (success, failure, denied). denied is a policy refusal such as a locked or unverified account.",
    },
    Metric {
        name: names::SSE_STREAMS_ACTIVE,
        kind: Kind::Gauge,
        unit: Unit::Count,
        emitted_by: "api",
        help: "Open /v1/me/stream connections, i.e. signed-in browsers holding a live notification channel.",
    },
    Metric {
        name: names::SSE_EVENTS_PUSHED,
        kind: Kind::Counter,
        unit: Unit::Count,
        emitted_by: "api",
        help: "Live notification frames pushed to a subscriber, by result. The push is fire-and-forget; the durable row survives a drop.",
    },
    Metric {
        name: names::SOLVE_ATTEMPTS,
        kind: Kind::Counter,
        unit: Unit::Count,
        emitted_by: "challenge-solver, render",
        help: "Challenge solve attempts by result (ok, error, rejected). rejected is an SSRF refusal, not a failure to solve.",
    },
    Metric {
        name: names::RENDER_REQUESTS,
        kind: Kind::Counter,
        unit: Unit::Count,
        emitted_by: "render",
        help: "Headless render requests by result (ok, error, rejected). rejected is an SSRF refusal.",
    },
];

/// Publish `# HELP`, `# TYPE` and the unit for every metric in [`CATALOGUE`].
///
/// Runs at install time, before any measurement: a `describe_*` after the first record is
/// ignored by the exporter, which is how a series ends up on a dashboard with no
/// documentation attached to it.
fn describe_all() {
    for metric in CATALOGUE {
        match metric.kind {
            Kind::Counter => metrics::describe_counter!(metric.name, metric.unit, metric.help),
            Kind::Gauge => metrics::describe_gauge!(metric.name, metric.unit, metric.help),
            Kind::Histogram(_) => {
                metrics::describe_histogram!(metric.name, metric.unit, metric.help);
            }
        }
    }
}

/// A cheap, cloneable handle onto the process's metrics facility — or onto nothing, when
/// metrics are switched off.
#[derive(Clone)]
pub struct MetricsRegistry {
    /// `None` when disabled. Its presence *is* the enabled flag; there is no separate
    /// boolean that could disagree with whether a recorder was actually installed.
    handle: Option<PrometheusHandle>,
    http_requests: bool,
    route: Arc<str>,
    /// When `Some`, the scrape endpoint is served on this dedicated address instead of the
    /// service's primary port. `None` keeps it merged onto the main port.
    listen: Option<Arc<str>>,
}

impl MetricsRegistry {
    /// Install the process-wide Prometheus recorder if `cfg` enables it, publish the
    /// [`CATALOGUE`] metadata, and record [`names::BUILD_INFO`] for `service`.
    ///
    /// `service` is the only place a service names itself in the metric stream. Every other
    /// metric stays unlabelled by service on purpose — Prometheus' `job` is what identifies
    /// the emitter — but a `build_info` series is what lets a dashboard annotate a panel with
    /// the running version, and it gives even an idle service one series to prove it is
    /// scrapable at all.
    ///
    /// # Errors
    /// Returns [`ServiceError::Metrics`] if a recorder is already installed, or if a bucket
    /// set in [`CATALOGUE`] is rejected.
    pub fn install(cfg: &MetricsConfig, service: &str) -> Result<Self, ServiceError> {
        if !cfg.enabled {
            tracing::info!("metrics disabled by configuration; no recorder installed");
            return Ok(Self::disabled());
        }

        // Buckets are registered from the catalogue rather than one call per histogram, so a
        // new histogram cannot be added with the exporter's default summary behaviour by
        // omission — the row that documents it is also the row that configures it.
        let mut builder = PrometheusBuilder::new();
        for metric in CATALOGUE {
            if let Kind::Histogram(buckets) = metric.kind {
                builder = builder
                    .set_buckets_for_metric(Matcher::Full(metric.name.to_owned()), buckets)
                    .map_err(|e| ServiceError::Metrics(e.to_string()))?;
            }
        }
        let handle = builder
            .install_recorder()
            .map_err(|e| ServiceError::Metrics(e.to_string()))?;

        describe_all();
        metrics::gauge!(
            names::BUILD_INFO,
            "service" => service.to_owned(),
            "version" => env!("CARGO_PKG_VERSION"),
        )
        .set(1.0);

        tracing::info!(
            route = %cfg.route,
            http_requests = cfg.http_requests,
            "metrics enabled"
        );
        Ok(Self {
            handle: Some(handle),
            http_requests: cfg.http_requests,
            route: Arc::from(cfg.route.as_str()),
            listen: cfg.listen.as_deref().map(Arc::from),
        })
    }

    /// A registry that records nothing — for tests and for services that opt out.
    #[must_use]
    pub fn disabled() -> Self {
        Self {
            handle: None,
            http_requests: false,
            route: Arc::from("/metrics"),
            listen: None,
        }
    }

    /// A disabled registry with an explicit scrape `route` and optional `listen` address,
    /// for exercising the router-wiring decisions without installing a global recorder.
    #[cfg(test)]
    pub(crate) fn disabled_with_listen(route: &str, listen: Option<&str>) -> Self {
        Self {
            handle: None,
            http_requests: false,
            route: Arc::from(route),
            listen: listen.map(Arc::from),
        }
    }

    /// Whether a recorder is installed and the scrape endpoint should serve.
    #[must_use]
    pub fn is_enabled(&self) -> bool {
        self.handle.is_some()
    }

    /// Whether the per-request HTTP middleware should be mounted.
    #[must_use]
    pub fn records_http_requests(&self) -> bool {
        self.handle.is_some() && self.http_requests
    }

    /// Path the scrape endpoint is mounted at.
    #[must_use]
    pub fn route(&self) -> &str {
        &self.route
    }

    /// The dedicated address the scrape endpoint should bind to, when it is isolated to its
    /// own port. `None` means the scrape stays merged onto the service's primary port.
    #[must_use]
    pub fn listen(&self) -> Option<&str> {
        self.listen.as_deref()
    }

    /// Render the Prometheus exposition text, or `None` when metrics are disabled.
    #[must_use]
    pub fn render(&self) -> Option<String> {
        self.handle.as_ref().map(PrometheusHandle::render)
    }
}

/// Record the outcome of a readiness probe.
///
/// Called from [`crate::health::Health::report`], so every service gets it without wiring:
/// the gauges track whatever probes `/ready`, and go stale — not to zero — if nothing does.
/// That is why the availability alerts key on `up`, and these answer *which dependency*.
pub(crate) fn record_readiness(report: &crate::health::HealthReport) {
    metrics::gauge!(names::SERVICE_READY).set(f64::from(u8::from(report.is_ready())));
    for check in &report.checks {
        let up = check.status == crate::health::HealthStatus::Up;
        metrics::gauge!(names::DEPENDENCY_UP, "dependency" => check.name)
            .set(f64::from(u8::from(up)));
    }
}

/// How often the pool gauges are refreshed. Well under any alert's `for:` window, and cheap —
/// both readings are atomics on the pool, not a query.
#[cfg(feature = "db")]
const POOL_SAMPLE_INTERVAL: std::time::Duration = std::time::Duration::from_secs(10);

/// Sample `pool` onto [`names::DB_POOL_CONNECTIONS`] until `shutdown`.
///
/// Sampled rather than recorded at acquire/release: the interesting quantity is the standing
/// level, and instrumenting every checkout would put a metrics call on the hot path of every
/// query to measure something a 10-second poll already answers.
#[cfg(feature = "db")]
pub fn spawn_pool_sampler(pool: tankovault_db::PgPool, shutdown: crate::CancellationToken) {
    tokio::spawn(async move {
        crate::shutdown::every(
            POOL_SAMPLE_INTERVAL,
            shutdown,
            "db-pool-sampler",
            move || {
                let pool = pool.clone();
                async move {
                    let size = f64::from(pool.size());
                    let idle = f64::from(u32::try_from(pool.num_idle()).unwrap_or(u32::MAX));
                    metrics::gauge!(names::DB_POOL_CONNECTIONS, "state" => "idle").set(idle);
                    // `size` counts established connections, so in-use is what is left of it.
                    // Deriving rather than publishing `size` keeps the two states summing to it.
                    metrics::gauge!(names::DB_POOL_CONNECTIONS, "state" => "in_use")
                        .set((size - idle).max(0.0));
                    metrics::gauge!(names::DB_POOL_MAX)
                        .set(f64::from(pool.options().get_max_connections()));
                }
            },
        )
        .await;
    });
}

/// Holds one unit of [`HTTP_IN_FLIGHT`] for as long as it is alive.
///
/// Decrement lives in `Drop`, not at the end of [`track_request`]: a client disconnect
/// drops the service future mid-`await`, skipping any statement placed after it, which
/// previously leaked one unit per disconnect on every SSE stream.
struct InFlightGuard;

impl InFlightGuard {
    fn enter() -> Self {
        metrics::gauge!(HTTP_IN_FLIGHT).increment(1.0);
        Self
    }
}

impl Drop for InFlightGuard {
    fn drop(&mut self) {
        metrics::gauge!(HTTP_IN_FLIGHT).decrement(1.0);
    }
}

/// Record per-request metrics around the rest of the stack.
///
/// Labels use axum's [`MatchedPath`], not the concrete URI, so cardinality stays bounded by
/// the route table; unmatched (attacker-controlled) paths fold into `unmatched`. An abandoned
/// request is counted in [`HTTP_IN_FLIGHT`] but not [`HTTP_REQUESTS`]/[`HTTP_DURATION`], which
/// record from a response a dropped future never produces.
pub async fn track_request(req: Request, next: Next) -> Response {
    let method = req.method().clone();
    let route: String = req
        .extensions()
        .get::<MatchedPath>()
        .map_or_else(|| "unmatched".to_owned(), |p| p.as_str().to_owned());

    let _in_flight = InFlightGuard::enter();
    let started = Instant::now();
    let response = next.run(req).await;
    let elapsed = started.elapsed();

    let status = response.status().as_u16().to_string();
    metrics::counter!(
        HTTP_REQUESTS,
        "method" => method.to_string(),
        "route" => route.clone(),
        "status" => status,
    )
    .increment(1);
    metrics::histogram!(
        HTTP_DURATION,
        "method" => method.to_string(),
        "route" => route,
    )
    .record(elapsed.as_secs_f64());

    response
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn disabled_registry_renders_nothing() {
        let registry = MetricsRegistry::disabled();
        assert!(!registry.is_enabled());
        assert!(!registry.records_http_requests());
        assert!(registry.render().is_none());
    }

    #[test]
    fn disabled_config_installs_no_recorder() {
        let cfg = MetricsConfig {
            enabled: false,
            ..MetricsConfig::default()
        };
        let registry =
            MetricsRegistry::install(&cfg, "test").expect("disabled install cannot fail");
        assert!(!registry.is_enabled());
    }

    /// Every metric the workspace emits must be described, and described once: a duplicate
    /// row would publish two `# HELP` lines for one series, and a missing one is what
    /// `repo-lint` catches at the call site.
    #[test]
    fn the_catalogue_has_no_duplicate_names() {
        let mut names: Vec<&str> = CATALOGUE.iter().map(|m| m.name).collect();
        names.sort_unstable();
        let before = names.len();
        names.dedup();
        assert_eq!(before, names.len(), "duplicate metric name in CATALOGUE");
    }

    /// A histogram with no explicit buckets falls back to the exporter's summary quantiles,
    /// which cannot be aggregated across replicas — the reason `LATENCY_BUCKETS` exists.
    #[test]
    fn every_histogram_carries_buckets() {
        for metric in CATALOGUE {
            if let Kind::Histogram(buckets) = metric.kind {
                assert!(!buckets.is_empty(), "{} has empty buckets", metric.name);
            }
        }
    }

    /// The catalogue has to reach the *exposition*, not just exist.
    ///
    /// Two failures this pins, both silent and both invisible in the source. A `describe_*`
    /// that runs after the first measurement is dropped by the exporter, and the series then
    /// appears on a dashboard with no `# HELP` at all. And a histogram whose buckets were not
    /// registered on the builder renders as `# TYPE … summary` — still a working panel, but
    /// carrying per-replica quantiles that no recording rule can aggregate, which is the exact
    /// thing every latency rule in the chart depends on not happening.
    #[test]
    fn the_catalogue_reaches_the_exposition() {
        let recorder = build_recorder_with_catalogue_buckets();
        let handle = recorder.handle();
        metrics::with_local_recorder(&recorder, || {
            describe_all();
            metrics::counter!(names::NOTIFICATIONS_DELIVERED, "channel" => "email").increment(1);
            metrics::histogram!(names::SCAN_TASK_DURATION, "provider" => "p").record(42.0);
        });
        let rendered = handle.render();

        assert!(
            rendered.contains(&format!("# HELP {} ", names::NOTIFICATIONS_DELIVERED)),
            "no HELP line for a described counter:\n{rendered}"
        );
        assert!(
            rendered.contains(&format!("# TYPE {} histogram", names::SCAN_TASK_DURATION)),
            "a catalogue histogram rendered as something other than a histogram:\n{rendered}"
        );
        assert!(
            rendered.contains("scan_task_duration_seconds_bucket"),
            "no buckets emitted for a catalogue histogram:\n{rendered}"
        );
    }

    /// The builder [`MetricsRegistry::install`] assembles, without installing it globally.
    fn build_recorder_with_catalogue_buckets() -> metrics_exporter_prometheus::PrometheusRecorder {
        let mut builder = PrometheusBuilder::new();
        for metric in CATALOGUE {
            if let Kind::Histogram(buckets) = metric.kind {
                builder = builder
                    .set_buckets_for_metric(Matcher::Full(metric.name.to_owned()), buckets)
                    .expect("catalogue buckets should be accepted");
            }
        }
        builder.build_recorder()
    }

    /// Bug pinned: an inline decrement after `next.run(req).await` never ran on a dropped
    /// future, leaking one unit per abandoned SSE stream. Do not move it back inline.
    #[test]
    fn in_flight_gauge_is_released_when_the_request_future_is_dropped() {
        // A local recorder, so this test observes real gauge values without installing the
        // process-wide one the sibling tests assert is absent.
        let mut builder = PrometheusBuilder::new();
        for metric in CATALOGUE {
            if let Kind::Histogram(buckets) = metric.kind {
                builder = builder
                    .set_buckets_for_metric(Matcher::Full(metric.name.to_owned()), buckets)
                    .expect("buckets");
            }
        }
        let recorder = builder.build_recorder();
        let handle = recorder.handle();

        metrics::with_local_recorder(&recorder, || {
            let guard = InFlightGuard::enter();
            assert_eq!(in_flight_value(&handle.render()), Some(1.0));
            // Standing in for the future being dropped mid-`await`: the guard goes out of
            // scope without `track_request` ever reaching its final statement.
            drop(guard);
            assert_eq!(in_flight_value(&handle.render()), Some(0.0));
        });
    }

    /// The unlabelled `http_requests_in_flight` sample from a Prometheus exposition body.
    fn in_flight_value(exposition: &str) -> Option<f64> {
        exposition
            .lines()
            .find_map(|line| line.strip_prefix("http_requests_in_flight "))
            .and_then(|value| value.trim().parse().ok())
    }

    #[test]
    fn http_request_tracking_requires_the_recorder() {
        // `http_requests` alone must not mount the layer: recording into a registry with
        // no installed recorder is pure overhead with nothing to scrape.
        let cfg = MetricsConfig {
            enabled: false,
            http_requests: true,
            ..MetricsConfig::default()
        };
        let registry =
            MetricsRegistry::install(&cfg, "test").expect("disabled install cannot fail");
        assert!(!registry.records_http_requests());
    }
}
