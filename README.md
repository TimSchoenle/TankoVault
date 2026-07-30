# TankoVault

A fully-Rust, multi-microservice **manga / manhwa / manhua aggregator and tracker**. TankoVault
indexes series metadata across many independent provider sites, treats each work as one **canonical
series** with many **provider sources**, and layers watchlists, read progress, notifications, and
AniList sync on top.

> **Links, not content.** TankoVault stores **links and metadata only**. It never downloads, caches,
> or serves chapter images or page content. Operators remain responsible for the legality of crawling
> any given source in their jurisdiction and under each site's terms.

## Features

- **Cross-provider aggregation** — merge title, alt titles, description, tags, cover URL, status, and
  author from many sites into a single canonical series with a search vector.
- **Two scan cadences** — a rare **full scan** (rebuild the archive) and a frequent **fast scan** (pick
  up only new chapters from each provider's "latest" feed).
- **No-code provider onboarding** — config-driven adapters cover the common CMS case; custom adapters
  drop in behind the same `SourceAdapter` trait.
- **Domain-migration resilience** — stored links are relative paths resolved against the provider base
  URL at read time, so a site moving domains is a one-field change.
- **Challenge bypass** — a cheap classifier detects Cloudflare / JS / Turnstile interstitials in
  milliseconds and delegates to a pluggable challenge-solver service (FlareSolverr by default).
- **User system** — watchlists, read progress, per-title notification opt-in, and back-sync to
  **AniList**.
- **Operator console** — live scan progress, provider health, and adapter config editing.
- **Horizontal scale** on the API and worker tiers.

## Architecture

Eight deployable services plus shared libraries, all in one Cargo workspace. PostgreSQL is the system
of record; NATS JetStream distributes tasks and domain events; Redis backs caching, rate-limit state,
and leader election.

```mermaid
flowchart TD
    FE[Frontend Dioxus WASM SPA] -->|REST + SSE| API[API service Axum]
    API -->|reads/writes| PG[(PostgreSQL)]
    API -->|control| CP[Control plane]
    CP -->|tasks via NATS| W[Worker pool]
    W -->|solve request| CS[Challenge solver]
    W -->|domain events| NOTIF[Notifier]
    W -->|progress| SYNC[AniList sync]
```

| Service | Role |
| --- | --- |
| `api` | Public Axum edge: auth, read models, write endpoints, admin, SSE scan feed. |
| `control-plane` | Scheduler, run planner, task fan-out, provider health; singleton leader. |
| `worker` | Fetch + parse via adapters, upsert chapter/metadata deltas. |
| `notifier` | New-chapter → user notification fan-out. |
| `sync` | AniList push/pull. |
| `challenge-solver` | Modular bot-management bypass tier (FlareSolverr-backed, pluggable). |
| `render` | Optional headless-browser tier for JS-rendered provider pages. |
| `frontend` | Serves the Dioxus WASM SPA and reverse-proxies `/v1/*` to the API. |

### Workspace layout

```
crates/
  domain/       pure types (Series, Chapter, Provider, enums) — no I/O
  db/           sqlx repositories and query modules
  adapters/     SourceAdapter trait + Madara/config-driven + custom adapters
  fetch/        Fetcher trait, browser emulation, rate limiting, caching, solver client
  solver/       ChallengeSolver trait + detection + FlareSolverr/render back-ends
  contracts/    NATS message/event schemas (serde)
  auth/         password hashing, JWT, RBAC guards
  config/       layered, typed config loading (file + env)
  matcher/      series canonicalisation / fuzzy matching
  service/      shared service wiring
  email/        outbound mail (lettre)
  bus/          NATS JetStream client helpers
  api-client/   generated typed client (from the OpenAPI spec)
  test-support/ shared test fixtures
services/       api, control-plane, worker, notifier, sync, challenge-solver, render, frontend
web/frontend/   Dioxus WASM SPA + Tailwind (excluded from the host workspace)
migrations/     versioned sqlx migration SQL
deploy/         Dockerfiles, docker-compose, Helm chart
xtask/          dev/ops tasks: migrate, reset, seed, openapi, sqlx-prepare
```

## Tech stack

- **Runtime / web** — Tokio, Axum, tower / tower-http
- **Storage** — PostgreSQL 19 via SQLx (compile-time-checked SQL), Redis 7 via `fred`
- **Messaging** — NATS JetStream (`async-nats`)
- **Crawl** — `wreq` + `wreq-util` (BoringSSL; browser TLS/HTTP2 fingerprint emulation) with
  `governor` rate limiting, `scraper` HTML parsing, optional `chromiumoxide` headless render.
  Internal service-to-service HTTP stays on `reqwest` (rustls).
- **Frontend** — Dioxus (WASM) + `dioxus-router`, TailwindCSS v4
- **Security** — Argon2id password hashing, JWT access + rotating refresh tokens
- **Observability** — `tracing` + OpenTelemetry + Prometheus metrics
- **IDs / time** — UUID v7, `time`

Rust edition 2024, MSRV 1.85. Lints are strict workspace-wide (`unsafe_code = "forbid"`,
`clippy::pedantic`).

## Quick start (Docker)

The full stack runs from the repo root (the compose build context is the repo root):

```bash
docker compose -f deploy/docker-compose.yml up --build
```

This applies migrations, seeds a demo admin (`admin` / `changeme12345`) and a placeholder provider,
then starts every service and the frontend.

Open the app at **http://localhost:3000** — the `frontend` server serves the SPA and proxies `/v1/*`
to the API, so a browser needs only this one origin. The API is also exposed directly on
http://localhost:8080 for tooling.

| Service | Port |
| --- | --- |
| frontend | 3000 |
| api | 8080 |
| control-plane | 8081 |
| notifier | 8082 |
| sync | 8083 |
| render | 8084 |
| challenge-solver | 8090 |
| FlareSolverr | 8191 |

Apply migrations only:

```bash
docker compose -f deploy/docker-compose.yml run --rm migrate
```

See [`deploy/README.md`](deploy/README.md) for single-service image builds and reproducible
builds. `docker compose` on a single host is the only supported deployment shape today;
Kubernetes is not implemented.

## Configuration

Every service reads layered config via `tankovault-config`: optional TOML at `$TANKOVAULT_CONFIG`,
overlaid by `TANKOVAULT_*` environment variables (`__` denotes nesting, e.g.
`TANKOVAULT_DATABASE__URL`). The compose file sets dev defaults inline.

**[`docs/CONFIGURATION.md`](docs/CONFIGURATION.md) is the complete reference** — every key, its
default, which services read it, and the failure modes worth knowing (several are silent).

**Required before any non-local use** — these have no working default and the stack fails fast
rather than booting insecure:

- `TANKOVAULT_AUTH__JWT_SECRET` — API token signing secret.
- `TANKOVAULT_AUTH__PASSWORD_PEPPER` — optional server-side pepper mixed into every argon2id hash;
  once set it must stay stable and match across the `api` and `seed` services.
- `TANKOVAULT_ANILIST__CLIENT_ID` / `__CLIENT_SECRET` / `__REDIRECT_URI` — AniList OAuth app.
- `TANKOVAULT_ANILIST__TOKEN_ENCRYPTION_KEY` — base64 32-byte key for tokens at rest
  (`openssl rand -base64 32`).

## Local development

Build, test, and lint the host workspace (the frontend is excluded and built separately):

```bash
cargo build
cargo test
# `--all-targets` silently EXCLUDES doc tests, and the `///` examples are contracts here, so
# they get their own run. CI does the same.
cargo test --workspace --doc
cargo clippy --all-targets -- -D warnings
cargo fmt --check
```

Property tests (`proptest`) live in `tests/prop_*.rs` next to the code they cover and run in that
ordinary `cargo test` — no extra toolchain. Coverage-guided fuzzing needs nightly and therefore
lives outside the workspace, in [`fuzz/`](fuzz/README.md), which no CI gate runs:

```bash
cargo +nightly fuzz build                                     # all targets compile
cargo +nightly fuzz run adapters_html_parsers \
  fuzz/corpus/adapters_html_parsers fuzz/seeds/adapters_html_parsers \
  -- -max_total_time=60 -timeout=2 -rss_limit_mb=512
```

Dev/ops tasks live in `xtask` (`migrate` / `reset` / `seed` / `sqlx-prepare` read `DATABASE_URL`;
`openapi` does not):

```bash
cargo run -p xtask -- migrate         # apply pending migrations
cargo run -p xtask -- reset           # DESTRUCTIVE: drop + recreate schema (dev only)
cargo run -p xtask -- seed            # demo admin + built-in provider presets
cargo run -p xtask -- openapi         # regenerate openapi.json + the typed api-client
cargo run -p xtask -- sqlx-prepare    # refresh the committed sqlx offline query cache
```

### Frontend

The Dioxus SPA targets `wasm32-unknown-unknown` and is built with the `dx` CLI from `web/frontend/`:

```bash
cd web/frontend
npm install          # first time only (Tailwind tooling)
npm run css:watch    # terminal 1: input.css -> assets/main.css
dx serve             # terminal 2: dev server with hot reload
dx build --release   # static WASM + assets
```

See [`web/frontend/README.md`](web/frontend/README.md) for the design system, the generated API
client, and the i18n rules.

## Documentation

- [`docs/design.md`](docs/design.md) — authoritative architecture and build specification.
- [`docs/OPERATIONS.md`](docs/OPERATIONS.md) — running and operating the fleet.
- [`docs/CONFIGURATION.md`](docs/CONFIGURATION.md) — every `TANKOVAULT_*` key and its default.
- [`docs/PROVIDERS.md`](docs/PROVIDERS.md) — provider adapters.
- [`docs/READING_PROGRESS_AND_SYNC.md`](docs/READING_PROGRESS_AND_SYNC.md) — progress and AniList sync.
- [`docs/PRODUCTION_READINESS.md`](docs/PRODUCTION_READINESS.md) — production checklist.
- [`docs/IMPLEMENTATION_STATUS.md`](docs/IMPLEMENTATION_STATUS.md) — current status.
- [`docs/audit/`](docs/audit/README.md) — full codebase audit (2026-07-29): findings and cleanup roadmap.
- [`openapi.json`](openapi.json) — canonical REST API spec (also served at `/scalar`).
