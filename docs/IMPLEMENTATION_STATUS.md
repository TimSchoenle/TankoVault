# TankoVault — Implementation Status & Handoff

This file tracks the build state of the system described in [`design.md`](./design.md).
Update it at the end of every coding session: mark what landed, and leave a precise
"pick up next" list so the next session starts without re-deriving context.

**Last updated:** 2026-07-27 (Session 18 — frontend migrated off nginx to a scratch-based axum server)

> **Session 18 — the frontend image dropped nginx for an axum static server on `scratch`.** The
> web edge is now `services/frontend` (`tankovault-frontend`, binary `frontend`): an axum app that
> serves the built Dioxus WASM bundle with SPA fallback to `index.html`, exposes `GET /healthz`,
> puts the baseline hardening headers (`X-Content-Type-Options`, `Referrer-Policy`,
> `X-Frame-Options`) on the app shell only, and reverse-proxies `/v1/*` to `api` via `reqwest`.
> The proxy streams responses (`bytes_stream`), sets no whole-request timeout and disables
> gzip/brotli decompression, so the SSE streams (`/v1/me/stream`, `/v1/admin/scans/stream`) flow
> through unbuffered and byte-for-byte; it forwards `X-Forwarded-For`/`X-Real-IP`/`X-Forwarded-Proto`
> (appending XFF like nginx's `$proxy_add_x_forwarded_for`) so the API's forwarded-header trust and
> rate limiter keep working. `deploy/docker/Dockerfile.frontend` now builds the WASM bundle **and**
> the static musl `frontend` binary, then ships both on a bare `scratch` image (numeric nonroot
> user, no shell) exactly like every other backend service — replacing the nginx runtime. Because
> nonroot cannot bind port 80, the server listens on **3000** (compose maps `3000:3000`, was
> `3000:80`); the `deploy/docker/frontend.nginx.conf` file is deleted. Config: `TANKOVAULT_BIND_ADDR`
> (default `0.0.0.0:3000`), `TANKOVAULT_FRONTEND__STATIC_DIR` (image sets `/srv/www`),
> `TANKOVAULT_FRONTEND__API_UPSTREAM` (default `http://api:8080`). Like the other `scratch`
> services it now carries no container healthcheck. Docs (`deploy/README.md`, `docs/design.md`,
> `docs/OPERATIONS.md`, the `CorsConfig` doc-comment) updated to drop the nginx references. Gates:
> the new crate is `clippy -D warnings`-clean and the workspace builds.

> **Session 17 — roles replaced by permissions; every feature put behind a runtime flag; user
> administration and the GDPR surfaces completed.** Four changes that are really one
> authorization/administration model, which is why they landed together.
>
> **1. Permissions replace roles.** The ordered `user < operator < admin` tier is *gone* —
> column, SQL enum, `UserRole` type and all. Authorization is now per-capability:
> `tankovault_domain::Permission` (24 capabilities in 8 groups), stored per user in
> `user_permissions`, with no ordering and no implication between grants. Two defects drove
> this: the tier could not express least privilege (merge-queue triage also handed over
> provider editing, scan triggering and every user's linked-account state), and
> `at_least(Operator)` said how privileged a caller must be rather than *what they may do*, so
> nothing connected an endpoint to the capability it exercises.
>
> Grants resolve **from the database on every authenticated request** rather than from a token
> claim. That is a deliberate cost — one indexed lookup — bought for immediate revocation,
> which a claim baked into a 15-minute token cannot offer. The access token now carries
> identity only, and a test asserts no authorization claim can creep back into it. Console
> presets (Reader/Operator/Administrator) expand to a checklist the administrator then edits
> and are never persisted; nothing in the database or in a decision knows presets exist.
>
> **2. Every feature is behind a runtime flag.** `tankovault_domain::Feature` — 37 features in
> 8 groups, each with a compiled default and an operator-facing description of what switching
> it off *does*. `feature_flag_overrides` stores only deviations, so an empty table is a working
> deployment and a new feature needs no migration. Crucially this is a **different mechanism
> from the §2 wiring toggles** and does not break their contract: a flag has to change without a
> redeploy, so it is consulted per request — but *declaratively*, via a `route_features()` table
> next to the route registration enforced by one middleware, plus a per-iteration check in the
> background loops that have no route to declare against. No handler contains flag logic.
> `admin.feature_flags` and `admin.users` are **locked**: disabling either would leave no
> in-band way to switch anything back on, so the API refuses and the runtime ignores a stored
> override. Flags are enforced in `api`, `control-plane` (scheduler sweeps), `notifier`
> (in-app/live/email/webhook/Discord) and `sync` (its own contract + the reconciliation loop).
>
> **3. User administration.** `GET /v1/admin/users` became a searchable, paged directory;
> `/v1/admin/users/{id}` gained detail, identity edits, suspend/reinstate, forced sign-out,
> administrative email confirmation, permission grant editing and erasure. Suspension is an
> identity-level state (`users.status`), *not* the absence of permissions — an account with no
> grants can still read its own watchlist, which is not what suspending it means; it is checked
> before authorization and also ends live refresh sessions. Three guard rails live in code
> because no constraint can express them: no self-administration, the last active holder of
> `users.permissions` cannot be revoked/suspended/erased, and erasure demands the username back.
> All three refusals are audited.
>
> **4. GDPR.** Self-service export and erasure were already there; erasure now also closes the
> caller's own open requests so the compliance record is not left looking abandoned. New
> alongside them: a tracked data-subject request queue (`gdpr_requests`) covering the rights the
> two endpoints cannot — Art. 16/18/21 need a human, Art. 12(3) needs a deadline on a tracked
> object, Art. 5(2) needs a durable record. Recording an outcome and performing it are separate
> endpoints with separate permissions (`privacy.export` is split from `privacy.write` because it
> is the one action that discloses another person's whole record). The queue stores **no
> snapshot of the subject's email or username**: `user_id` is `ON DELETE SET NULL`, so a
> completed erasure leaves an accountability record that is no longer personal data.
>
> **Two collisions the generated client caught, both real bugs:** `tankovault_domain::
> AccountStatus` silently clobbered `tankovault_contracts::sync::AccountStatus` in the `OpenAPI`
> components (the latter is now published as `SyncAccountStatus`), and `#[serde(flatten)]` on
> `AdminRequestRow` produced a schema with *no properties*, so the typed client lost every field
> the queue renders. Both fixed; `#[serde(flatten)]` must not be used on a `ToSchema` type.
>
> **Frontend:** `Session::role` is gone, replaced by a `CapabilitySet` fetched from
> `GET /v1/me/capabilities` and keyed on the session token, so a grant change reaches the UI
> within one token lifetime. Rail entries, console tabs and individual buttons each declare the
> permission *and* feature they need. New Console tabs (Features, Privacy) and a rewritten Users
> tab; new Account "Privacy & data" panel (export, requests, deletion). 628 catalogue keys, en
> and de in lockstep.
>
> Gates: `fmt --check`, `clippy --workspace --all-targets --all-features -D warnings`,
> `cargo test --workspace` (**257 passing**, was 214), `openapi --check`, `sqlx-prepare --check`,
> and the `wasm32` frontend `fmt`/`clippy -D warnings`/`test` all clean. Migration `0018` was
> additionally applied from scratch to a throwaway database, and a **44-check end-to-end smoke
> test** against a live API + Postgres verified: the empty token claim set, immediate
> grant/revocation, per-feature 404s with the flag named in the body, locked-flag refusal,
> suspension blocking both sign-in and live sessions, last-administrator protection, and the
> full GDPR queue including operator-fulfilled erasure leaving a pseudonymised record.
>
> Two **pre-existing** breakages were fixed in passing: `services/api`'s lib tests called
> `RouteClassifier::classify` with its old one-argument signature (so `cargo test --workspace`
> did not build), and `clippy::unnecessary_wraps` failed on `MetricsConfig::default_listen`.

> Session 16 — production hardening of every backend service. A new shared crate
> `crates/service` (`tankovault-service`) now owns every cross-cutting concern; the old
> `crates/observability` was folded into it and **deleted**. What it provides, and what it
> replaced:
>
> | Concern | Before | Now |
> |---|---|---|
> | Bootstrap | Each `main.rs` open-coded load-config → init-telemetry → connect-pool → bind → `axum::serve` | One shared runtime; `main.rs` is wiring only |
> | Shutdown | None. A container stop severed in-flight requests and background loops mid-write | One `CancellationToken` drives the server *and* every spawned loop (`shutdown::every`) |
> | `/ready` | Literal `"ok"` in all 7 services — reported healthy with the DB gone | Concurrent, individually-timed dependency probes; `503` + per-dependency JSON body |
> | Inbound rate limit | **None anywhere** (`governor` was only outbound crawl politeness) | `RateLimitStore` trait, in-memory (`governor`) + Redis token-bucket (atomic Lua) backends, per-route-class budgets, `429` + `Retry-After` + RFC 9457 body |
> | CORS | `CorsLayer::permissive()` on the public edge — any site could read a signed-in user's data | Explicit origin allowlist; **empty by default** (same-origin only) |
> | Other edge controls | None | Request-id, body cap (1 MiB), request timeout, `nosniff`/`DENY`/`no-referrer`/CORP, optional HSTS |
> | Metrics | 3 counters in 2 services, always on | Togglable (**recorder not installed when off**), `http_requests_total` / `http_request_duration_seconds` / `http_requests_in_flight` labelled by *matched route* so cardinality is bounded |
> | Audit | One private helper in `admin.rs`; successes only | `AuditSink` trait + Postgres/no-op sinks; `outcome` (`success`/`failure`/`denied`), `actor_ip`, `user_agent` columns (migration `0017`); **denials now recorded** — `AuthUser::require` audits refusals, and login failure / refresh-token-reuse are audited |
> | GDPR | Nothing | `GET /v1/me/export` (Art. 20, credential-redacted) and `DELETE /v1/me` (Art. 17, cascade + audit pseudonymisation via the existing `ON DELETE SET NULL`); configurable audit retention sweep (Art. 5(1)(e)) |
>
> **Toggles** are wiring decisions, never call-site branches: `metrics.enabled = false`
> installs no recorder at all, `audit.enabled = false` swaps in `NoopAuditSink`, and
> `rate_limit.enabled = false` leaves the layer unmounted. All three verified off and on
> against a live stack.
>
> **Restructuring:** `services/api/src/admin.rs` (1452 lines) → `admin/{providers,scans,
> system,merge,sync}.rs`; `me.rs` (1346) → `me/{watchlist,progress,notifications,dashboard,
> account,sync,privacy}.rs`. Pure code movement — the route table and `OpenAPI` document are
> unchanged apart from the two new GDPR paths. Re-exports are **globs** because `utoipa`'s
> `routes!` resolves a hidden `__path_<handler>` type per handler.
>
> **Two real bugs found and fixed while building this:** `RateLimitPolicy::capacity()`
> clamped the burst *up* to the sustained rate, so the shipped default of 300/min with a
> 60-deep bucket actually allowed 300 back-to-back requests; and `Health::report` gathered
> its probes sequentially while documenting them as concurrent.
>
> Gates: `fmt --check`, `clippy --workspace --all-targets --all-features -D warnings`,
> `cargo test --workspace` (**214 passing**), and the `wasm32` frontend check all clean,
> both online and with `SQLX_OFFLINE=true`.

> Session 15: **no code shipped** — a design pass on the two weakest areas of user tracking.
> New `docs/READING_PROGRESS_AND_SYNC.md` (RFC, proposed/not implemented) fully redesigns:
> **(A) local read tracking** — deliberately stays **scalar** (no per-chapter ledger table):
> splits the `read_progress` high-water-mark into two independent scalar frontiers,
> `last_read_whole_number` (renamed from `last_read_number`) and a new nullable
> `last_read_part_number`, fixing a real bug where marking a sub-chapter part (e.g. `152.3`)
> read would silently mark the later whole chapter `152` as read too once scanned in; a
> part-number write can now only ever advance the part scalar, never the whole one.
> `last_read_whole_number` is directly the external-service-shaped progress integer — no
> derivation needed. Also adds a per-series `sync_excluded` opt-out flag (+ optional
> per-provider override) so a title can be tracked locally without ever touching AniList.
> Explicitly accepts the same monotonic-frontier trade-off the as-built system already has
> (no arbitrary non-contiguous per-chapter marking) — this redesign fixes the whole/part
> conflation bug, it does not add ledger-style granularity. **(B) external sync** — moves
> `conflict_policy` from a process env var to a persisted per-`external_accounts`-row setting
> with a new `auto_sync_enabled` toggle; replaces the current two-way `reconcile_progress`
> (current-vs-current + timestamp) with a three-way merge against a `last_synced_*` snapshot
> on `sync_mappings` (distinguishes "only local changed" / "only remote changed" / "both,
> agree" / "both, disagree"); adds a fourth `ask_me` policy backed by a new `sync_conflicts`
> review queue, a user-facing `sync_history` log, and a new *scheduled* reconciliation loop
> (the `sync` service, control-plane-cron-shaped) alongside the existing reactive
> push-on-write — closing the gap where a change made directly on AniList's site never flowed
> back automatically. Added superseding pointers in `design.md` §6/§15 (as-built stays
> documented as such; the new doc's rollout plan is additive/phased, non-breaking). Pick up
> next: implement Part A first (a single column-rename-plus-add migration + the two-scalar
> repo rewrite + API + frontend per-chapter toggle — no backfill needed), then Part B
> (three-way merge + scheduler + settings/conflict UI) — see the doc's §2 rollout plan for the
> exact ordered steps.

> Session 14 landed: the external-sync feature was **massively improved** end to end — a
> generalized multi-provider architecture, an immediate per-chapter push to AniList, and full
> admin visibility. `services/sync` gained a new `provider.rs`: an `ExternalProvider`
> `#[async_trait]` (matching the existing `SourceAdapter`/`ChallengeSolver` dyn-trait
> precedent) with shared `OAuthTokens`/`Viewer`/`RemoteEntry` types (status crosses the
> boundary as the shared `WatchStatus`, never a provider-specific enum). `AniListClient` now
> implements it; `SyncEngine` holds a `HashMap<&'static str, Box<dyn ExternalProvider>>`
> registry instead of one hardcoded client, and every method (`link`/`unlink`/`status`/
> `pull`/`push`) takes a `provider` slug. AniList remains the only registered provider — the
> registry is a drop-in seam for a second one, not built speculatively. Routes moved from
> `/v1/anilist/*` to `/v1/sync/{provider}/*` (+ `GET /v1/sync/providers`); the API's proxy
> layer (`services/api/src/me.rs`) followed to `/v1/me/sync/{provider}/*` (+
> `GET /v1/me/sync/providers`). **New: `SyncEngine::push_series`** — a fast, non-reconciling
> targeted push (local state wins outright, no remote-list fetch) fanned out to every provider
> a user has linked, exposed as `POST /v1/sync/push-series`; the API's `put_progress` and
> `put_watchlist` handlers fire it via a best-effort `tokio::spawn` (`me::spawn_targeted_push`)
> after their local write commits — never blocking the response, logged-only on failure. This
> is what makes "mark a chapter read" reach AniList without a manual "Push" click. The Series
> page's chapter rows gained a real **Mark read / Mark unread** action (`PUT
> /v1/me/progress/:id`, wired to the existing but previously-unused `api::set_progress`) —
> previously the "Read" button only opened the external chapter URL (now relabelled "Open");
> mark-unread steps back to the *previous* rendered row's number, not `number - 1`, since
> chapter numbers aren't guaranteed contiguous integers. **New admin Console "Sync" tab**:
> linked-accounts + series-mappings tables with operator actions — force pull/push/unlink an
> account, clear a bad mapping — every mutation audited (`sync.pull`/`sync.push`/
> `sync.unlink`/`sync.mapping.clear`) via the existing `audit()` helper, mirroring
> `MergeQueue`'s pattern. Migration `0011_sync_generalize.sql` adds
> `external_accounts.last_error` (cleared on success, set on any pull/push/targeted-push
> failure — the admin table's only failure signal, deliberately not a full log table) and
> `sync_mappings.updated_at`. New `db::repo::sync` reads: `list_linked_providers`,
> `delete_mapping`, `admin_list_accounts`, `admin_list_mappings`; `tracking` gained
> `watchlist_status_get`. Frontend: the Account "Sync & integrations" panel is now
> provider-driven (`GET /v1/me/sync/providers` → one `ProviderSyncCard` per entry) instead of
> a hardcoded AniList block; the Watchlist "Sync now" quick action and the Series page's
> AniList status pill stay intentionally hardcoded to `"anilist"` (not generalized — no UI
> need for it yet). Also fixed two pre-existing `clippy::doc_markdown` failures (`TankoVault`
> without backticks in `crates/domain`/`crates/db` doc comments) that were blocking a clean
> `-D warnings` run, unrelated to this feature. Gates green: workspace `fmt`- and
> `clippy -D warnings`-clean (both `cargo fmt --check` and `clippy` scoped to every touched
> file — a large pre-existing import-ordering `fmt` drift across untouched files elsewhere in
> the repo, likely a local `rustfmt` version bump, was left alone rather than mass-reformatted);
> **135 tests green** (sync's own suite unchanged at 15 after the `RemoteEntry`→`AniListEntry`
> rename); wasm frontend `cargo check`/`clippy -D warnings` clean.

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
| `domain` | ✅ | Entities, typed UUIDv7 ids, enums, `resolve_link` (tested), title normalize, politeness+ceilings. **+ the two registries the whole system authorizes and switches against: `Permission` (24 capabilities, groups, presets) and `Feature` (37 flags, defaults, locked set)** — `UserRole` removed. 38 tests. |
| `config` | ✅ | Figment layered load (defaults → TOML → `TANKOVAULT_*` env). `ConfigError` boxes the large `figment::Error`. Tested. |
| `contracts` | ✅ | NATS subjects/streams + task/progress/chapter/provider-state messages. Tested. |
| `bus` | ✅ | `async-nats` JetStream client: stream provisioning, task publish/consume, chapter events. **+ core-NATS client** for non-durable live pushes (`publish_user_notification`/`subscribe_user_notifications`; `BusError::Nats`). |
| `service` | ✅ | **New (Session 16).** The shared production runtime: `init_tracing`, togglable `MetricsRegistry`, `install_shutdown` + `shutdown::every`, `Health`/`HealthCheck` (+`PostgresCheck`), `HttpStack` (request-id → trace → metrics → security headers → CORS → rate limit → timeout → body cap → compression), `ops_router`, `serve` with graceful drain, `RateLimitStore` (memory + Redis) and `AuditSink` (Postgres + no-op). **+ `flags`: the runtime `FeatureGate` (DB-backed snapshot + refresh loop) and the declarative `RouteFeatures` middleware.** Features: `db`, `redis`. 48 tests. |
| ~~`observability`~~ | — | **Deleted (Session 16)**; folded into `service`, which splits tracing from metrics so metrics can genuinely be switched off. OTLP export still deferred (§4). |
| `db` | ✅ | Pool, embedded migrations, repos: providers, catalog (`ingest_series` tx now runs the **matcher-driven `resolve_canonical_series`** + merge-candidate recording; `list_tags`), users+refresh, scans (SKIP LOCKED), tracking (+`progress_state`, `watchlist_set_status`, `feed`, `notifications_unread_count`), matching (trigram + **`merge_series`/`list_open_merge_candidates`/`dismiss_merge_candidate`**), audit, sync (external_accounts + sync_mappings). Depends on `tankovault-matcher`. Not yet DB-integration-tested in-repo (§3). |
| `fetch` | ✅ | `Fetcher` trait + decorator stack (robots → rate-limit → cache → solving → retry → base), **SSRF guard**, solver client. 12 tests. |
| `solver` | ✅ | `ChallengeSolver` trait, `detect_challenge` classifier, FlareSolverr back-end, fake solver for the swap test. 7 tests. |
| `adapters` | ✅ | `SourceAdapter` trait, `Ctx`, generic/Madara config adapter, custom `DemonicScansAdapter`, HTML helpers (number/status/date parsers, `relativize`/`absolutize`), `builtin_presets()` (demonicscans + manhuaus + kunmanga, solver-derived), factory slug dispatch. Fixtures per provider; unit + fixture tests. See [`PROVIDERS.md`](./PROVIDERS.md). |
| `auth` | ✅ | argon2id, HS256 access JWT (**identity claims only — no role or permission travels in a token**), rotating hashed refresh + reuse detection. **+ `SecretBox` (AES-256-GCM) for external tokens at rest (§16).** 15 tests. |
| `matcher` | ✅ | Pure scoring/decision bands (attach/ambiguous/create). First live consumer is the sync service. 5 tests. |

### Services (`services/`)
| Service | Status | Notes |
|---|---|---|
| `api` | ✅ | **Full §11 contract.** Auth (register/login/refresh/logout), series browse/detail/chapters (resolved links), `/v1/tags`, admin provider CRUD + `base_url` migration + trigger-scan + **`providers/:id/test`** (dry-run adapter via the injected fetch stack) + **`scans/stream`** (SSE, DB-poll), `merge-candidates` list/dismiss + `series/merge`, **operator dashboard reads** (`admin/stats`, `admin/providers/stats`, `admin/scans` list, `admin/scan-failures`, `admin/audit`), `/me` watchlist/progress/feed/notifications, and the **provider-keyed** `/me/sync/{provider}/*` proxy (+ `/me/sync/providers`). Now also depends on `adapters`/`fetch`/`solver` for the test endpoint. **+ `GET /v1/me/stream`** (SSE live notifications; token-in-query auth; core-NATS relay via `AppState.bus: Option<Bus>`, degrades to `503` if NATS is down). **+ admin Sync endpoints** (`admin/sync/accounts`, `admin/sync/mappings` list; `admin/sync/{pull,push,unlink}`, `admin/sync/mappings/clear` — all audited). **+ `spawn_targeted_push`**: `put_progress`/`put_watchlist` fire a best-effort background `POST {sync}/v1/sync/push-series` after their local write, so marking a chapter read reflects to every linked provider without a manual sync (design: immediate targeted push, §7). **+ Session 16:** handlers split into `admin/*` and `me/*` submodules; the shared `tankovault-service` stack (rate limit, CORS allowlist, security headers, request-id, body cap, timeout, togglable metrics/audit, real `/ready`, graceful shutdown); audited auth outcomes and authz denials; **GDPR `GET /v1/me/export` + `DELETE /v1/me`**; audit-retention sweep. **+ Session 17:** authorization is per-`Permission`, resolved from the DB each request (no role, no token claim); the declarative `route_features()` flag table + middleware; full user administration (`/v1/admin/users/*`: directory, detail, identity, suspend, sessions, grants, erasure) and `GET /v1/admin/permissions`; the feature-flag control plane (`/v1/admin/feature-flags`); the GDPR request queue (`/v1/me/privacy/requests`, `/v1/admin/privacy/requests/*`); and `GET /v1/me/capabilities`. See [`OPERATIONS.md`](./OPERATIONS.md). |
| `worker` | ✅ | One-shot inline full/fast scan **and** JetStream consumer (consumer-group scale); idempotent `ingest_series`, emits `chapter.discovered`. |
| `control-plane` | ✅ | Scheduler (interval sweeps) + planner (run→tasks→JetStream) + `/internal/scans` trigger. **+ progress aggregator** (`scan.progress` consumer → atomic `finalize_if_complete` → republished terminal event) **+ Redis leader election** (`SET NX PX` lock w/ `GET`/`PEXPIRE` renewal; `Leadership` flag gates every sweep; fails open to sole-leader without Redis, stands down on Redis error). 5 tests. |
| `notifier` | ✅ | Consumes `chapter.discovered`, fans out to watchers (partial-index lookup), dedup, in-app rows. **+ pluggable external channels** (`NotificationChannel` trait): config-driven generic JSON webhook + Discord incoming-webhook **+ email (SMTP via `lettre`, rustls/ring)**, delivered once per genuinely-new chapter (best-effort, failures logged not fatal). **+ live push**: after each in-app row it publishes a best-effort core-NATS `UserNotification` (with the fresh unread count) to `notify.user.<id>`, relayed to connected clients by the API's `/v1/me/stream`. Pure payload/message builders tested (13 tests). |
| `challenge-solver` | ✅ | `POST /v1/solve` HTTP contract, FlareSolverr-backed default, config-selected back-end. |
| `sync` | ✅ | **Generalized this session** to a multi-provider registry (`ExternalProvider` trait; AniList is the only registered implementation). OAuth link/unlink, pull (remote→local) and push (local→remote) reusing `matcher`; tokens encrypted at rest; user-selectable conflict policy (local/remote/newest-wins). **+ `push_series`**: a fast, non-reconciling targeted single-series push fanned out to every provider a user has linked (backs the API's immediate-push-on-mark-read flow). 15 tests. See §7. |
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
| Migrations (`migrations/`) | ✅ | 18 files (`0001_extensions` … `0018_permissions_flags_privacy`), verified to apply cleanly from scratch on Postgres 19. `0018` is the authorization/administration model: `user_permissions` (backfilled from the roles it replaces, then `users.role` and the `user_role` type are **dropped**), `users.status`/`suspended_at`/`suspension_reason`/`last_login_at`, `feature_flag_overrides`, and `gdpr_requests`. |
| Dockerfiles / `docker-compose.yml` | ✅ | `deploy/docker/Dockerfile` (parameterised cargo-chef; static **musl → `scratch`** runtime; digest-pinned bases, `--locked` + `SOURCE_DATE_EPOCH` for reproducibility) builds any backend via `--build-arg BIN`; the optional `render` tier uses the extra `runtime-browser` target (Debian slim + Chromium). **`deploy/docker/Dockerfile.frontend`** builds the Dioxus WASM SPA + serves it via nginx (`frontend.nginx.conf`), reverse-proxying `/v1/*`→`api`. `deploy/docker-compose.yml` runs the **full E2E stack**: Postgres/Redis/NATS/FlareSolverr + migrate/seed + every backend service + the frontend (front door on `:3000`). Redis is wired to `control-plane` (leader election); NATS is healthchecked. **Frontend image build + serve + `/v1` proxy verified this session; backend images not rebuilt.** k8s/Helm still pending. |
| CI | ✅ | `.github/workflows/ci.yml`: parallel `fmt --check`, `clippy -D warnings`, `cargo test --workspace`, `wasm32` frontend check, `cargo-deny` (`deny.toml`), `cargo-audit`, and a `docker build` matrix over every service `BIN`. |
| Config | ✅ (env) | Services are configured via `TANKOVAULT_*` env in compose; no standalone sample TOMLs. |

---

## 3. Schema validation

All migrations applied cleanly to a throwaway `postgres:19-alpine` (Session 1). The new
`sync` repo SQL (`external_accounts`, `sync_mappings`) matches `0005_external_sync.sql`
(bytea ciphertext token columns) and was validated by inspection. In-repo DB integration
tests (via `sqlx`'s test harness against a disposable database) remain the priority
pickup (§6) that would exercise all repo SQL — including enum text-casts, `xmax = 0`
new-chapter detection, SKIP LOCKED claim, and trigram candidate lookup — against a live
PG19.

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

0a. **Follow-ups left open by Session 17** (none blocking; the model is complete and verified):
   - **A permission-model integration test suite.** The 44-check smoke script that verified this
     session lives only in a scratchpad. It should become a checked-in integration test against
     a disposable PG19 (see §6.4) — grant/revoke visibility, suspension, last-administrator
     protection and the GDPR queue transitions are exactly the behaviours a refactor would break
     silently.
   - **Bootstrap for a deployment with no administrator.** `xtask seed` grants the demo admin
     every capability, and the migration converts existing roles, but an installation that
     somehow reaches zero active `users.permissions` holders has no in-band recovery. A
     `xtask grant <user> <permission>` command would be the escape hatch.
   - **Per-user notification channel preferences.** `notifications.{email,webhook,discord}` are
     deployment-wide flags; the long-standing wish for per-user channel prefs (§6.7) is
     unchanged by this work and would need its own table.
   - **Flag change history.** `feature_flag_overrides` keeps only the *last* change (who, when,
     why). Every change is in `audit_log` under `flag.set`/`flag.reset`, but the console does
     not surface that timeline next to the switch.

0. **Implement `docs/READING_PROGRESS_AND_SYNC.md` (Session 15 design, not yet coded).**
   Highest-priority pickup: Part A (split `read_progress.last_read_number` into scalar
   `last_read_whole_number`/`last_read_part_number` — no ledger table, just a rename + one
   added column — fixes the part-vs-whole-chapter read bug) should land before Part B
   (persisted per-account `auto_sync_enabled`/`conflict_policy`, three-way merge, scheduled
   reconciliation, `sync_excluded`) since Part B's progress push reads Part A's
   `last_read_whole_number` directly. Follow the doc's own §2 rollout plan (schema → Part A
   backend → Part B backend → frontend → cleanup).
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
   PG19 — exercises every repo (canonicalisation `resolve_canonical_series`, `merge_series`,
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

## 7. The sync service — how it works (generalized multi-provider, Session 14)

- **Modules:** `provider` (the `ExternalProvider` `#[async_trait]` contract + shared
  `OAuthTokens`/`Viewer`/`RemoteEntry`/`ProviderInfo` types — status crosses this boundary as
  the shared `WatchStatus`), `mapping` (pure status/conflict logic, fully unit-tested — still
  AniList-only: `AniListStatus` never leaves `anilist`), `anilist` (`impl ExternalProvider for
  AniListClient`; `AniListEntry` is the AniList-shaped wire type, converted to the shared
  `RemoteEntry` via `From`), `engine` (`SyncEngine` holds the provider registry + orchestrates
  over `db` + `matcher` + `SecretBox`), `main` (Axum contract + `build_providers` registry
  wiring). AniList is the only registered provider; a second one is a new `ExternalProvider`
  impl inserted into `build_providers`, no other wiring changes.
- **Contract:** `GET /v1/sync/providers`, `POST /v1/sync/push-series` (targeted push, always
  `200`), `GET /v1/sync/{provider}/authorize-url`, `GET /v1/sync/{provider}/status/{user_id}`,
  `POST`/`DELETE /v1/sync/{provider}/link`, `POST /v1/sync/{provider}/pull`,
  `POST /v1/sync/{provider}/push`, plus `/health` `/ready`.
- **Linking:** `exchange_code` → seal access/refresh with `SecretBox` →
  `db::repo::sync::upsert_account` (ciphertext, keyed by `provider` slug). `access_token()`
  decrypts and, if expired with a refresh token present, refreshes first.
- **Series ↔ media resolution:** existing `sync_mappings` first (keyed by `provider`); else
  trigram `find_candidates` + `matcher::decide` (pull) or a provider title search (push).
  Resolved ids are cached back into `sync_mappings`.
- **Full reconciliation (`pull`/`push`):** `reconcile_progress(local, remote, policy)` returns
  the agreed progress, the authoritative side, and which side to write. Pull applies
  `update_local` (and adopts the remote status, preserving each user's `notify`); push applies
  `update_remote`. Policy is `local_wins | remote_wins | newest_wins` (default newest). Both
  now record `external_accounts.last_error` on failure (cleared by the next success).
- **Targeted push (`push_series`, design: immediate targeted push):** the fast path behind
  "mark a chapter read" — reads local progress/status as-is (no remote-list fetch, no
  reconciliation: a direct user action is authoritative by construction), resolves/caches the
  external id, and calls `save_entry` for every provider the user has linked
  (`db::repo::sync::list_linked_providers`). Never fails its caller; every outcome is
  best-effort logged and written to `last_error`/`last_synced_at`.
- **Rate/robustness:** AniList's client paces requests (~700 ms default floor) and retries
  GraphQL once on `429` (honouring `Retry-After`) — a per-provider concern, not shared engine
  logic.
- **Config keys:** unchanged env-var shape, `anilist.client_id/client_secret/redirect_uri/
  token_encryption_key` (base64 32-byte), optional `graphql_url`, `oauth_base`,
  `default_conflict_policy`, `min_request_interval_ms` — kept as-is deliberately (no reason to
  touch deployed `TANKOVAULT_ANILIST__*` env names for a purely internal registry refactor).
- **Admin visibility (design: admin Sync console tab):** `db::repo::sync::admin_list_accounts`/
  `admin_list_mappings` back `GET /v1/admin/sync/{accounts,mappings}`; operators can force a
  pull/push/unlink for any user's linked account or clear a bad mapping
  (`POST /v1/admin/sync/{pull,push,unlink}`, `POST /v1/admin/sync/mappings/clear`), each
  audited. Frontend: Console → **Sync** tab (`SyncAdminPanel`, between Merge and Users).

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
