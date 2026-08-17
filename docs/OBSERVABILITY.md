# Observability Reference

What this deployment measures, what is collected, which alerts exist, and what to do when each
one fires.

Every service installs the shared Prometheus recorder from `crates/service` and serves the
exposition at `/metrics` on its own listener (`TANKOVAULT_METRICS__LISTEN`, default
`0.0.0.0:9090`) so a scrape never touches the request-facing port. That listener carries the
scrape and nothing else — the health probes stay on the service's primary port. `xtask repo-lint`
holds every service to that wiring; it used to be convention, and a service that forgot
`spawn_metrics_server` would have compiled, reported healthy and been silently unscrapable.

The **collection** side is not in this repository. The scrape config, the recording and alerting
rules below, and the provisioned dashboard live in the chart that deploys them,
[`TimSchoenle/helm-charts`](https://github.com/TimSchoenle/helm-charts) (`charts/tankovault`):
`rules/` for the two rule files, `templates/prometheusrule.yaml`, `templates/servicemonitor.yaml`
and `templates/grafanadashboard.yaml` for the wiring, and `tests/observability_test.yaml` for the
gate. A `deploy/observability/` copy plus a compose overlay lived here until 2026-08-03; two sets
of rules with only one of them deployed is how the deployed set goes stale.

This document stays here because it is about what the *code* emits and what each alert means —
neither of which moves with the manifests. Every rule name below is the name in the chart.

---

## 1. What is measured

The inventory is **not maintained here.** It lives in code, as
`tankovault_service::metrics::CATALOGUE` (`crates/service/src/metrics.rs`): one row per metric,
carrying its type, unit, `# HELP` text, histogram buckets and the services that emit it. That row
is what publishes the `describe_*` at start-up, so the help text an operator reads on a Grafana
panel is the same string a developer reads next to the call site.

It is a gate, not a convention. `xtask repo-lint` resolves every `counter!`, `gauge!` and
`histogram!` in the workspace back to the catalogue and fails on a metric that is emitted without
a row, a row nothing emits, or a name that is neither a literal nor a `names::*` constant. This
document previously claimed a hand-verified list and was wrong in two places at once — a row
missing from `OPERATIONS.md`, and a `Present on` column asserting coverage on two services whose
code cannot produce it. Hand-verification is what the rule replaces.

The table below is therefore a **reading guide**, not the source of truth. Regenerate your
mental model from `CATALOGUE` when they disagree.

### Request path — every service

| Metric | Type | Labels |
|---|---|---|
| `http_requests_total` | counter | `method`, `route`, `status` |
| `http_request_duration_seconds` | histogram | `method`, `route` |
| `http_requests_in_flight` | gauge | — |
| `tankovault_build_info` | gauge | `service`, `version` |
| `service_ready` | gauge | — |
| `service_dependency_up` | gauge | `dependency` |

`tankovault_build_info` is the one metric that names its emitter. Everything else stays
unlabelled by service — `job` identifies the emitter — but a constant `1` carrying the version
is what lets a panel annotate a deploy, and it gives even a service with no HTTP routes one
series proving the scrape works. `service_ready` and `service_dependency_up` are written by
`Health::report`, so they move only while something probes `/ready`: they answer *which
dependency* failed, and `up`/`probe_success` remain the availability signals.

### Edge policy

| Metric | Type | Labels | Present on |
|---|---|---|---|
| `http_rate_limited_total` | counter | `class` (`global`/`auth`/`expensive`) | services with a limiter |
| `rate_limit_store_errors_total` | counter | `backend` (`redis`) | services on the Redis backend |
| `http_feature_disabled_total` | counter | `feature` | `api`, `sync` |
| `http_account_required_total` | counter | — | `api` |
| `auth_attempts_total` | counter | `operation`, `result` (`success`/`failure`/`denied`) | `api` |

`http_feature_disabled_total` is emitted by `flags::enforce`, the HTTP middleware — and only
`api` and `sync` mount it. `control-plane` and `notifier` consult `FeatureGate::is_enabled`
directly inside background loops, which records nothing. The earlier claim that all four emit it
was the defect that motivated the lint rule.

`http_account_required_total` counts requests refused because `accounts.required` is on and the
caller presented no account (`api` only — see OPERATIONS.md §4). Unlabelled: the flag is
deployment-wide, so the interesting series is its rate, and a `route` label would only re-derive
what `http_requests_total{status="401"}` already carries. It stays at zero on a public
deployment, which makes any non-zero rate a statement about the deployment rather than a signal
to interpret.

### Database — `api`, `control-plane`, `worker`, `notifier`, `sync`

| Metric | Type | Labels |
|---|---|---|
| `db_pool_connections` | gauge | `state` (`in_use`/`idle`) |
| `db_pool_max_connections` | gauge | — |

Sampled every 10s rather than recorded per checkout. `in_use / db_pool_max_connections` is pool
saturation, and it climbs long before `/ready` fails — which is what made pool exhaustion a
lagging indicator before.

### Crawl tier — `worker`

| Metric | Type | Labels |
|---|---|---|
| `provider_fetch_total` | counter | `provider`, `outcome` (`2xx`/`3xx`/`4xx`/`5xx`/`other`/`error`) |
| `provider_fetch_duration_seconds` | histogram | `provider` |
| `provider_backoff_waits_total` | counter | `provider`, `status` |
| `provider_backoff_wait_seconds` | histogram | `provider` |

`outcome` is the status **class**, not the code: crossed with `provider`, a hostile site's
status codes would be an unbounded label source for a distinction no panel makes.

### Scan pipeline

| Metric | Type | Labels | Emitted by |
|---|---|---|---|
| `scan_runs_planned_total` | counter | `provider`, `scan`, `result` (`planned`/`duplicate`/`coalesced`/`cooling_down`/`error`) | `control-plane` |
| `scheduler_sweep_duration_seconds` | histogram | `scan` | `control-plane` |
| `scheduler_leader` | gauge | — | `control-plane` |
| `merge_sweep_actions_total` | counter | `action` | `control-plane` |
| `merge_sweep_duration_seconds` | histogram | `scope` (`full`/`rotation`) | `control-plane` |
| `scan_tasks_served_total` | counter | `provider`, `scan` (`full`/`fast`) | `worker` |
| `scan_tasks_settled_total` | counter | `provider`, `scan`, `outcome` (`completed`/`requeued`/`failed`) | `worker` |
| `scan_task_duration_seconds` | histogram | `provider`, `scan`, `kind` | `worker` |
| `scan_stage_duration_seconds` | histogram | `provider`, `kind`, `stage` | `worker` |
| `scan_task_pace_wait_seconds` | histogram | `provider` | `worker` |
| `provider_pace_wait_seconds` | histogram | `provider` | `worker`, `sync` |
| `chapters_discovered_total` | counter | `provider` | `worker` |
| `chapters_rejected_total` | counter | `provider` | `worker` |
| `scan_catalog_pages_truncated_total` | counter | `provider` | `worker` |

`sum(scheduler_leader)` over the `control-plane` job must be exactly `1`. `0` means nothing is
planning scans — a failure that previously looked identical to an idle deployment, because the
only evidence was a `debug` log line on the replicas that skipped.

`scan_tasks_served_total` counts hand-outs and a redelivery increments it again, so it conflates
throughput with retries; `scan_tasks_settled_total{outcome="failed"}` is the permanent-failure
rate, and `outcome="requeued"` is the retry rate the broker's `num_redelivered` shows in
progress. `chapters_discovered_total` is the pipeline's actual output — a fleet that is busy and
flat here is doing work and finding nothing.

### Why a scan is slow

`scan_task_duration_seconds` says a task took nine minutes; it does not say what for, and that is
the question an operator actually has. Two metrics answer it without opening the database.

`scan_stage_duration_seconds{stage}` splits a task into the things it does — `catalog_fetch`,
`catalog_register`, `catalog_fanout`, `feed_fetch`, `feed_fanout`, `series_metadata`,
`series_chapters`, `series_ingest`. Time in a `*_fetch` stage is the provider's; time in
`series_ingest` is ours, and a rising ingest share is a database problem wearing a scan's clothes.

`scan_task_pace_wait_seconds` is the one to read first. It is the part of a task spent waiting for
*permission to send* — the concurrency gate, the token rate, the crawl delay, and the adaptive
penalty a 429 imposes. Against `scan_task_duration_seconds` it separates the only two diagnoses
that matter:

```promql
sum(rate(scan_task_pace_wait_seconds_sum[15m])) by (provider)
  / sum(rate(scan_task_duration_seconds_sum[15m])) by (provider)
```

Near 1 means the scan is **polite, not broken**: it is being crawled exactly as fast as that
provider's budget allows, and nothing in the code will speed it up — raise `politeness.rps` or
lower `crawl_delay_ms` for that provider, or accept the duration. Near 0 with a long duration
means the time is in the requests themselves or in the ingest, and
`scan_stage_duration_seconds{stage}` says which. `provider_pace_wait_seconds` is the same wait per
request rather than per task, and it climbs on its own when a provider answers 429/503 — a rise
there with no config change is the adaptive penalty working.

The same breakdown is stored per task (`scan_tasks.telemetry`) and surfaced per run by
`GET /v1/admin/scans/{run_id}/tasks`, which is what the console's "Why so long?" panel reads.

`chapters_rejected_total` counts listing entries a scan refused to index because the source
cannot plausibly have released them (see `chapter_outliers` in [`CONFIGURATION.md`](CONFIGURATION.md)).
A steady trickle is normal — it is what the rule exists for. A **step change on one provider** is
the signal worth acting on: either that site started publishing junk slugs, or an adapter change
altered how its numbers parse and the rule is now discarding real chapters. The `warn` log at the
same site carries the numbers, which is what tells the two apart.

### Notifications

| Metric | Type | Labels | Emitted by |
|---|---|---|---|
| `notification_events_total` | counter | `result` (`ok`/`error`) | `notifier` |
| `notification_fanout_duration_seconds` | histogram | — | `notifier` |
| `notifications_created_total` | counter | — | `notifier` |
| `notifications_delivered_total` | counter | `channel`, `result` (`ok`/`error`/`skipped`) | `notifier` |
| `notification_channel_duration_seconds` | histogram | `channel` | `notifier` |
| `sse_streams_active` | gauge | — | `api` |
| `sse_events_pushed_total` | counter | `result` (`ok`/`error`/`undecodable`) | `api` |

Before these, `notifier` mounted the shared middleware over an **empty router** and so emitted
nothing at all: its scrape answered `200` with an empty body, and the only thing distinguishing a
notifier delivering everything from one delivering nothing was the broker's backlog.

`sse_streams_active` exists because `http_requests_in_flight` cannot answer this honestly — see
the cancellation note below.

### Fetch tier

| Metric | Type | Labels | Emitted by |
|---|---|---|---|
| `solve_attempts_total` | counter | `result` (`ok`/`unavailable`/`error`/`rejected`) | `challenge-solver` **and** `render` |
| `solve_retries_total` | counter | `provider` | `worker` (`crates/fetch`) |
| `render_requests_total` | counter | `result` (`ok`/`error`/`rejected`) | `render` |

### AniList — `sync`

| Metric | Type | Labels |
|---|---|---|
| `anilist_requests_total` | counter | `operation`, `result` (`ok`/`error`/`rate_limited`) |
| `anilist_request_duration_seconds` | histogram | `operation` |

`operation` is a `&'static str` the caller passes (`viewer`, `media_list`, `save_entry`,
`search`, `metadata_by_id`, `metadata_by_title`), never anything derived from the query text —
a label taken from a request body is unbounded the moment a query is built dynamically.
`rate_limited` is separated from `error` because it is not a fault in this deployment and it
widens every later request's gap; it is the one outcome answered by waiting.

One thing the services deliberately do **not** measure is how much work is *waiting*.
`scan_tasks_served_total` counts tasks handed out, so from the worker's side a wedged pipeline and
an empty one are indistinguishable. The broker knows, and already exposes it: NATS runs with
`-m 8222` for its healthcheck, and `prometheus-nats-exporter` turns that into series. The ones this
stack uses, verified against a live JetStream instance:

| Metric | Type | Labels used | Means |
|---|---|---|---|
| `jetstream_consumer_num_pending` | gauge | `stream_name`, `consumer_name`, → `provider`, `scan` | messages waiting on a lane — **queue depth** |
| `jetstream_consumer_num_redelivered` | gauge | same | redelivered and not yet acked — retries in progress |
| `jetstream_consumer_num_ack_pending` | gauge | same | claimed but not yet acked |
| `jetstream_stream_total_messages` | gauge | `stream_name` | messages held on `TANKOVAULT_TASKS` / `TANKOVAULT_EVENTS` |
| `gnatsd_varz_slow_consumers` | counter | — | clients NATS disconnected for falling behind, **dropping their pending messages** |

`provider` and `scan` are not native. Worker consumers are named
`tankovault-workers-<mode>-<slug>` (`tankovault_contracts::subjects::worker_consumer`, with the
mode ahead of the slug precisely so the name can be taken apart again even though a slug may
contain `-`), and `prometheus.yml` re-derives the two labels from `consumer_name` so backlog and
throughput can be read on the same axes. **If that naming ever changes, the extraction fails
silently** — the series keep arriving, just without `provider`/`scan`, and the queue-depth panels go
blank. That is the symptom to recognise.

Four properties of the service-emitted data shape everything downstream and are worth stating once:

**There is no `service` label, except on `tankovault_build_info`.** No *measurement* carries the
emitting service's name. `TANKOVAULT_TELEMETRY__SERVICE_NAME` is a *log* field, and it reaches
the metric stream at exactly one place: the constant `1` of `tankovault_build_info`, which exists
to be joined against, not aggregated. `job` — assigned by Prometheus, one job per service —
remains the only thing that says where a series came from. That is why `prometheus.yml` declares
eight separate jobs rather than one job with a target list, and why collapsing them would
silently destroy every per-service rule.

**`solve_attempts_total` comes from two services.** The `/v1/solve` contract is defined once, in
`crates/solver/src/http.rs`, and both `challenge-solver` and `render` mount it — so every rule over
that metric keeps a `job` dimension.

**`result="unavailable"` is not `result="error"`.** They are the two halves of what used to be one
label, and they lead opposite ways: `error` is a challenge the back-end ran and could not beat,
which is the provider's answer; `unavailable` is the tier failing to run it at all — the browser
pool full, a replica restarting — which says nothing about the provider and clears on its own.
`solve_retries_total` counts the repeats `worker` makes for the second kind only; a sustained rate
there is a solver tier that is too small for its load, not a provider getting harder.

**`route` is the matched path, never the URI.** `/v1/series/{id}`, not `/v1/series/9f3c…`, so
cardinality is bounded by the route table. Anything that matched no route folds into a single
`unmatched` label, because an unrouted path is attacker-controlled and would otherwise be an
unbounded label source. On the `frontend` job, `route="unmatched"` is mostly the **static bundle**
(served by a fallback service, which carries no matched path) — not suspicious traffic.

**`status` is the exact code, not a class.** `"503"`, not `"5xx"`. Class splits happen in the
recording rules.

Health and readiness probe traffic appears in **none** of these: `ops_router` is merged outside
`HttpStack`, so `/health` and `/ready` never reach the metrics middleware. The request-rate panels
show real traffic rather than a floor of probes.

### Cancelled requests, and what the SSE stream does to the numbers

A request the client abandons is counted in `http_requests_in_flight` but **not** in
`http_requests_total` or `http_request_duration_seconds`. The latter two are recorded from the
response, and a client disconnect drops the middleware future before there is one.

This matters because `/v1/me/stream` is a long-lived SSE response whose normal end *is* the browser
closing it. So:

- Notification streams are largely **invisible** in the request counter and the latency histogram.
- They are fully **visible** in the in-flight gauge, for as long as the tab is open.
- When the *server* closes a stream instead (an `api` restart, NATS lost), a multi-minute duration
  does land in the histogram's `+Inf` bucket. The per-route latency rule excludes
  `route="/v1/me/stream"` for exactly this reason, and the `frontend` job is excluded from the
  latency alerts altogether because its only proxy route is `/v1/{*rest}` — the stream cannot be
  separated out there by label.

---

## 2. What is collected

The chart's `ServiceMonitor`, 15s scrape interval; retention is the cluster Prometheus's.

Eight per-service jobs on `<service>:9090`, plus Prometheus itself. Two deliberate asymmetries:

- **`worker` is discovered by DNS, everything else is a static target.** A static target that stops
  answering keeps its series and reports `up == 0`, which is alertable; a DNS-discovered target
  disappears, and an alert on an absent series never fires. So static is the better default — but
  `worker` runs `replicas: 2` behind one compose DNS name, and a static `worker:9090` would resolve
  to a different replica on different scrapes, interleaving two processes' counters into one
  `instance` series. Every replica switch would look like a counter reset and `rate()` would
  under-report. `dns_sd_configs` gives each replica its own `instance`. The cost is paid back by
  `TankoVaultWorkerTargetsAbsent`, which alerts on `absent()` instead of on `up == 0`.

- **Readiness is probed through `blackbox_exporter`, not scraped.** `GET /ready` is the only place
  the stack reports on its *dependencies*, and it answers with a JSON body — which Prometheus
  cannot parse as exposition, so scraping it directly would report a healthy service as `up == 0`.
  The prober turns the HTTP status into `probe_success{service=…}`. This is the only reason a
  Postgres outage, an exhausted connection pool or an unreachable NATS is alertable at all without
  adding a gauge to the Rust runtime.

`/ready` needs no credential even on the internal tier: `ops_router` is merged outside
`HttpStack`, by design, so an orchestrator never needs the secret. Under
`internal.identity = "mtls"` the certificate is a credential the prober also does not have, so
`/health` and `/ready` get a plaintext listener of their own on `internal.tls.probe_listen`
(`0.0.0.0:9091`) — point the blackbox prober and the pod's own probes there, not at the service
port, which answers a plain `GET` with a TLS alert. The scrape does **not** move with them: it
stays wherever `metrics.listen` puts it.

### Recording rules

`charts/tankovault/rules/tankovault-recording.rules.yml` in the chart repository, named
`level:metric:operations`. They precompute per-service request rate, 4xx/5xx/429 splits, the 5xx
ratio, p50/p95/p99 latency, per-route latency and error rate, rate-limit and feature-gate rates,
scan tasks per provider and per tier, solve/render outcome rates and error ratios, replica counts
and readiness.

Two things about them are load-bearing:

- **Quantiles are computed from buckets, aggregated across replicas first.**
  `crates/service/src/metrics.rs` registers `http_request_duration_seconds` with explicit bucket
  boundaries rather than the exporter's default summary behaviour precisely so this is possible:
  quantiles cannot be averaged, buckets can. Every quantile rule keeps its `sum by (…, le)` inside
  `histogram_quantile`. Do not "simplify" one to an `avg`.
- **Ratio rules are NaN while there is no traffic.** An idle service records a gap rather than a
  fabricated `0`, every comparison against NaN is false so an idle service cannot trip a threshold,
  and Grafana draws the gap honestly.

### Alerting, and the absence of an Alertmanager

There is no Alertmanager. Rules are evaluated and fire regardless — they are visible on Prometheus'
Alerts page and on the dashboard's *Firing alerts* panel. An Alertmanager shipped here would have no
receiver this repository can ground (no operator address, no chat webhook, no rotation), and a
notifier that delivers nowhere while looking configured is the same defect OPS-8.2 removed. The
stanza to add when there is somewhere to send is in `prometheus.yml`.

### Validating a change

The rules live in the chart, so they are validated there: `TimSchoenle/helm-charts` runs
`promtool` over `charts/tankovault/rules/` and asserts the rendered templates in
`charts/tankovault/tests/observability_test.yaml`. A rule change is a pull request against that
repository.

The unit tests exist because `check rules` only proves the PromQL parses, and both bugs found
while writing these rules parsed fine.

---

## 3. Alerts, and what to do when each fires

Thresholds and reasoning are in the comments beside each rule; this is the operator's index. The
`description` annotation on every alert says what to *do*, so the alert itself is the first step of
the runbook.

### Availability

| Alert | Fires when | First move |
|---|---|---|
| `TankoVaultRequestPathDown` | `api` or `frontend` unscrapable 3m | Past 3m the container's own restart cycle has already failed, so suspect a **boot refusal** before a crash. `docker compose logs --tail=50 <service>` and look for a refused secret: the internal-tier token, the JWT secret, the AniList encryption key, or a value on the placeholder deny list. Each logs the key it wanted. |
| `TankoVaultServiceDown` | any other backend unscrapable 5m | Reads still work. What is lost depends which: control-plane = no scans planned; notifier = no notifications; sync = AniList calls `502` through the api; challenge-solver/render = challenge-protected providers stop being scannable. Same log check. |
| `TankoVaultWorkerTargetsAbsent` | no worker replica discovered 5m | Scan tasks are queueing and nothing executes them. `docker compose ps worker`. The backlog is not lost — the work-queue stream drops a message on ack, not on consumer loss — but it drains as slowly as it filled. |
| `TankoVaultWorkerFleetIncomplete` | fewer than the 2 declared replicas, 10m | Throughput halved, nothing broken. If it is not a deploy, look for a replica in a restart loop and check the 768m memory limit for an OOM during a large catalogue fan-out. |
| `TankoVaultServiceNotReady` | `/ready` non-2xx for 5m | This is the dependency alarm. `curl -s http://<service>:<port>/ready \| jq` names the failing dependency — the body is per-dependency, so this is one command from a diagnosis. Postgres down = the postgres container. Postgres up but the check failing = **connection-pool exhaustion**; look for a slow query holding connections. NATS down = the scan pipeline and live notifications are both stalled. |
| `TankoVaultReadinessProberDown` | `blackbox-exporter` down 10m | Readiness is unmonitored and `TankoVaultServiceNotReady` **cannot fire** while this holds. Restart it; if it will not start, check its prober config in the chart parses. |
| `TankoVaultPrometheusRuleEvaluationFailing` | any rule group erroring 5m | A group is failing, so the series it produces are stale and alerts built on them are silently disarmed. Status → Rules names the group. This is the alert that keeps the others from failing quietly. |

### Request path

| Alert | Fires when | First move |
|---|---|---|
| `TankoVaultHighServerErrorRatio` | >5% 5xx for 10m, above 0.05 req/s | A server-side defect: every client-side outcome in this API is a 4xx. Find the route on the *5xx by route* panel, then pull logs by `x-request-id` — the logs are JSON and the header is on every response. A `/v1/me/sync/*` route usually means the failure is downstream in `sync`. |
| `TankoVaultServerErrorsCritical` | >25% 5xx for 5m | Treat as a tier outage, not a handler bug. Check `/ready` first, then whatever it names. A whole-tier 5xx rate on a service that reports ready points at something shared — the database, or a config value every handler reads. |
| `TankoVaultLatencyDegraded` | p99 over 2.5s for 15m (`api`, `control-plane`, `sync`) | Use the per-route p95 panel. All routes = a shared dependency, usually the database (a missing index after a migration, or a table that outgrew one). One route = that handler. On `sync`, a slow route is usually AniList itself, which is expected to degrade per-request and is why `sync` deliberately does not probe it in `/ready`. |
| `TankoVaultLatencyCritical` | p99 over 10s for 5m | Requests are heading for the 30s `request_timeout_secs`, after which clients get `408` and the work is wasted. Check `/ready` and the database. If the database is healthy, look at in-flight concurrency for the same job: climbing concurrency with flat throughput is queueing, which here means the pool. |

Both latency thresholds are **histogram bucket boundaries** from `LATENCY_BUCKETS`
(2.5s and 10s, the top finite bucket). `histogram_quantile` interpolates within a bucket, so a
threshold between boundaries would alert on an interpolated number rather than an observed one.

### Edge policy

| Alert | Fires when | First move |
|---|---|---|
| `TankoVaultRateLimiterFailingOpen` | any `rate_limit_store_errors_total` in 10m | **Rate limiting is not being enforced while this fires.** The Redis backend fails open on purpose so a counter-store outage cannot take the edge down — which means login attempts are currently unlimited. Restore Redis. If it will be down a while, either accept the exposure knowingly or set `TANKOVAULT_RATE_LIMIT__BACKEND=memory` for per-replica limiting (effective limit becomes N× the configured one, but finite). |
| `TankoVaultAuthRateLimitSurge` | `auth`-class denials over 0.1/s for 15m | Probably credential stuffing; the limiter is working, so this is not an outage. Rule it out as self-inflicted first — a client looping on a failed refresh looks identical. `auth.login`/`auth.refresh` audit entries tell you whether the attempts name one account (client bug) or many (attack). |
| `TankoVaultDisabledFeatureStillCalled` | a disabled feature still called after 1h | These are answered `404 feature_disabled`, so users see an empty surface rather than an error. Other replicas converge within `features.refresh_secs` (15s) and the SPA re-reads `/v1/me/capabilities`, so an hour later means a client is not honouring the capability list — a frontend fix, not a flag change. |

### Scan pipeline

| Alert | Fires when | First move |
|---|---|---|
| `TankoVaultFastScanLanesStarved` | full tasks flowing, no fast task for 15m, sustained 30m | New chapters are not being picked up although the worker is consuming. Fast scans have **strict** priority and a fast run enqueues exactly one task per provider, so a fast task can never be stuck behind a full backlog — the lanes are the suspect. Grep the worker log for `could not open provider task lane`. If the lanes are open, check the control-plane is still planning fast runs: a scheduler wedged without dying would not have tripped `TankoVaultServiceDown`. |
| `TankoVaultFastScanBacklogImpossible` | fast-tier backlog exceeds the number of fast lanes, 15m | Fast tasks are being enqueued and not consumed. Check the worker is consuming at all, then the lanes. **If a fast scan has recently been changed to fan out, this alert's premise is gone** — §7 of `OPERATIONS.md` warns about exactly that change, and the rule would then need a real threshold rather than a derived one. |
| `TankoVaultScanTaskRetryStorm` | a lane holds redelivered tasks for 30m | Tasks keep failing and burning their delivery budget before being recorded as failed. Usual causes: the provider changed its markup so the adapter's selectors no longer match; the provider is rate-limiting or challenging the crawler; or the challenge solver is broken. Check `TankoVaultChallengeSolvingFailing` first — one broken solver produces this on every challenge-protected provider at once. |
| `TankoVaultNotificationBacklogNotDraining` | events backlog non-zero and not shrinking, 30m | The notifier acks only after fan-out, so one hanging channel stalls everything behind it — an SMTP relay that accepts the connection and never answers is the classic case and it logs no error. Check the last event the notifier handled, then the configured channels. Switching the suspect channel off by feature flag both confirms it and unblocks the queue. |
| `TankoVaultNatsSlowConsumers` | NATS dropped a client's pending messages, in 15m | Live notifications are published over **core** NATS, fire-and-forget — a drop means a signed-in user never received a push. The durable `notifications` row survives, so the bell fills in on the next page load; only the push is lost. The client that fell behind is almost always `api` relaying to SSE subscribers. |
| `TankoVaultNatsExporterDown` | `nats` job down 10m | Every backlog alert above is **disarmed** while this holds — their series went stale, not to zero. Restart the exporter; if it will not start, confirm NATS still has `-m 8222` in its compose command. |
| `TankoVaultScanLaneUnfair` | one provider takes >90% of served tasks for 1h | Either the other lanes are empty (fine, and normal right after a provider is added) or they failed to open (their scans will never run). Compare the per-provider panel against the console's provider list: queued runs with a flat counter is a lane problem. Remember a **redelivered task counts again**, so one failing provider retrying can produce the same shape — check its recent run states first. |

Two of these have unusually good grounding and are worth calling out as the pattern to copy:

- `TankoVaultFastScanLanesStarved` is **self-gating**, so it is safe on an idle deployment: it
  requires full tasks to be flowing — proof the worker is alive and consuming — before it will
  consider the fast lane's silence meaningful. That is the diagnostic in
  [`OPERATIONS.md` §7](./OPERATIONS.md) turned into a rule.
- `TankoVaultFastScanBacklogImpossible` has **no invented number at all**. §7 records that a fast run
  enqueues exactly one task per provider, so the fast lanes hold at most one task each — which is
  also what makes strict priority safe. `count()` over the fast lanes *is* the provider count, so the
  threshold is "backlog exceeds lane count", and it rescales itself as providers are added.

Both depend on that invariant. §7 already warns that changing a fast scan to fan out would break the
priority reasoning; it breaks these two alerts at the same moment, and the second one's annotation
says so.

### Fetch tier

| Alert | Fires when | First move |
|---|---|---|
| `TankoVaultChallengeSolvingFailing` | >50% solve errors for 15m | Every challenge-protected provider stops being scannable, and its tasks fail rather than queue. On `challenge-solver`, check TRAWL — `curl -sf trawl:8191/health` reports live browser-pool capacity, and a saturated pool answers `429`, which reads as a solve failure here. On `render`, this is Chromium: OOM against the 2g limit, or a shrunken `/dev/shm` (`shm_size: 1gb`). |
| `TankoVaultRenderTierFailing` | >50% render errors for 15m | Providers needing a real browser stop yielding chapters; others are unaffected. Same checks. `render` is deliberately not `read_only`, so a full disk is also a candidate — it writes a browser profile and cache. |
| `TankoVaultRefusedFetchTargets` | any SSRF refusal in 15m | The control working, and worth knowing about. Both endpoints return the fetched DOM *and the cookies collected*, so a successful request to a private address would be a full internal-network read. The log line (`refused a solve target` / `refused a render target`) names the URL and the rule. A provider URL means a bad row or a redirect into a private range. Anything else means something is reaching these endpoints that should not: verify `internal.peers` on the service names only `worker`, and that no `ports:` entry publishes them. |

### Thresholds that need the operator's first week

Two, both labelled `grounding: baseline-needed` in the rule file:

- **`TankoVaultConcurrencyUnusuallyHigh` (200 in flight).** There is no configured concurrency limit
  anywhere in this stack to derive a number from, and on `api`/`frontend` every open SSE stream is
  legitimately one request in flight — so this gauge partly counts *connected browsers*, and its
  normal value scales with the user count rather than with load. Replace 200 with a multiple of the
  observed steady state. Severity is `info` and the annotation says to read the graph, not the
  alert. It is deliberately not a page: a threshold nobody can justify should not wake anyone.
- **`TankoVaultScanLaneUnfair` (90% share, ≥3 providers, >100 tasks/h).** A two-provider deployment
  where one provider is genuinely much larger would trip this legitimately. Re-tune against the real
  provider mix, or raise the provider-count floor.

---

## 4. What is not measured

The honest half of this document. These are real, documented failure modes of this system that
have **no metric**, so there is no rule for them and no panel — rather than an approximation that
would read as coverage.

Six gaps were listed here when the collection layer shipped, deliberately unclosed at the time:
instrumentation is a change to running services, and mixing it with the rules work would have
meant shipping alerts over metrics nobody had watched in a real process. Five of the six are now
closed by call sites (`provider_backoff_waits_total`, `notifications_delivered_total`,
`scan_tasks_settled_total`, `scan_catalog_pages_truncated_total`, `db_pool_connections`), and the
sixth — native readiness — is now `service_ready` / `service_dependency_up`, though
`blackbox_exporter` remains the availability signal because a gauge inside a process cannot
report that the process is gone.

What is left:

| Failure mode | Currently visible as | What would make it alertable |
|---|---|---|
| **Per-provider crawl budget exhaustion** | `provider_fetch_duration_seconds` widens and `provider_backoff_waits_total` climbs, so throttling is now visible — but the crawler's *own* rate limiter, the budget it chose before the provider ever pushed back, is not. A provider configured too conservatively looks identical to a fast one that is idle. | A counter at the `governor` wait in `crates/fetch/src/ratelimit.rs`, labelled by provider, plus the wait as a histogram. |
| **Per-user sync outcomes** | `anilist_requests_total` covers the transport, not the reconciliation above it: a user whose entries fail to map produces successful API calls and no synced progress. | An outcome counter at the reconcile arms in `services/sync/src/engine/reconcile.rs`. Cardinality has to stay per-*outcome*, never per-user. |
| **Email specifically, as against channels generally** | `notifications_delivered_total{channel="email"}` now separates a fast-failing relay from an idle one. What it cannot see is a relay that accepts the message and drops it — delivery, as against handoff. | Nothing in this process can. It needs the relay's own bounce reporting. |

The three metrics that were added and are *not* alerted on yet are deliberate:
`chapters_discovered_total`, `scan_task_duration_seconds` and `db_pool_connections` need a
baseline from a real deployment before a threshold on them is anything but invented — the same
reason `TankoVaultConcurrencyUnusuallyHigh` carries `grounding: baseline-needed`. They are on the
dashboard so the baseline accumulates; the rules come after.

### One defect this exercise did find and fix

`http_requests_in_flight` leaked. The decrement sat on the line after `next.run(req).await`, and a
client disconnect drops that future mid-await, so the decrement never ran. Since `/v1/me/stream` is
an SSE response whose normal end *is* the browser closing it, the gauge climbed with every
notification stream ever opened and reported hundreds of concurrent requests on an idle `api`.

It is now released by a `Drop` guard, which runs on both paths, with a regression test in
`crates/service/src/metrics.rs` whose doc comment records the bug. Worth noting for what it says
about the method: this was found by asking what each metric would look like on a dashboard, not by
reading the code for bugs.

---

## 5. Files

All of it is in [`TimSchoenle/helm-charts`](https://github.com/TimSchoenle/helm-charts), under
`charts/tankovault/`:

| Path | What it is |
|---|---|
| `rules/tankovault-recording.rules.yml` | 31 recording rules. |
| `rules/tankovault-alerts.rules.yml` | 25 alerts, each with a stated threshold rationale. |
| `templates/prometheusrule.yaml` | Wraps both rule files as a `PrometheusRule`. |
| `templates/servicemonitor.yaml` | The scrape config: per-service targets on `9090`. |
| `templates/grafanadashboard.yaml` | Provisions the dashboard below. |
| `dashboards/tankovault-overview.json` | The dashboard: fleet, request path, edge policy, scan pipeline, fetch tier. |
| `tests/observability_test.yaml` | The gate over the rendered output. |

---

## 6. The desktop client

Everything above is the fleet. The native client is scraped by nothing — it runs on a reader's
machine, behind their firewall, and the only telemetry that reaches a maintainer is what the
reader chooses to send. So it writes its own, on disk, and the settings sheet's About tab has a
button that opens the folder.

| Platform | Where |
|---|---|
| Windows | `%LOCALAPPDATA%\TankoVault\data\logs` |
| Linux | `$XDG_DATA_HOME/TankoVault/logs`, else `~/.local/share/TankoVault/logs` |

The *local* data directory, not the config directory beside `settings.json`: on a domain-joined
Windows machine the latter is the roaming profile, copied over the network at every sign-in.

| File | What it is |
|---|---|
| `tankovault.log` | The live log. Rolls at 2 MiB, keeping `.1`, `.2`, `.3`. |
| `crash-<date>-<pid>.log` | One per panic — version, commit, platform, thread, source location, backtrace. Ten kept. |
| `session.running` | Present only while the app runs. |

`session.running` is the part that covers a Windows crash. A Rust panic runs a hook and writes a
report; an access violation, a stack overflow or a `WebView2` host dying under the renderer runs
**nothing** — there is no unwinding, no hook, and the frontend crate forbids the `unsafe` a
structured-exception handler would need. So the marker is written at start and removed when the
event loop is destroyed, and one found at the *next* start is logged as a kill. A report that says
"the previous session ended without a clean shutdown and without a panic" means: stop looking for
a panic.

Frames in a crash report may be bare addresses — the shipped profile carries no debug info and
strips symbols. Read the `location` line first; it comes from `Location` and is compiled in
regardless.

Default level is `info`. `TANKOVAULT_LOG` raises or narrows it in `RUST_LOG` syntax
(`TANKOVAULT_LOG=debug,wry=trace`), and it is deliberately not `RUST_LOG` — turning this app up
should not turn up every other Rust program in the same shell. Libraries log into the same
subscriber, so `wry`, `tao` and `reqwest` are in the file too.

The code is `web/frontend/src/diagnostics/`. The web build has none of this and needs none: a
browser tab that dies leaves a console entry and a devtools stack behind.
