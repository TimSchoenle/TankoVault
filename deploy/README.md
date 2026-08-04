# Deployment

Container build and local orchestration for TankoVault (design §19).

## Layout
- `docker/Dockerfile` — **every image, from one file**: a cargo-chef-cached multi-stage build
  that compiles all nine workspace binaries in a single `cargo` invocation and then hands each
  one to a thin runtime stage. Pick a backend service with `--build-arg BIN=<name>` (`api`,
  `worker`, `control-plane`, `notifier`, `sync`, `challenge-solver`, `render`, `bootstrap`).
  `xtask` builds too, but is **never published**: the deploy blacklist
  (`[workspace.metadata.deploy.exclude]` in the root `Cargo.toml`, enforced by `xtask
  repo-lint`) holds it back, because it is the repository's task runner and carries `reset`.
  The install steps a deployment actually needs are the `bootstrap` image
  ([below](#installing-into-a-cluster)). Each binary is compiled natively on
  Alpine as a **musl binary** and shipped on a bare `scratch` image (no OS, no shell, no package manager — just the binary, the musl loader and `libgcc_s`
  it resolves, a CA trust store, and a numeric nonroot user). It is dynamically rather than
  statically linked because `wreq`'s BoringSSL build script `dlopen`s libclang, which a
  `crt-static` build script cannot do; see the Dockerfile's builder stage. Two tiers need
  something else and select it with `--target`:
  - `runtime-browser` — the `render` tier, which drives a real Chromium and so needs a Debian
    base rather than `scratch`.
  - `frontend` — the Dioxus WASM SPA (`web/frontend/`, built with the `dx` CLI) shipped
    alongside the `frontend` axum server (`services/frontend/`) on `scratch`. The server serves
    the SPA and reverse-proxies `/v1/*` (REST + SSE) to the `api` service, so the SPA's
    same-origin API calls resolve without CORS. This was a second Dockerfile until its musl
    cross-build was folded into the shared Alpine builder, which had been compiling the same
    dependency graph a second time for a binary the workspace already builds.
- `docker-compose.yml` — the full end-to-end local stack: Postgres 18 (pgvector), Redis 8, NATS
  (JetStream), TRAWL, the one-shot `migrate`/`seed`/`seed-providers` steps (the
  `bootstrap` image, so the local stack exercises the artefact a cluster runs), every backend
  service, and the web frontend. **This is the only supported deployment shape** — see [Kubernetes](#kubernetes)
  below.

## Quick start
```bash
# From the repo root (compose build context is the repo root).
docker compose -f deploy/docker-compose.yml up --build
```
This applies migrations, seeds a demo admin (`admin` / `changeme12345`) and a placeholder
Madara provider, then starts all services and the frontend.

**Open the app at http://localhost:3000** — the `frontend` server serves the SPA and proxies
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
denotes nesting, e.g. `TANKOVAULT_DATABASE__URL`), then file-backed values —
`$TANKOVAULT_SECRETS_DIR` and `TANKOVAULT_<KEY>_FILE`. The compose file sets these inline.

The file layers are what the Kubernetes charts use: a `Secret` mounted as a volume keeps
credentials out of the pod's environment, and a rotated file makes the service rebuild itself
rather than requiring a restart. A key set by two of the last three layers fails the boot
instead of being resolved by precedence — see
[`docs/CONFIGURATION.md` §7](../docs/CONFIGURATION.md#7-secrets).

The complete surface — every `TANKOVAULT_*` key, its default, and which service reads it — is
[`docs/CONFIGURATION.md`](../docs/CONFIGURATION.md). The short version:

**Required, with no working default** (compose fails fast rather than booting insecure):
- `TANKOVAULT_AUTH__JWT_SECRET` — API token signing secret.
- `TANKOVAULT_AUTH__PASSWORD_PEPPER` — *optional* server-side password pepper mixed into every
  argon2id hash so a database leak alone can't be brute-forced offline. Empty (the default)
  keeps hashing un-peppered. Once set it must stay stable (or existing passwords stop
  verifying) and must be given to both the `api` and `seed` services with the same value.
- `TANKOVAULT_ANILIST__CLIENT_ID` / `__CLIENT_SECRET` / `__REDIRECT_URI` — AniList OAuth app.
- `TANKOVAULT_ANILIST__TOKEN_ENCRYPTION_KEY` — base64 32-byte key for tokens at rest; generate
  with `openssl rand -base64 32`.

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
All base images are pinned by digest, cargo resolves against the committed `Cargo.lock`
(`--locked`), and passing `SOURCE_DATE_EPOCH` clamps image layer timestamps so repeated
builds of the same commit are byte-identical:
```bash
docker build -f deploy/docker/Dockerfile \
  --build-arg BIN=api \
  --build-arg SOURCE_DATE_EPOCH="$(git log -1 --format=%ct)" \
  -t tankovault-api .
```
The dependency graph is compiled once by cargo-chef (reused across every service image and
exported by CI via the GHA layer cache); BuildKit `type=cache` mounts additionally keep the
crate registry warm across local rebuilds. Refresh a pinned digest with
`docker buildx imagetools inspect <image:tag>`.

## Running migrations only
```bash
docker compose -f deploy/docker-compose.yml run --rm migrate
```

### Upgrading past migration 0026 — pgvector is required

Migration `0026_recsys_signals` runs `CREATE EXTENSION vector`. From that release on, **the
database must have [pgvector](https://github.com/pgvector/pgvector) available**; the migration
fails loudly rather than degrading, because a recommender that silently returns nothing is worse
than one that refuses to start.

- **Using the compose stack:** nothing to do. The `postgres` service moved from
  `postgres:18-alpine` to `pgvector/pgvector:pg18` — the same upstream Postgres major with the
  extension preinstalled, same entrypoint and environment contract, so the existing `pgdata`
  volume is reused in place. Pull the new image and bring the stack up.
- **Running your own Postgres:** install the extension package for your platform *before*
  applying migrations (`apt install postgresql-18-pgvector`, `CREATE EXTENSION vector`, or your
  managed provider's equivalent — RDS, Cloud SQL and Azure Flexible Server all ship it behind an
  allowlist setting). The migration only needs it to be installable; it issues the
  `CREATE EXTENSION` itself.

Rolling back drops the recommender's tables but deliberately leaves the extension in place —
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
  `service_healthy` rather than `service_started` — previously the frontend started as soon as
  the API *process* existed, which raced on every `compose up`.
- The scratch containers additionally run `read_only: true`, `cap_drop: [ALL]` and
  `no-new-privileges`. `read_only` is safe by construction there: the image *is* the binary,
  the musl loader, `libgcc_s` and a CA bundle, so there is no writable path to depend on.
  `render` gets the capability drop but not `read_only` — it is a Debian base under a Chromium
  that writes a profile and a cache to paths not enumerable from here.

## Observability

Every service serves a Prometheus scrape on an isolated `9090`; the metric inventory and the
runbook for every alert are in [`docs/OBSERVABILITY.md`](../docs/OBSERVABILITY.md).

There is no compose overlay for collection. A Prometheus/Grafana/blackbox overlay and its
config lived here until 2026-08-03; the scrape config, recording and alerting rules and the
provisioned dashboard now live in the chart that actually deploys them,
[`TimSchoenle/helm-charts`](https://github.com/TimSchoenle/helm-charts) (`charts/tankovault`),
where they are gated by that repository's own tests. Keeping a second copy here meant two sets
of rules to edit and only one of them deployed.

## Kubernetes

**Not implemented.** Tracked as design §19; `docs/IMPLEMENTATION_STATUS.md` is the live status.

This section previously described a Helm chart at `deploy/helm/tankovault` — its values,
HPAs, probe wiring and a linked `README.md` — none of which has ever existed: `deploy/helm/`
held four empty, untracked directories. An operator following it reached a dead link. The
claim is removed rather than replaced with a chart, because a chart nobody has rendered against
a real cluster would be the same defect wearing different clothes.

**The only supported deployment today is `deploy/docker-compose.yml` on a single host.** It has
no replica story for the api and worker tiers beyond compose's own `replicas:`.

What a future chart already has to build on, and would not have to invent: every service
exposes `/health` (liveness) and `/ready` (readiness with per-dependency detail) on its main
port and a Prometheus scrape on an isolated `9090`, schema migration is already a discrete
one-shot step rather than something a service does at startup, and the control-plane scheduler
already elects a leader through Redis — so a `Deployment` with `replicas > 1` is safe there.

### Installing into a cluster

The one-shot steps ship as their own image, `bootstrap`, so nothing published carries a
destructive command:

| Command | When | Needs |
|---|---|---|
| `bootstrap migrate` | Before every rollout, as a `Job` or `initContainer`. Idempotent. | `TANKOVAULT_DATABASE__URL` |
| `bootstrap seed-admin` | Once, at install. Creates the first administrator — the only account privilege is ever minted for, since registration confers none. Create-only: re-running changes nothing. | `TANKOVAULT_SEED_ADMIN_PASSWORD`, and `TANKOVAULT_AUTH__PASSWORD_PEPPER` **exactly as the api has it** |
| `bootstrap seed-providers` | Once, at install, if you want the built-in provider presets. Each can be disabled or retargeted from the admin console afterwards. | `TANKOVAULT_DATABASE__URL` |

Resetting the schema is deliberately not available in any published image; `xtask reset` does
it, from a checkout, behind `TANKOVAULT_CONFIRM_RESET=1`.
