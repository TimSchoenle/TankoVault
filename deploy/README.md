# Deployment

Container build and local orchestration for TankoVault (design §19).

## Layout
- `docker/Dockerfile` — a single parameterised, cargo-chef-cached multi-stage build for the
  Rust services. Pick the binary with `--build-arg BIN=<name>` (`api`, `worker`,
  `control-plane`, `notifier`, `sync`, `challenge-solver`, `render`, `xtask`). Runtime image
  is distroless, nonroot.
- `docker/Dockerfile.frontend` — builds the Dioxus WASM SPA (`web/frontend/`) with the `dx`
  CLI, then serves it behind nginx. nginx also reverse-proxies `/v1/*` (REST + SSE) to the
  `api` service, so the SPA's same-origin API calls resolve without CORS.
- `docker/frontend.nginx.conf` — the nginx server block (SPA fallback + `/v1/*` proxy).
- `docker-compose.yml` — the full end-to-end local stack: Postgres 19, Redis 7, NATS
  (JetStream), FlareSolverr, a one-shot `migrate`+`seed`, every backend service, and the
  web frontend.

## Quick start
```bash
# From the repo root (compose build context is the repo root).
docker compose -f deploy/docker-compose.yml up --build
```
This applies migrations, seeds a demo admin (`admin` / `changeme12345`) and a placeholder
Madara provider, then starts all services and the frontend.

**Open the app at http://localhost:3000** — nginx serves the SPA and proxies `/v1/*` to the
API, so this single origin is all a browser needs. The API is also exposed directly on
http://localhost:8080 for tooling/curl.

Ports: **frontend `3000`**, api `8080`, control-plane `8081`, notifier `8082`, sync `8083`,
render `8084`, challenge-solver `8090`, FlareSolverr `8191`.

### Why the frontend sits behind a proxy
The WASM client calls the API same-origin (`web/frontend/src/api.rs` → `API_BASE = ""`) and
opens the live-notification SSE stream (`/v1/me/stream`) via the browser `EventSource` API.
Serving the SPA and proxying `/v1/*` from one nginx origin makes those calls resolve with no
cross-origin hop, and the proxy disables buffering so SSE frames flush immediately.

## Configuration
Every service reads layered config via `tankovault-config`: optional TOML at `$TANKOVAULT_CONFIG`,
then `TANKOVAULT_*` environment variables (`__` denotes nesting, e.g.
`TANKOVAULT_DATABASE__URL`). The compose file sets these inline.

**Replace before any non-local use** (the compose defaults are dev-only):
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
# A backend service:
docker build -f deploy/docker/Dockerfile --build-arg BIN=api -t tankovault-api .
# The frontend:
docker build -f deploy/docker/Dockerfile.frontend -t tankovault-frontend .
```

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
- The **Rust service** containers are distroless (no shell), so they carry no container-level
  healthcheck; `depends_on` ordering plus the infra healthchecks (postgres/redis/nats) and
  the `migrate` completion gate handle startup sequencing. The frontend (nginx) and infra
  images do have healthchecks.

## Not yet included
- Kubernetes / Helm chart (design §19) — the compose stack is the current target.
- Per-service HTTP health probes for the distroless backends (they ship no shell/curl; wire
  k8s HTTP probes against each service's `/health` and `/ready` when the chart lands).
