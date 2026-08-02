# Observability Reference

What this deployment measures, what is collected, which alerts exist, and what to do when each
one fires.

Every service installs the shared Prometheus recorder from `crates/service` and serves the
exposition on its own listener (`TANKOVAULT_METRICS__LISTEN`, default `0.0.0.0:9090`) so a scrape
never touches the request-facing port. Until now nothing collected it. The collection side lives in
[`deploy/observability/`](../deploy/observability/) and is started by an overlay:

```bash
docker compose -f deploy/docker-compose.yml -f deploy/docker-compose.observability.yml \
  --env-file deploy/local.env up -d
```

Grafana on <http://127.0.0.1:3001> (folder *TankoVault*), Prometheus on <http://127.0.0.1:9091>.
Both are bound to loopback; the reasoning, and why this is an overlay rather than a compose
profile, is at the top of `deploy/docker-compose.observability.yml`. A plain
`docker compose -f deploy/docker-compose.yml up` is unchanged and starts nothing extra.

`GRAFANA_ADMIN_PASSWORD` has no default and the overlay refuses to start without it, for the same
reason the API refuses a placeholder JWT secret: Grafana's `admin`/`admin` is published in
Grafana's own documentation, so it is not a password.

---

## 1. What is measured

This is the **complete** inventory — ten metrics, hand-verified against every `counter!`,
`histogram!` and `gauge!` call site in the workspace. Nothing in the dashboards or rules queries
anything outside this table, because a panel or an alert referencing a metric nobody emits is an
operator believing they have coverage they do not have.

| Metric | Type | Labels | Emitted by | Present on |
|---|---|---|---|---|
| `http_requests_total` | counter | `method`, `route`, `status` | `crates/service/src/metrics.rs` | every service |
| `http_request_duration_seconds` | histogram | `method`, `route` | `crates/service/src/metrics.rs` | every service |
| `http_requests_in_flight` | gauge | — | `crates/service/src/metrics.rs` | every service |
| `http_rate_limited_total` | counter | `class` (`global`/`auth`/`expensive`) | `crates/service/src/ratelimit/mod.rs` | services with a limiter |
| `rate_limit_store_errors_total` | counter | `backend` (`redis`) | `crates/service/src/ratelimit/redis.rs` | services on the Redis backend |
| `http_feature_disabled_total` | counter | `feature` | `crates/service/src/flags.rs` | `api`, `control-plane`, `sync`, `notifier` |
| `scan_tasks_served_total` | counter | `provider`, `scan` (`full`/`fast`) | `services/worker/src/queue.rs` | `worker` |
| `chapters_rejected_total` | counter | `provider` | `services/worker/src/engine.rs` | `worker` |
| `solve_attempts_total` | counter | `result` (`ok`/`error`/`rejected`) | `crates/solver/src/http.rs` | `challenge-solver` **and** `render` |
| `render_requests_total` | counter | `result` (`ok`/`error`/`rejected`) | `services/render/src/main.rs` | `render` |

`chapters_rejected_total` counts listing entries a scan refused to index because the source
cannot plausibly have released them (see `chapter_outliers` in [`CONFIGURATION.md`](CONFIGURATION.md)).
A steady trickle is normal — it is what the rule exists for. A **step change on one provider** is
the signal worth acting on: either that site started publishing junk slugs, or an adapter change
altered how its numbers parse and the rule is now discarding real chapters. The `warn` log at the
same site carries the numbers, which is what tells the two apart.

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

**There is no `service` label.** No metric carries the emitting service's name.
`TANKOVAULT_TELEMETRY__SERVICE_NAME` is a *log* field, not a metric label. `job` — assigned by
Prometheus, one job per service — is the only thing that says where a series came from. That is
why `prometheus.yml` declares eight separate jobs rather than one job with a target list, and why
collapsing them would silently destroy every per-service rule.

**`solve_attempts_total` comes from two services.** The `/v1/solve` contract is defined once, in
`crates/solver/src/http.rs`, and both `challenge-solver` and `render` mount it — so every rule over
that metric keeps a `job` dimension.

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

`deploy/observability/prometheus/prometheus.yml`, 15s scrape interval, 15 days retention.

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

`/ready` needs no `X-Internal-Token` even on the internal tier: `ops_router` is merged outside
`HttpStack`, by design, so an orchestrator never needs the secret.

### Recording rules

`deploy/observability/prometheus/rules/tankovault-recording.rules.yml`, named
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

`promtool` ships in the pinned Prometheus image, so no local install is needed:

```bash
# Config, rule-file discovery and every expression's syntax.
docker run --rm --entrypoint /bin/promtool \
  -v "$PWD/deploy/observability/prometheus:/etc/prometheus:ro" prom/prometheus:v3.5.0 \
  check config /etc/prometheus/prometheus.yml

# Behaviour: the low-traffic guard and the fast-lane `unless` actually work.
docker run --rm --entrypoint /bin/promtool \
  -v "$PWD/deploy/observability/prometheus:/etc/prometheus:ro" prom/prometheus:v3.5.0 \
  test rules /etc/prometheus/rules/tests/alerts.test.yml
```

The unit tests exist because `check rules` only proves the PromQL parses, and both bugs found while
writing these rules parsed fine — see the header of `rules/tests/alerts.test.yml`. Prometheus is
started with `--web.enable-lifecycle`, so an edited rule file can be applied without losing the
TSDB: `curl -X POST http://127.0.0.1:9091/-/reload`.

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
| `TankoVaultReadinessProberDown` | `blackbox-exporter` down 10m | Readiness is unmonitored and `TankoVaultServiceNotReady` **cannot fire** while this holds. Restart it; if it will not start, check `deploy/observability/blackbox/blackbox.yml` parses. |
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
| `TankoVaultChallengeSolvingFailing` | >50% solve errors for 15m | Every challenge-protected provider stops being scannable, and its tasks fail rather than queue. On `challenge-solver`, check FlareSolverr — it is a third-party image with no healthcheck, so it can be running and useless. On `render`, this is Chromium: OOM against the 2g limit, or a shrunken `/dev/shm` (`shm_size: 1gb`). |
| `TankoVaultRenderTierFailing` | >50% render errors for 15m | Providers needing a real browser stop yielding chapters; others are unaffected. Same checks. `render` is deliberately not `read_only`, so a full disk is also a candidate — it writes a browser profile and cache. |
| `TankoVaultRefusedFetchTargets` | any SSRF refusal in 15m | The control working, and worth knowing about. Both endpoints return the fetched DOM *and the cookies collected*, so a successful request to a private address would be a full internal-network read. The log line (`refused a solve target` / `refused a render target`) names the URL and the rule. A provider URL means a bad row or a redirect into a private range. Anything else means something is reaching these endpoints that should not: verify `TANKOVAULT_INTERNAL__TOKEN` on the service and its callers, and that no `ports:` entry publishes them. |

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
would read as coverage. Each names the smallest emission that would fix it, so the work is a
call site rather than a design.

| Failure mode | Currently visible as | What would make it alertable |
|---|---|---|
| **Provider rate-limit backoff and 429 penalties** (`crates/fetch/src/backoff.rs`) | log lines only. A provider that has throttled the crawler into hour-long waits is invisible; scans simply take longer. | `provider_backoff_waits_total{provider,status}` and a histogram of the wait, at the one `tracing::warn!` site in `backoff.rs`. This is the largest gap of the four. |
| **Email delivery failure** (ARCH-13) | **partly.** A relay that *hangs* is caught: the notifier acks only after fan-out, so the events backlog stops draining and `TankoVaultNotificationBacklogNotDraining` fires. A relay that **fails fast** is not — the notifier logs a `tracing::warn!` per channel, acks, and moves on, so delivery silently stops with a draining queue and no metric. | `notifications_delivered_total{channel,result}` at the point each channel returns. Also makes "up but delivering nothing" distinguishable from "nothing to deliver". |
| **JetStream retry versus settle** (ARCH-14) | **partly**, and from the broker rather than the worker. `jetstream_consumer_num_redelivered` shows retries in progress per lane (`TankoVaultScanTaskRetryStorm`), which is the failure mode an operator feels. What is still missing is the *settle* half: how many tasks exhausted `MAX_TASK_DELIVERIES` and were recorded as permanently failed. `scan_tasks_served_total` counts tasks handed *out* and a redelivery increments it again, so throughput and retry remain conflated in that counter. | `scan_tasks_settled_total{provider,scan,outcome}` — acked / requeued / gave-up — at the three arms in `services/worker/src/main.rs`. The permanent-failure rate falls out of it, and the conflation disappears. |
| **Scan fan-out truncation** | nothing. A catalogue larger than `max_catalog_pages` is silently truncated — this has already happened in practice on a large provider. | `scan_catalog_pages_truncated_total{provider}` at the point the page budget is hit. One counter, and "we have been quietly missing most of a provider's catalogue" stops being undetectable. |
| **Database connection-pool saturation** | `/ready` flipping to `503`, i.e. `TankoVaultServiceNotReady` — but only once the pool is exhausted *enough* to fail a health check. Queueing short of that shows only as latency. | `sqlx::Pool` exposes `size()` and `num_idle()`; a periodic gauge pair `db_pool_connections{state}` would turn a symptom into a leading indicator. |
| **Per-service readiness, natively** | only via `blackbox_exporter`, which is why it is in the overlay. | A `service_ready` gauge updated by the same `Health::report` the endpoint calls would remove a container from the stack and make readiness a first-class series. |

No metric was added to any service for this. Instrumentation is a change to running services and
this work is the collection layer; mixing the two would have meant shipping rules over metrics that
had never been observed in a real process. Where a gap could be closed **without** touching Rust it
was — `blackbox_exporter` for readiness, `prometheus-nats-exporter` for queue depth and retries,
both verified against live instances before a rule was written. What remains genuinely needs a call
site, and is worth its own tracker row.

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

| Path | What it is |
|---|---|
| `deploy/docker-compose.observability.yml` | The overlay: `prometheus`, `grafana`, `blackbox-exporter`, `nats-exporter`. Pinned tags (digests recorded in comments), memory limits, `cap_drop: [ALL]`, `no-new-privileges`, `read_only` on all but Grafana, healthchecks, loopback-only published ports. |
| `deploy/observability/prometheus/prometheus.yml` | Scrape config: eight per-service jobs, DNS discovery for `worker`, blackbox readiness probes, JetStream queue depth with the `provider`/`scan` relabelling. |
| `deploy/observability/prometheus/rules/tankovault-recording.rules.yml` | 31 recording rules. |
| `deploy/observability/prometheus/rules/tankovault-alerts.rules.yml` | 25 alerts, each with a stated threshold rationale. |
| `deploy/observability/prometheus/rules/tests/alerts.test.yml` | `promtool test rules` cases for the two expressions whose label semantics are easy to get wrong. |
| `deploy/observability/blackbox/blackbox.yml` | One prober module for `GET /ready`, with a 3s timeout chosen against the 2s per-dependency check bound. |
| `deploy/observability/grafana/provisioning/` | Datasource (fixed uid, referenced by the dashboard) and dashboard provider. |
| `deploy/observability/grafana/dashboards/tankovault-overview.json` | The dashboard: fleet, request path, edge policy, scan pipeline, fetch tier. |
