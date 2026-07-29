# Deployment

Container build and local orchestration for TankoVault (design §19).

## Layout
- `docker/Dockerfile` — a single parameterised, cargo-chef-cached multi-stage build for the
  Rust services. Pick the binary with `--build-arg BIN=<name>` (`api`, `worker`,
  `control-plane`, `notifier`, `sync`, `challenge-solver`, `render`, `xtask`). Each binary is
  compiled natively on Alpine as a **musl binary** and shipped on a bare `scratch` image (no
  OS, no shell, no package manager — just the binary, the musl loader and `libgcc_s` it
  resolves, a CA trust store, and a numeric nonroot user). It is dynamically rather than
  statically linked because `wreq`'s BoringSSL build script `dlopen`s libclang, which a
  `crt-static` build script cannot do; see the Dockerfile's builder stage. The `render` tier
  needs a real Chromium at runtime, so it uses the Debian `runtime-browser` stage
  (`--target runtime-browser`) instead.
- `docker/Dockerfile.frontend` — builds the Dioxus WASM SPA (`web/frontend/`) with the `dx`
  CLI, compiles the `frontend` axum server (`services/frontend/`) as a **static musl binary**,
  and ships both on a bare `scratch` image (like every backend service). The server serves the
  SPA and reverse-proxies `/v1/*` (REST + SSE) to the `api` service, so the SPA's same-origin
  API calls resolve without CORS.
- `docker-compose.yml` — the full end-to-end local stack: Postgres 17, Redis 7, NATS
  (JetStream), FlareSolverr, a one-shot `migrate`+`seed`, every backend service, and the
  web frontend. **This is the only supported deployment shape** — see [Kubernetes](#kubernetes)
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
`8085`, challenge-solver `8090`, FlareSolverr `8191`, and every backend's Prometheus scrape on
`9090`.

### Why the frontend sits behind a proxy
The WASM client calls the API same-origin (`web/frontend/src/api.rs` → `API_BASE = ""`) and
opens the live-notification SSE stream (`/v1/me/stream`) via the browser `EventSource` API.
Serving the SPA and proxying `/v1/*` from one origin makes those calls resolve with no
cross-origin hop, and the proxy streams responses unbuffered so SSE frames flush immediately.

## Configuration
Every service reads layered config via `tankovault-config`: optional TOML at `$TANKOVAULT_CONFIG`,
then `TANKOVAULT_*` environment variables (`__` denotes nesting, e.g.
`TANKOVAULT_DATABASE__URL`). The compose file sets these inline.

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
# The frontend:
docker build -f deploy/docker/Dockerfile.frontend -t tankovault-frontend .
```

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
