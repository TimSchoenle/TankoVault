# Deployment

Container build and local orchestration for TankoVault (`docs/ARCHITECTURE.md` §19).

## Layout
- `docker/Dockerfile`: **every image, from one file**: a cargo-chef-cached multi-stage build
  that compiles all nine workspace binaries in a single `cargo` invocation and then hands each
  one to a thin runtime stage. Pick a backend service with `--build-arg BIN=<name>` (`api`,
  `worker`, `control-plane`, `notifier`, `sync`, `challenge-solver`, `render`, `bootstrap`).
  `xtask` builds too, but is **never published**: the deploy blacklist
  (`[workspace.metadata.deploy.exclude]` in the root `Cargo.toml`, enforced by `xtask
  repo-lint`) holds it back, because it is the repository's task runner and carries `reset`.
  The install steps a deployment actually needs are the `bootstrap` image
  ([below](#installing-into-a-cluster)). Each binary is compiled natively on
  Alpine as a **musl binary** and shipped on a bare `scratch` image (no OS, no shell, no
  package manager: just the binary, the musl loader and the `libgcc_s` it resolves, a CA trust
  store, and a numeric nonroot user). It is dynamically, not statically, linked because `wreq`'s
  BoringSSL build script `dlopen`s libclang, which a
  `crt-static` build script cannot do; see the Dockerfile's builder stage. Two tiers need
  something else and select it with `--target`:
  - `runtime-browser`: the `render` tier, which drives a real Chromium and so needs a Debian
    base, not `scratch`.
  - `frontend`: the Dioxus WASM SPA (`web/frontend/`, built with the `dx` CLI) shipped
    alongside the `frontend` axum server (`services/frontend/`) on `scratch`. The server serves
    the SPA and reverse-proxies `/v1/*` (REST and SSE) to the `api` service, so the SPA's
    same-origin API calls resolve without CORS. It shares the Alpine builder with every other
    binary, so the dependency graph is compiled once.
- `docker-compose.yml`: the full end-to-end local stack: Postgres 18 (pgvector), Redis 8, NATS
  (JetStream), TRAWL, the one-shot `migrate`/`seed`/`seed-providers` steps (the
  `bootstrap` image, so the local stack exercises the artefact a cluster runs), every backend
  service, and the web frontend. It is the local and single-host shape; a cluster deploys
  through the chart described under [Kubernetes](#kubernetes) below.

## Quick start
```bash
# From the repo root (compose build context is the repo root).
docker compose -f deploy/docker-compose.yml up --build
```
This applies migrations, seeds a demo admin (`admin` / `changeme12345`) and a placeholder
Madara provider, then starts all services and the frontend.

**Open the app at http://localhost:3000**: the `frontend` server serves the SPA and proxies
`/v1/*` to the API, so this single origin is all a browser needs.

**Only port 3000 is published on the host.** Every other service listens on the compose network
only; see the exposure note at the top of `docker-compose.yml` for why. In-network ports:
api `8080`, control-plane `8081`, notifier `8082`, sync `8083`, render `8084`, worker (ops)
`8085`, challenge-solver `8090`, TRAWL `8191`, and every backend's Prometheus scrape on
`9090`.

### Why the frontend sits behind a proxy
The WASM client calls the API same-origin (`web/frontend/src/api.rs` → `API_BASE = ""`) and
opens the live-notification SSE stream (`/v1/me/stream`) via the browser `EventSource` API.
Serving the SPA and proxying `/v1/*` from one origin makes those calls resolve with no
cross-origin hop, and the proxy streams responses unbuffered so SSE frames flush immediately.

## Configuration
Every service reads layered config via `tankovault-config`: optional TOML at `$TANKOVAULT_CONFIG`
(a file, or a directory of `*.toml` fragments), then `TANKOVAULT_*` environment variables (`__`
denotes nesting, e.g. `TANKOVAULT_DATABASE__URL`), then file-backed values:
`$TANKOVAULT_SECRETS_DIR` and `TANKOVAULT_<KEY>_FILE`. The compose file sets these inline.

The file layers are what the Kubernetes charts use: a `Secret` mounted as a volume keeps
credentials out of the pod's environment, and a rotated file makes the service rebuild itself
instead of requiring a restart. A key set by two of the last three layers fails the boot
instead of being resolved by precedence: see
[`docs/CONFIGURATION.md` §7](../docs/CONFIGURATION.md#7-secrets).

The complete surface (every `TANKOVAULT_*` key, its default, and which service reads it) is
[`docs/CONFIGURATION.md`](../docs/CONFIGURATION.md). The short version:

**Required, with no working default** (compose fails fast instead of booting insecure):
- `TANKOVAULT_AUTH__JWT_SECRET`: API token signing secret.
- `TANKOVAULT_ANILIST__CLIENT_ID` / `__CLIENT_SECRET` / `__REDIRECT_URI`: AniList OAuth app.
- `TANKOVAULT_ANILIST__TOKEN_ENCRYPTION_KEY`: base64 32-byte key for tokens at rest; generate
  with `openssl rand -base64 32`.

**Password pepper** (`TANKOVAULT_AUTH__PASSWORD_PEPPER`): a server-side secret mixed into every
argon2id hash so a database leak alone cannot be brute-forced offline. Empty by default, which
keeps hashing un-peppered; `bootstrap seed-admin` refuses an empty value when
`TANKOVAULT_PROFILE=production`. Once set it must stay stable (or existing passwords stop
verifying), and the `api` and `seed` services must have the same value.

## Building a single service image
```bash
# A backend service (musl -> scratch):
docker build -f deploy/docker/Dockerfile --build-arg BIN=api -t tankovault-api .
# The render tier (Debian + Chromium):
docker build -f deploy/docker/Dockerfile --build-arg BIN=render --target runtime-browser -t tankovault-render .
# The frontend (WASM bundle + axum server -> scratch):
docker build -f deploy/docker/Dockerfile --target frontend -t tankovault-frontend .
```
Note that `BIN` selects which *already-compiled* binary the runtime layer copies in, not what
gets compiled: the `builder` stage builds all nine at once, so the second and subsequent images
in a session are a `COPY` onto `scratch` and cost seconds.

What decides "all nine" is `ARG SERVICE_BINS`, whose default is the full list. A release
overrides it with the images that release actually publishes (`xtask release-plan`, see
[docs/RELEASING.md](../docs/RELEASING.md)), which drops a thin-LTO link per image it is not
building. `BIN` must be one of the names in `SERVICE_BINS`, or the final `COPY` has nothing to
take.

### Reproducible & cached builds
All base images are pinned by digest and cargo resolves against the committed `Cargo.lock`
(`--locked`):
```bash
docker build -f deploy/docker/Dockerfile \
  --build-arg BIN=api \
  -t tankovault-api .
```
Image layer timestamps are clamped from `SOURCE_DATE_EPOCH`, which `rewrite-timestamp=true`
applies to the files inside the image:
```bash
SOURCE_DATE_EPOCH=0 docker buildx build \
  -f deploy/docker/Dockerfile --build-arg BIN=api \
  --output type=image,name=tankovault-api,rewrite-timestamp=true .
```
**Use `0`, not a per-commit value.** buildx propagates this variable into the build as
`build-arg:SOURCE_DATE_EPOCH` (setting it in the environment and passing `--build-arg` are the
same thing on the wire) so its value is part of every stage's cache key. Derived from the commit
timestamp it made every `cache-from` in CI a total miss, 24 to 45 minutes of recompilation a run.
Both workflows pin it to `0` at workflow level; the Dockerfile's header comment carries the rule
and `cargo run -p xtask -- repo-lint` enforces it.
A consequence worth knowing, because it reached production once: every file in the image is
therefore dated `Thu, 01 Jan 1970`, so an mtime-derived `Last-Modified` or `ETag` is **the same
on every build** and cannot tell two builds apart. The frontend serves the app shell from memory
with no validator and `Cache-Control: no-store` for exactly this reason: see
[Running behind Cloudflare](../docs/CONFIGURATION.md#running-behind-cloudflare). Anything else put in front of a
TankoVault image that revalidates by timestamp inherits the same trap.

The dependency graph is compiled once by cargo-chef (reused across every service image and
exported by CI via the GHA layer cache); BuildKit `type=cache` mounts additionally keep the
crate registry warm across local rebuilds. Refresh a pinned digest with
`docker buildx imagetools inspect <image:tag>`.

## Running migrations only
```bash
docker compose -f deploy/docker-compose.yml run --rm migrate
```

### Upgrading past migration 0027: pgvector is required

Migration `0027_recsys_signals` runs `CREATE EXTENSION vector`. From that release on, **the
database must have [pgvector](https://github.com/pgvector/pgvector) available**; the migration
fails loudly instead of degrading, because a recommender that silently returns nothing is worse
than one that refuses to start.

- **Using the compose stack:** nothing to do. The `postgres` service moved from
  `postgres:18-alpine` to `pgvector/pgvector:pg18`: the same upstream Postgres major with the
  extension preinstalled, same entrypoint and environment contract, so the existing `pgdata`
  volume is reused in place. Pull the new image and bring the stack up.
- **Running your own Postgres:** install the extension package for your platform *before*
  applying migrations (`apt install postgresql-18-pgvector`, `CREATE EXTENSION vector`, or your
  managed provider's equivalent: RDS, Cloud SQL and Azure Flexible Server all ship it behind an
  allowlist setting). The migration only needs it to be installable; it issues the
  `CREATE EXTENSION` itself.

Rolling back drops the recommender's tables but deliberately leaves the extension in place:
dropping it would cascade into every column typed by it, which is a worse outcome than an unused
extension.

## Service wiring notes
- **Redis** backs the control-plane's singleton-scheduler leader election
  (`TANKOVAULT_REDIS__URL`). It fails open to sole-leader if Redis is absent, but the compose
  stack wires it so the behaviour matches a multi-replica deployment.
- **NATS** exposes its HTTP monitoring port (`-m 8222`) purely so compose can healthcheck it
  (`/healthz`); backend services wait on `nats: service_healthy` before starting.
- Every **Rust service** container carries a healthcheck, including the `scratch` ones. They
  have no shell, no `wget` and no `curl`, so the binary probes *itself*: `--healthcheck` is an
  argv branch handled before config loading (`crates/service/src/healthcheck.rs`) that TCP-
  connects to the service's own `bind_addr` and exits 0/1. That is what lets `depends_on` say
  `service_healthy`, not `service_started`: a frontend started as soon as the API *process*
  exists would race it on every `compose up`.
- The scratch containers additionally run `read_only: true`, `cap_drop: [ALL]` and
  `no-new-privileges`. `read_only` is safe by construction there: the image *is* the binary,
  the musl loader, `libgcc_s` and a CA bundle, so there is no writable path to depend on.
  `render` gets the capability drop but not `read_only`: it is a Debian base under a Chromium
  that writes a profile and a cache to paths not enumerable from here.

## Observability

Every service serves a Prometheus scrape on an isolated `9090`; the metric inventory and the
runbook for every alert are in [`docs/OBSERVABILITY.md`](../docs/OBSERVABILITY.md).

There is no compose overlay for collection. The scrape config, recording and alerting rules and
the provisioned dashboard live in the chart that deploys them,
[`TimSchoenle/helm-charts`](https://github.com/TimSchoenle/helm-charts) (`charts/tankovault`),
where they are gated by that repository's own tests. A second copy here would be two sets of
rules to edit and only one of them deployed.

### Slow queries: `pg_stat_statements` and `auto_explain`

The compose `postgres` service preloads both, with `track_io_timing` on. The API's own slow log
(sqlx, 1 s threshold) says *which* statement was slow; these say *why*. The library is loaded at
start-up, but the `pg_stat_statements` view needs its extension created once per database:

```bash
docker compose -f deploy/docker-compose.yml exec -T postgres \
  psql -U tankovault -d tankovault -c 'CREATE EXTENSION IF NOT EXISTS pg_stat_statements'
```

**Where the time goes, aggregated.** Worst total cost first; `read_ms` against `total_s` is the
share spent waiting on disk, and a high `mean_ms` with a low `stddev_ms` is a plan problem, not cache pressure:

```sql
SELECT calls, round(total_exec_time::numeric / 1000, 1) AS total_s,
       round(mean_exec_time::numeric, 1) AS mean_ms, round(stddev_exec_time::numeric, 1) AS stddev_ms,
       shared_blks_hit, shared_blks_read, round(shared_blk_read_time::numeric, 1) AS read_ms,
       temp_blks_written, left(regexp_replace(query, '\s+', ' ', 'g'), 160) AS query
  FROM pg_stat_statements ORDER BY total_exec_time DESC LIMIT 25;
```

`SELECT pg_stat_statements_reset();` starts a fresh window: do it before measuring a change.

**One slow execution, explained.** Any statement running longer than 2 s is logged with its
plan, actual row counts and buffer usage:

```bash
docker compose -f deploy/docker-compose.yml logs postgres | grep -A40 'duration:'
```

Read three things off each plan. `Buffers: shared read=` large relative to `hit=` means the
pages were not cached: memory, not the query. `Rows Removed by Filter` in the thousands on a node
reading `chapters` means the predicate did not reach the index. `temp read/written` or
`external merge` means a sort spilled past `work_mem`. Per-node timings are off
(`auto_explain.log_timing=off`) to keep instrumentation cheap; `EXPLAIN (ANALYZE, BUFFERS)` the
statement by hand when you need them. `docs/perf/2026-09-prod-diagnostics.sql` is the full
evidence-gathering script.

Running your own Postgres: add `pg_stat_statements,auto_explain` to `shared_preload_libraries`
(a restart) and the same `auto_explain.*` settings; both ship with every Postgres distribution.

## Kubernetes

The cluster deployment is the Helm chart in
[`TimSchoenle/helm-charts`](https://github.com/TimSchoenle/helm-charts) (`charts/tankovault`),
not anything in this directory. Every release proposes the matching chart bump there: the
`helm-release` job in `.github/workflows/release-please.yaml` pins each published service to its
signed manifest-list digest and opens a pull request against that repository
([docs/RELEASING.md](../docs/RELEASING.md) describes the hand-off). The chart's own values,
templates and tests live and are gated there.

What the chart builds on, from this repository: every service exposes `/health` (liveness) and
`/ready` (readiness, with per-dependency detail where the service has dependencies to report) on
its main port and a Prometheus scrape on an isolated `9090`. Under `mtls`, the services whose
main port requires a client certificate (`control-plane`, `worker`, `sync`, `render` and
`challenge-solver`) answer probes on `TANKOVAULT_INTERNAL__TLS__PROBE_LISTEN` (default
`0.0.0.0:9091`) instead, because a kubelet probe cannot present one; `api`, `notifier` and
`frontend` keep their probes on the main port. Schema migration is a discrete one-shot step
instead of something a service does at startup. The control-plane scheduler elects a leader
through Redis, so a control-plane `Deployment` with `replicas > 1` needs `TANKOVAULT_REDIS__URL`:
without it, every replica is its own sole leader and runs the scheduler sweeps. Redis makes the
leader unique, not available: a leader that loses Redis stands down, and no replica runs the
sweeps until one reacquires the lock through Redis.

### Installing into a cluster

The one-shot steps ship as their own image, `bootstrap`, so nothing published carries a
destructive command:

| Command | When | Needs |
|---|---|---|
| `bootstrap migrate` | Before every rollout, as a `Job` or `initContainer`. Idempotent. | `TANKOVAULT_DATABASE__URL` |
| `bootstrap seed-admin` | Once, at install. Creates the first administrator: the only account privilege is ever minted for, since registration confers none. Create-only: re-running changes nothing. | `TANKOVAULT_SEED_ADMIN_PASSWORD`, and `TANKOVAULT_AUTH__PASSWORD_PEPPER` **exactly as the api has it** |
| `bootstrap seed-providers` | **Before every rollout**, alongside `migrate`. Installs presets this build ships that the deployment lacks, and re-applies each one to the providers still following it: that is how a selector fix reaches an existing installation. It never touches a provider you registered yourself, one you have unlocked in the console, or *any* provider's crawl budget or pause state. New rows arrive enabled. | `TANKOVAULT_DATABASE__URL` |

Resetting the schema is deliberately not available in any published image; `xtask reset` does
it, from a checkout, behind `TANKOVAULT_CONFIRM_RESET=1`.
