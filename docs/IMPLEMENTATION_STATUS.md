# TankoVault — Implementation Status & Handoff

This file tracks the build state of the system described in [`design.md`](./design.md).
Update it at the end of every coding session: mark what landed, and leave a precise
"pick up next" list so the next session starts without re-deriving context.

**Last updated:** 2026-07-20 (Session 13)

> Session 13 landed: the operator **Console is now a full admin control surface** (design
> §17.2.7). New backend read models: `db::repo::stats` (`system_overview` → catalogue/scan
> rollup via scalar subqueries; `provider_stats` → per-provider series/source/chapter counts,
> 24h/7d freshness, last-scan + last-run health via a CTE join over `series_sources`/`chapters`/
> `scan_runs`), `scans::recent_failed_tasks` (+ `FailedTaskView`, joined to run+provider), and
> `audit::list_recent` (+ `AuditView`, joined to the actor username). Five new **Operator**-gated
> GET endpoints on the api: `/v1/admin/stats`, `/v1/admin/providers/stats`, `/v1/admin/scans`
> (recent-runs list), `/v1/admin/scan-failures`, `/v1/admin/audit`. Frontend `console.rs` was
> rewritten around a shared **auto-refresh tick** (pausable "Live · 4s" control; 5 read-only
> panels poll on it via `gloo-timers`, kept separate from the editing surfaces so refreshes
> never clobber in-progress edits): a **system-overview KPI header** (9 tiles), a **live scan
> queue** (trigger + active-run progress + recent-runs table + a task-failure triage feed with
> errors), a **per-provider statistics table**, the existing **provider management** surface,
> the **merge queue**, and the **audit trail**. Relative timestamps use the browser's own
> `js_sys::Date` parser (no date crate in the bundle). New DTOs (`SystemStats`, `ProviderStat`,
> `FailedTask`, `AuditEntry`) + api-client methods; new deps `gloo-timers` + `js-sys`; new
> Inkstone CSS (`ik-kpis`/`ik-kpi*`, `ik-live*`, `ik-subhead`, `ik-tablewrap`, `ik-table-compact`,
> `ik-fail`, `ik-pill.run`). The old single-run poll (`recent_runs_via_get`) was removed. Gates
> green: workspace `fmt`- and `clippy -D warnings`-clean, **130 tests** (unchanged — additive
> queries + UI); wasm frontend `fmt`/`clippy -D warnings`-clean.

> Session 12 landed: **three real provider scanners** — demonicscans.org, manhuaus.com,
> kunmanga.co.uk (design §7). Every selector was derived from **live markup fetched through
> the project's own solver pipeline** (FlareSolverr), not guessed, and each is pinned by
> fixture tests. The config-vs-code split is the headline (new [`docs/PROVIDERS.md`](./PROVIDERS.md)):
> **manhuaus and kunmanga are just a config of the existing Madara parser** — data-only
> `ProviderPreset`s with a handful of selector overrides — while **demonicscans needed a
> custom adapter** (bespoke PHP layout). Manhuaus overrides: path pagination
> `/manga/page/{page}/`, `link[rel=next]` (theme drops `a.nextpostslink`), and a lazy-load
> cover (`img@data-src`). Kunmanga overrides: path pagination, ad-tile exclusion
> (`div.page-item-detail:not(.custom-item-ad)`), `a[aria-label="Next"]`, and `<div>` (not
> `<li>`) chapter rows. New `DemonicScansAdapter` (`crates/adapters/src/demonicscans.rs`):
> `/advanced.php?list={page}` catalogue, home-feed latest, and a series page whose synopsis
> is split out of SEO boilerplate on the `"The Summary is"` marker, whose status/alternatives
> come from `#manga-info-stats` label/value rows, and whose chapters carry parsed ISO dates.
> Shared `html.rs` gained `absolutize` (CDN-preserving cover resolver), `parse_ymd_date`, and
> a promoted `map_status` (removed the private copy in `generic.rs`, which now uses both).
> Wiring: `factory.rs` dispatches `Custom("demonicscans")`; `lib.rs` exports
> `DemonicScansAdapter` + `builtin_presets`; `xtask seed` now seeds all three presets (was one
> placeholder). Gates green: workspace `fmt`- and `clippy -D warnings`-clean; **130 tests**
> (was 115: +3 html helpers, +1 `split_alt_titles`, +1 custom-dispatch, +2 preset sanity,
> +4 demonicscans fixture, +4 Madara-preset fixture). Solver-fetched HTML lives only in the
> scratchpad; the checked-in fixtures are trimmed representatives of it.

> Session 11 landed: **full provider lifecycle control in the operator Console** (design
> §17.2.7). The Providers panel is now a complete admin surface: an Admin-only **create**
> form (slug/name/base_url/adapter/config), a per-provider **editor card** (name, the
> `base_url` domain-migration with a confirm step, adapter `config` JSON, and crawl
> **politeness** — rps/concurrency/crawl-delay/user-agent, clamped server-side),
> **enable/disable** state override, **per-provider scan** (fast/full), a live **adapter
> dry-run** ("Test" → `POST providers/:id/test`, rendering the parsed sample), and Admin
> **delete** (with confirm; source links cascade). Backend gained the two pieces the UI was
> missing: `providers::delete` (repo) + `DELETE /v1/admin/providers/:id` (Admin, audited)
> and `POST /v1/admin/providers/:id/state` (Operator, audited). `update_provider` /
> `create_provider` / `test_adapter` already existed and are now wired in. Frontend: the
> `Provider` DTO gained `adapter`/`config`/`politeness` + a new `Politeness` DTO;
> `Role::is_admin`; the api client gained `create_provider` / full `update_provider` /
> `delete_provider` / `set_provider_state` / `test_adapter`. All gates green: workspace +
> wasm frontend `fmt`- and `clippy -D warnings`-clean; **115 tests** unchanged (the new
> surface is UI + additive handlers). Backend images unchanged this session.

> Session 10 landed: the **frontend is now part of the local Docker Compose stack**, making
> `docker compose -f deploy/docker-compose.yml up --build` a genuine **full E2E environment**.
> A new `deploy/docker/Dockerfile.frontend` builds the Dioxus 0.7.9 WASM bundle with a
> version-matched `dx` CLI (installed in the builder; the frontend crate builds standalone
> since it is workspace-excluded and self-pins deps), then serves it behind **nginx**
> (`deploy/docker/frontend.nginx.conf`). nginx is the single front-door origin on
> **`http://localhost:3000`**: it serves the SPA with client-side-routing fallback **and**
> reverse-proxies `/v1/*` to `api:8080` with buffering/caching off + long timeouts so the SSE
> streams (`/v1/me/stream`, `/v1/admin/scans/stream`) flush live. This is what makes the
> client's same-origin calls (`api.rs: API_BASE = ""`) resolve with no CORS hop. The proxy
> uses Docker's embedded resolver (`127.0.0.11`) so nginx tolerates an `api` restart/re-IP.
> Also **wired Redis into `control-plane`** (`TANKOVAULT_REDIS__URL`) — the compose `redis` service
> was previously started but unused; it now backs singleton-scheduler leader election.
> **NATS** now exposes its monitoring port (`-m 8222`) with a compose `/healthz` healthcheck,
> and every NATS-dependent service waits on `nats: service_healthy` (was `service_started`);
> `worker` also gained a `challenge-solver` start-dep; AniList `redirect_uri` moved to the
> `:3000` origin. **Verified:** the frontend image builds, nginx serves the SPA + hashed
> JS/WASM (`application/wasm`)/CSS with 200s, SPA fallback works, and the `/v1/*` proxy
> resolves `api` via Docker DNS and passes the path through (proved against a stand-in `api`).
> `docker compose config` validates. Backend images were not rebuilt this session (compose
> wiring changes are additive/validated). No Rust code changed; test count unchanged (115).

> Session 9 landed: **live per-user notification push** — the last functional gap in the
> notification path (design §14 "pushes to connected clients (WS/SSE via the API)" and §17.4
> "real-time unread badges"). The notifier, after writing each in-app `notifications` row,
> now publishes a best-effort **core-NATS** (non-durable) `UserNotification` — carrying the
> recipient's fresh unread count — to `notify.user.<user_id>`. The API exposes
> `GET /v1/me/stream` (SSE): it authenticates via an `access_token` **query parameter**
> (the browser `EventSource` API cannot set an `Authorization` header), subscribes to the
> caller's subject, and relays each push as a `notification` event with keep-alive. Core NATS
> (not JetStream) is deliberate: the durable record is the DB row, so a disconnected client
> simply misses the live push and reconciles via its unread count on reconnect — no
> per-user JetStream fan-out or retained backlog. The API connects NATS **best-effort**
> (`AppState.bus: Option<Bus>`): a broker outage degrades only `/v1/me/stream` to `503`
> (`ApiError::Unavailable`), never the rest of the edge. Frontend: a new `live.rs` opens an
> `EventSource` (gloo-net `eventsource` feature) from the `Shell`, keyed on the access token
> via `use_resource` so a sign-out/refresh tears down and re-establishes the stream cleanly;
> each push updates the rail's unread badge in real time. `bus` gained a stored core client
> + `publish_user_notification`/`subscribe_user_notifications`; `db` gained
> `notifications_unread_count`; compose passes `TANKOVAULT_NATS__URL` to `api`. Whole workspace
> **and** the wasm frontend stay `rustfmt`- and `clippy -D warnings`-clean (also fixed two
> pre-existing `discover.rs` lints); **115 tests green** (was 113: +1 subject, +1
> `UserNotification` round-trip).

> Session 8 landed: the **CI pipeline** (design §19/§21, the last ⬜ infra item) and the
> completion of **`xtask`** (the last 🟡 backend component). `xtask` now has a `reset`
> command alongside `migrate`/`seed`: it drops & recreates the `public` schema and re-applies
> every migration from scratch (via a new `tankovault_db::reset` that reuses the embedded
> migrator), guarded by `TANKOVAULT_CONFIRM_RESET=1` so a mis-pointed `DATABASE_URL` can't wipe a
> non-local database by accident. CI lives in `.github/workflows/ci.yml`: parallel jobs for
> `fmt --check`, `clippy --workspace --all-targets -D warnings`, `cargo test --workspace`, the
> `wasm32` frontend check (`web/frontend`), `cargo-deny` (advisories/licenses/sources/bans via
> `deny.toml`), `cargo-audit`, and a `docker build` matrix over every service `BIN`
> (render selects the `runtime-browser` target, all others the distroless `runtime`). Whole
> workspace stays `rustfmt`- and `clippy -D warnings`-clean; **113 tests green** (unchanged —
> `reset`/CI add no unit tests).

> Session 7 landed: the **`render` service** is now a real implementation (was a stub) —
> the last 🟡 backend service. It drives a long-lived, **lazily-launched** `chromiumoxide`
> headless browser and exposes `POST /v1/render { url, wait_selector?, wait_ms? }` returning
> the fully-rendered DOM + final URL + session cookies + effective user-agent (design §9,
> JS-rendered listings). Per the same section it **doubles as an alternate `ChallengeSolver`
> back-end**: `POST /v1/solve` mirrors the challenge-solver contract exactly (same
> `SolveRequest`→`SolveOutcome`), so the fetch pipeline can point at it when FlareSolverr is
> unavailable — no adapter/worker change. The browser launches on first use so `/health`
> `/ready` come up even without a Chrome binary (a render/solve then fails cleanly with `502`).
> `chromiumoxide` is added default-features-only (the CDP link is a plain local `ws://`, so no
> extra TLS stack is pulled). Deploy: a new `runtime-browser` Dockerfile stage (Debian slim +
> Chromium, `CHROME_PATH`/`NO_SANDBOX` preset) that the compose `render` service selects via
> `target:`; the distroless default stays the default for every other service. Whole workspace
> stays `rustfmt`- and `clippy -D warnings`-clean; **113 tests green** (was 109: +4 render).

> Session 6 landed: the notifier's **email (SMTP) channel** — the last pluggable
> `NotificationChannel` back-end named in design §14. It is config-driven (`email_smtp_url`
> in lettre's `from_url` format + `email_from` + `email_to[]`), sends a plain-text
> new-chapter alert to one or more recipients, and fires once per genuinely-new chapter
> alongside the webhook/Discord channels (best-effort; failures logged, never fatal).
> `lettre` uses **rustls + ring** (matching the rest of the stack; no native-tls/OpenSSL in
> the distroless images) and the non-pooled transport so construction is runtime-independent.
> Whole workspace stays `rustfmt`- and `clippy -D warnings`-clean; **109 tests green**
> (was 103: +6 email channel).

> Session 5 landed: the **control-plane is now complete** (✅). The progress **aggregator**
> is wired into the service — it consumes `scan.progress`, finalises a run via the atomic
> `scans::finalize_if_complete` once every planned task has settled, and republishes one
> terminal event so the console sees a `completed`/`failed` run without DB-polling to a
> conclusion. **Redis leader election** for the singleton scheduler landed too
> (`SET NX PX` acquire + `GET`/`PEXPIRE` renew; a cloneable `Leadership` flag gates every
> scheduler sweep); it fails open to sole-leader when no `redis` block is configured and
> stands down on Redis errors. Whole workspace stays `rustfmt`- and `clippy -D warnings`-
> clean; **103 tests green** (was 98: +3 aggregator, +2 leader-election).

> Session 4 landed: the **Phase 2 frontend** — a Dioxus 0.7 (WASM SPA) + `dioxus-router`
> app implementing the **Inkstone** design system and all core screens (Discover with
> filters/sort, Series detail with sources + chapters + watchlist/notify, Reading feed,
> Watchlist board, Notifications, Search, Login/Register, and the operator Console: scan
> trigger + run progress, provider health tiles, the `base_url` domain-migration editor,
> and the merge queue). It lives in `web/frontend/` (excluded from the host workspace),
> talks to the API's `/v1` contract via a typed `gloo-net` client, holds the access token
> in memory (boot-time silent refresh), and gates the Console by the JWT-decoded role.
> `cargo check --target wasm32-unknown-unknown` **and** a full `dx build --platform web`
> both pass. Backend is unchanged (98 tests still green).
>
> Session 3 landed: pluggable **external notification channels** for the notifier
> (generic JSON webhook + Discord incoming-webhook) behind a `NotificationChannel` trait,
> config-driven and fired once per genuinely-new chapter so rescans never re-alert. The
> frontend (Phase 2) remains the major outstanding piece. The whole workspace stays
> `rustfmt`- and `clippy -D warnings`-clean; 98 tests green (was 91).
>
> Session 2 landed: the AniList sync service; canonicalisation wired into the scan ingest
> path + operator merge queue (Phase 4); the remaining `/me` and admin API endpoints; and
> the Docker/compose deployment stack.

---

## 1. How to build & run

```bash
# Type-check the whole backend workspace (frontend is excluded; see §5).
cargo check --workspace

# Lint gate as CI runs it (now clean workspace-wide).
cargo fmt --check
cargo clippy --workspace --all-targets -- -D warnings

# Run all unit tests (113 green).
cargo test --workspace

# Full E2E stack (Postgres + Redis + NATS + FlareSolverr + all services + web frontend).
# Open the app at http://localhost:3000 (nginx serves the SPA + proxies /v1/* to the api).
docker compose -f deploy/docker-compose.yml up --build

# Dev/ops tasks (reads DATABASE_URL). reset is destructive and guarded:
cargo run -p xtask -- migrate
TANKOVAULT_CONFIRM_RESET=1 cargo run -p xtask -- reset   # drop schema + re-migrate
cargo run -p xtask -- seed

# Supply-chain gates (as CI runs them).
cargo deny check
cargo audit
```

The Dioxus WASM frontend under `web/frontend/` is **excluded** from the default
workspace (it targets `wasm32` via the `dx` CLI) and is built separately.

### Toolchain / dependency policy
- Rust **edition 2024**, `rust-version = 1.85`. Toolchain in use: 1.94.
- **Latest stable** versions of every dependency (per user instruction). Deliberate
  pin-backs: `argon2 = "0.5"` (0.6 is pre-release only).
- SQLx **0.9**. Queries are runtime-checked (`query`/`query_as`), not the compile-time
  macros — this keeps the build free of a live-DB dependency (important for Docker/CI).
  `cargo sqlx prepare` can layer offline compile-time checking on later.
- `aes-gcm = "0.11"` is now in use (token encryption at rest — see §2 `auth`). Its
  `hybrid-array` API deprecates `from_slice`; construct nonces/keys via `From`/`try_from`.

---

## 2. Component status

Legend: ✅ done & compiling · 🟡 partial/skeleton · ⬜ not started

### Shared crates (`crates/`)
| Crate | Status | Notes |
|---|---|---|
| `domain` | ✅ | Entities, typed UUIDv7 ids, enums, `resolve_link` (tested), title normalize, politeness+ceilings. 23 tests. |
| `config` | ✅ | Figment layered load (defaults → TOML → `TANKOVAULT_*` env). `ConfigError` boxes the large `figment::Error`. Tested. |
| `contracts` | ✅ | NATS subjects/streams + task/progress/chapter/provider-state messages. Tested. |
| `bus` | ✅ | `async-nats` JetStream client: stream provisioning, task publish/consume, chapter events. **+ core-NATS client** for non-durable live pushes (`publish_user_notification`/`subscribe_user_notifications`; `BusError::Nats`). |
| `observability` | ✅ | `tracing` (json/pretty) + Prometheus recorder. **OTLP export deferred** (§4). |
| `db` | ✅ | Pool, embedded migrations, repos: providers, catalog (`ingest_series` tx now runs the **matcher-driven `resolve_canonical_series`** + merge-candidate recording; `list_tags`), users+refresh, scans (SKIP LOCKED), tracking (+`progress_state`, `watchlist_set_status`, `feed`, `notifications_unread_count`), matching (trigram + **`merge_series`/`list_open_merge_candidates`/`dismiss_merge_candidate`**), audit, sync (external_accounts + sync_mappings). Depends on `tankovault-matcher`. Not yet DB-integration-tested in-repo (§3). |
| `fetch` | ✅ | `Fetcher` trait + decorator stack (robots → rate-limit → cache → solving → retry → base), **SSRF guard**, solver client. 12 tests. |
| `solver` | ✅ | `ChallengeSolver` trait, `detect_challenge` classifier, FlareSolverr back-end, fake solver for the swap test. 7 tests. |
| `adapters` | ✅ | `SourceAdapter` trait, `Ctx`, generic/Madara config adapter, custom `DemonicScansAdapter`, HTML helpers (number/status/date parsers, `relativize`/`absolutize`), `builtin_presets()` (demonicscans + manhuaus + kunmanga, solver-derived), factory slug dispatch. Fixtures per provider; unit + fixture tests. See [`PROVIDERS.md`](./PROVIDERS.md). |
| `auth` | ✅ | argon2id, HS256 access JWT, rotating hashed refresh + reuse detection, RBAC. **+ `SecretBox` (AES-256-GCM) for external tokens at rest (§16).** 14 tests. |
| `matcher` | ✅ | Pure scoring/decision bands (attach/ambiguous/create). First live consumer is the sync service. 5 tests. |

### Services (`services/`)
| Service | Status | Notes |
|---|---|---|
| `api` | ✅ | **Full §11 contract.** Auth (register/login/refresh/logout), series browse/detail/chapters (resolved links), `/v1/tags`, admin provider CRUD + `base_url` migration + trigger-scan + **`providers/:id/test`** (dry-run adapter via the injected fetch stack) + **`scans/stream`** (SSE, DB-poll), `merge-candidates` list/dismiss + `series/merge`, **operator dashboard reads** (`admin/stats`, `admin/providers/stats`, `admin/scans` list, `admin/scan-failures`, `admin/audit`), `/me` watchlist/progress/feed/notifications, and the `/me/sync/anilist/*` proxy. Now also depends on `adapters`/`fetch`/`solver` for the test endpoint. **+ `GET /v1/me/stream`** (SSE live notifications; token-in-query auth; core-NATS relay via `AppState.bus: Option<Bus>`, degrades to `503` if NATS is down). |
| `worker` | ✅ | One-shot inline full/fast scan **and** JetStream consumer (consumer-group scale); idempotent `ingest_series`, emits `chapter.discovered`. |
| `control-plane` | ✅ | Scheduler (interval sweeps) + planner (run→tasks→JetStream) + `/internal/scans` trigger. **+ progress aggregator** (`scan.progress` consumer → atomic `finalize_if_complete` → republished terminal event) **+ Redis leader election** (`SET NX PX` lock w/ `GET`/`PEXPIRE` renewal; `Leadership` flag gates every sweep; fails open to sole-leader without Redis, stands down on Redis error). 5 tests. |
| `notifier` | ✅ | Consumes `chapter.discovered`, fans out to watchers (partial-index lookup), dedup, in-app rows. **+ pluggable external channels** (`NotificationChannel` trait): config-driven generic JSON webhook + Discord incoming-webhook **+ email (SMTP via `lettre`, rustls/ring)**, delivered once per genuinely-new chapter (best-effort, failures logged not fatal). **+ live push**: after each in-app row it publishes a best-effort core-NATS `UserNotification` (with the fresh unread count) to `notify.user.<id>`, relayed to connected clients by the API's `/v1/me/stream`. Pure payload/message builders tested (13 tests). |
| `challenge-solver` | ✅ | `POST /v1/solve` HTTP contract, FlareSolverr-backed default, config-selected back-end. |
| `sync` | ✅ | **NEW this session.** AniList OAuth link/unlink, pull (AniList→local) and push (local→AniList) reusing `matcher`; tokens encrypted at rest; user-selectable conflict policy (local/remote/newest-wins). 15 tests. See §7. |
| `render` | ✅ | **Real this session.** Lazily-launched `chromiumoxide` headless browser. `POST /v1/render` → rendered DOM + final URL + cookies + UA (JS-rendered listings, §9). Doubles as a `ChallengeSolver` back-end: `POST /v1/solve` mirrors the challenge-solver contract for a FlareSolverr-free bypass path. Browser launches on first use (health up without Chrome; renders `502` cleanly). Chromium-provisioned via the `runtime-browser` Docker stage. 4 browser-free tests (config + solve-outcome shaping). |
| `xtask` | ✅ | `migrate` / `reset` (drop+recreate schema, re-migrate; guarded by `TANKOVAULT_CONFIRM_RESET=1`) / `seed` (demo admin + the three built-in provider presets via `tankovault_adapters::builtin_presets()`). Reads `DATABASE_URL`. |

### Frontend (`web/frontend/`)
✅ **TankoVault redesign complete (frontend F0–F5).** The Dioxus 0.7 WASM SPA + `dioxus-router`
has been fully rebuilt to the `docs/frontend/DESIGN_SPEC.md` mockup (the "Inkstone" evolution).
Tailwind is now the **real** build (`input.css` + `tailwind.config.js` → committed, minified
`assets/main.css` via `npm run css:build`); the full design-token ramp, role/state colors, and
theme knobs (accent/density/cover-style) ship as CSS-variable swaps. Inline-SVG icon module
(`src/icons.rs`, ~45 glyphs, no web font). Self-hosted latin `.woff2` subsets (Bricolage
Grotesque + IBM Plex Sans/Mono) are vendored under `assets/fonts/` and bundled via `asset!()`
`@font-face` rules emitted from `FontFaces` in `src/main.rs` (a plain `url()` in the Tailwind CSS
is not processed by manganis, so it must not live there). **All 9 screens** are built and render
against **today's** API: Home dashboard (`/`), Discover (filter panel + 3-state tag chips +
provider checkboxes + dual year/min-chapter ranges + 6-option sort + removable active-filter
chips + pagination), blurred-hero Series detail + `1fr 340px` sidebar, Watchlist kanban with
HTML5 drag-and-drop (+`<select>` keyboard fallback), Notifications (filter tabs + kind icons),
Search, Account (settings shell with **Appearance fully wired**: theme/accent/density/cover
persisted to `localStorage` + OS `prefers-color-scheme` fallback; other panels are honest
`TODO(api)` stubs), 8-tab operator Console (Overview / Live scans / Providers / Challenge &
solver / Adapter test / Merge / Users / Audit), and the reskinned Auth card. Typed `gloo-net`
API client; in-memory token + silent refresh; role-gated Console; live unread badge via
`live.rs` `EventSource` → `/v1/me/stream`. Quality floor held: `:focus-visible` rings,
`prefers-reduced-motion` degrade, skeletons/empty/error states, optimistic watchlist moves,
responsive rail-collapse + 2-up grid reflow. **Verified this session:** `cargo check --target
wasm32-unknown-unknown` clean and `dx build --release --platform web` produces a working bundle
with all 8 fonts + `main.css` copied into `.../public/assets/` (the `wasm-opt failed` log on
Windows is the known non-fatal size-pass issue, not an app error). **Every backend gap is a
visible, honest `TODO(api)` stub — never a fabricated value.** Full per-screen status and the
build/verify recipe live in the frontend-only handoff tracker `docs/frontend/PROGRESS.md`.
**F6 backend enrichment — DONE.** The additive `docs/frontend/IMPLEMENTATION_PLAN.md` §9.1–9.5
endpoints now ship in `services/api` + `crates/db` (runtime-checked SQL; migration
`0009_account.sql`): server-side series filter/sort/paginate on `GET /v1/series` (total/next via
`X-Total-Count`/`X-Next-Cursor` headers, body still `SeriesSummary[]`), `SeriesDetail` enrichment
(`alt_titles`/`tags`/`is_primary`) + auth-scoped `ChapterDto.read`, `/v1/me/{continue,
recommendations,stats}` + a watchlist that embeds title/cover/progress/unread (kills the N+1),
public `GET /v1/providers`, Account (`PATCH /v1/me/profile`, `GET/DELETE /v1/me/sessions`,
`GET/PUT /v1/me/notification-prefs`; 2FA deferred), and Console `GET /v1/admin/users` +
`POST /v1/admin/providers/{id}/resolve` (live-scan SSE already existed). **F6 frontend rewire —
DONE (Session 7):** every screen now consumes its matching endpoint — Discover filters/sorts/
paginates server-side (`api::list_series_filtered` + `X-Total-Count`/`X-Next-Cursor`) with a
provider facet from `/v1/providers`; Series renders `alt_titles`/`tags`/`is_primary` + per-chapter
read-state; Home shows continue/stats/recommendations; Account edits profile + manages sessions +
notification-prefs; Console lists `/v1/admin/users` and wires Re-solve. `cargo check --target
wasm32-unknown-unknown` clean. The only honest stubs left are features with no endpoint at all
(AniList sync, series "related", 2FA).

### Infra
| Item | Status | Notes |
|---|---|---|
| Migrations (`migrations/`) | ✅ | 9 files (`0001_extensions` … `0008_scan_task_dedup`, `0009_account`), apply cleanly on Postgres 16. `0009` adds `users.notification_prefs jsonb` for the frontend §9.4 account settings. |
| Dockerfiles / `docker-compose.yml` | ✅ | `deploy/docker/Dockerfile` (parameterised cargo-chef + distroless) builds any backend via `--build-arg BIN`; the optional `render` tier uses the extra `runtime-browser` target (Debian slim + Chromium). **`deploy/docker/Dockerfile.frontend`** builds the Dioxus WASM SPA + serves it via nginx (`frontend.nginx.conf`), reverse-proxying `/v1/*`→`api`. `deploy/docker-compose.yml` runs the **full E2E stack**: Postgres/Redis/NATS/FlareSolverr + migrate/seed + every backend service + the frontend (front door on `:3000`). Redis is wired to `control-plane` (leader election); NATS is healthchecked. **Frontend image build + serve + `/v1` proxy verified this session; backend images not rebuilt.** k8s/Helm still pending. |
| CI | ✅ | `.github/workflows/ci.yml`: parallel `fmt --check`, `clippy -D warnings`, `cargo test --workspace`, `wasm32` frontend check, `cargo-deny` (`deny.toml`), `cargo-audit`, and a `docker build` matrix over every service `BIN`. |
| Config | ✅ (env) | Services are configured via `TANKOVAULT_*` env in compose; no standalone sample TOMLs. |

---

## 3. Schema validation

All migrations applied cleanly to a throwaway `postgres:16-alpine` (Session 1). The new
`sync` repo SQL (`external_accounts`, `sync_mappings`) matches `0005_external_sync.sql`
(bytea ciphertext token columns) and was validated by inspection. In-repo DB integration
tests (via `sqlx`'s test harness against a disposable database) remain the priority
pickup (§6) that would exercise all repo SQL — including enum text-casts, `xmax = 0`
new-chapter detection, SKIP LOCKED claim, and trigram candidate lookup — against a live
PG16.

---

## 4. Deviations from the spec (with rationale)

1. **UUIDv7 defaults.** Schema `DEFAULT`s use `gen_random_uuid()` (v4) as a fallback;
   production ids are always app-generated **v7** (`uuid` crate) and passed explicitly.
   Avoids requiring the `pg_uuidv7` extension. Sortability preserved by real inserts.
2. **SQLx runtime queries** instead of compile-time macros (§1) — same "no ORM, full SQL
   control" intent without a build-time DB dependency.
3. **OTLP export deferred.** `observability` ships local `tracing` + Prometheus now;
   collector export is a Phase-5 hardening task. `TelemetryConfig` already carries
   `otlp_endpoint`.
4. **`argon2 = 0.5`** (latest stable; 0.6 is pre-release only).
5. Added `notification_dedup`, `merge_candidates`, and `audit_log` tables (`0007`) that
   the spec text (§10, §16) requires but the §6 SQL block omitted.
6. **Sync token encryption lives in `auth::SecretBox`** (AES-256-GCM), not a standalone
   crate — it is a security primitive that belongs with the other §16 primitives and is
   reusable for any future encrypted-at-rest secret.
7. **Sync service owns its own HTTP contract** (`/v1/anilist/*`); the API's user-facing
   `/v1/me/sync/anilist/*` routes proxy to it (the API injects the authenticated `user_id`
   into each internal call).
8. **Canonicalisation runs inside `ingest_series`** (`db` depends on `matcher`) rather than
   being orchestrated by the worker, so candidate lookup + create are one atomic step.
   Concurrent first-creation of the same title across providers can still create two
   series; the merge queue + re-scan Attach path converge those.

---

## 5. Key invariants being upheld (design Appendix A)
- Relative paths stored; single tested `resolve_link` resolver (`domain::link`).
- No image/content column or fetch path anywhere.
- Idempotent worker writes (`ON CONFLICT`) — `catalog::ingest_series`; sync mappings and
  accounts are likewise upserts.
- Domain crate is persistence-free; enums cross the DB boundary via `::text` casts.
- External tokens are **never stored in plaintext** — sealed by `SecretBox` before the
  `db::repo::sync` layer ever sees them.
- Whole workspace is `rustfmt`- and `clippy -D warnings`-clean (DoD §21).

---

## 6. Pick up next (ordered)

1. **`web/frontend/` — TankoVault redesign is DONE (frontend F0–F5).** The full mockup
   (`docs/frontend/DESIGN_SPEC.md`) is implemented and shipping: all 9 screens, the Tailwind
   CLI build, tokens/icons/self-hosted fonts, kanban drag-between-columns, the Account shell +
   wired Appearance knobs, and the 8-tab operator Console. Per-screen status + the build/verify
   recipe live in `docs/frontend/PROGRESS.md`. **F6 backend is now DONE** — the additive
   `docs/frontend/IMPLEMENTATION_PLAN.md` §9.1–9.5 endpoints shipped in `services/api` +
   `crates/db` (server-side Discover filter/sort/paginate, `SeriesDetail`/`ChapterDto` enrichment,
   `/v1/me/{continue,recommendations,stats}` + embedded watchlist title/cover, public
   `/v1/providers`, Account profile/sessions/notification-prefs, Console `/v1/admin/users` +
   provider re-solve). **F6 frontend rewire is now DONE (Session 7):** every screen consumes its
   matching endpoint (Discover server-side filtering + provider facet, Series read-state/alt-titles/
   tags/primary-source, Home continue/recs/stats, Account profile/sessions/notification-prefs,
   Console Users list + Re-solve); `cargo check --target wasm32-unknown-unknown` is clean. Only
   endpointless features remain (AniList sync, series "related", 2FA). Still open elsewhere:
   scan-progress SSE still DB-polls (relay NATS `scan.progress`), frontend tests, and generating
   the client DTOs from `contracts` to replace the hand-mirrored `models.rs`.
2. ~~**Control-plane:** progress aggregator + Redis leader election.~~ **DONE (Session 5).**
   The aggregator now finalises runs (`finalize_if_complete`) and republishes one terminal
   `scan.progress` over NATS; the singleton scheduler is guarded by a Redis `SET NX PX`
   lease with `GET`/`PEXPIRE` renewal (fails open without Redis). Run counters were already
   aggregated DB-side by `scans::complete_task`/`fail_task`. Remaining polish: the API SSE
   could subscribe to the relayed NATS events instead of DB-polling.
4. **In-repo DB integration tests** (§3) using `sqlx`'s test harness against a disposable
   PG16 — exercises every repo (canonicalisation `resolve_canonical_series`, `merge_series`,
   `feed`, `sync`, SKIP LOCKED, `xmax = 0`).
5. ~~**CI pipeline:** wire fmt + clippy(`-D warnings`) + `cargo deny` + `cargo audit` +
   tests + `docker build` of each `BIN`.~~ **DONE (Session 8).** `.github/workflows/ci.yml`
   runs those as parallel jobs plus a `wasm32` frontend check; `deny.toml` configures the
   license/advisory/source/ban gates. `xtask` was completed in the same session (`reset`
   command). Remaining polish: the pipeline is unproven against GitHub's runners (authored,
   not yet executed on CI infrastructure).
6. ~~**`services/render`:** `chromiumoxide` headless render for JS-rendered listings + as a
   fallback `ChallengeSolver` back-end.~~ **DONE (Session 7).** `POST /v1/render` returns the
   rendered DOM/cookies/UA; `POST /v1/solve` implements the `ChallengeSolver` contract as a
   FlareSolverr-free back-end. Chromium-provisioned `runtime-browser` Docker stage + compose
   wiring. Remaining polish: solved-session write-back to Redis (today the caller/`challenge-
   solver` owns caching) and a per-provider render rate-limit inside this tier.
7. ~~**Notifier external channels** + **live WS/SSE push to connected clients.**~~ Channels
   (webhook + Discord + email/SMTP) landed earlier; **live push landed Session 9** — the
   notifier publishes a core-NATS `UserNotification` per new in-app row and the API relays
   it over `GET /v1/me/stream` (SSE) to the frontend's live unread badge. Remaining polish:
   per-user *channel* prefs (webhook/Discord/email are still operator-level/global, which
   would need a new prefs table) and pushing the richer feed row (not just the badge count)
   into the client on the same stream.
8. **k8s/Helm chart** (design §19) with HPAs and HTTP health probes.

---

## 7. The sync service (AniList) — how it works

- **Modules:** `mapping` (pure status/conflict logic, fully unit-tested), `anilist`
  (OAuth2 + GraphQL client; pure `parse_media_list` is tested, network methods are not),
  `engine` (orchestration over `db` + `matcher` + `SecretBox`), `main` (Axum contract).
- **Contract:** `GET /v1/anilist/authorize-url`, `POST/DELETE /v1/anilist/link`,
  `POST /v1/anilist/pull`, `POST /v1/anilist/push`, plus `/health` `/ready`.
- **Linking:** `exchange_code` → seal access/refresh with `SecretBox` →
  `db::repo::sync::upsert_account` (ciphertext). `access_token()` decrypts and, if expired
  with a refresh token present, refreshes first.
- **Series ↔ media resolution:** existing `sync_mappings` first; else trigram
  `find_candidates` + `matcher::decide` (pull) or an AniList title search (push). Resolved
  ids are cached back into `sync_mappings`.
- **Reconciliation:** `reconcile_progress(local, remote, policy)` returns the agreed
  progress, the authoritative side, and which side to write. Pull applies `update_local`
  (and adopts the remote status, preserving each user's `notify`); push applies
  `update_remote`. Policy is `local_wins | remote_wins | newest_wins` (default newest).
- **Rate/robustness:** a per-client pacer floors the gap between AniList requests
  (~700 ms default) and GraphQL calls retry once on `429` (honouring `Retry-After`).
- **Config keys:** `anilist.client_id/client_secret/redirect_uri/token_encryption_key`
  (base64 32-byte), optional `graphql_url`, `oauth_base`, `default_conflict_policy`,
  `min_request_interval_ms`.

---

## 8. Notes / gotchas for the next session
- SQLx 0.9 rejects non-`'static` SQL strings (`SqlSafeStr`). Composed queries use
  `macro_rules!` column lists + `concat!` to stay static — follow that pattern; do not
  reintroduce `format!`-built SQL.
- Typed id newtypes are `Copy`; `as_uuid(self)` takes self by value (works with `.map`).
- Multi-statement repo fns take `&mut sqlx::PgConnection` and reborrow `&mut *conn`;
  single-statement fns are generic over `impl PgExecutor`.
- Binary-crate internal modules use `pub(crate)` (not `pub`) to satisfy `unreachable_pub`;
  see `services/*/src/*.rs`.
- Workspace lints: the `rust_2018_idioms` **group** must sit at `priority = -1` in
  `[workspace.lints.rust]`, otherwise `cargo clippy` refuses to run. Test modules that
  assert exact, exactly-representable float values carry a scoped
  `#![allow(clippy::float_cmp)]`.
- A throwaway test DB container `tankovault-pg-test` (host port 55432) may still exist from an
  earlier session; remove with `docker rm -f tankovault-pg-test`.
- The frontend Docker build logs a **non-fatal** `wasm-opt failed ... (unsupported version of
  DWARF)` warning: the `dx`-bundled `wasm-opt` can't parse the release build's debug info, so
  it is skipped and the **unoptimised** wasm is shipped (~1.4 MB, functional). Harmless for
  local E2E; to shrink for prod, strip DWARF / disable debug in the wasm build.
- The frontend crate's package name is `tankovault-frontend`, so `dx` emits the bundle under
  `target/dx/tankovault-frontend/release/web/public` (not `.../tankovault/...`). The Dockerfile finds
  it by `-path '*/web/public'` rather than hardcoding the app name.
- nginx `/v1/*` proxy relies on the compose network's embedded DNS (`127.0.0.11`); it only
  resolves `api` **inside** the compose network, not via a bare `docker run`.
