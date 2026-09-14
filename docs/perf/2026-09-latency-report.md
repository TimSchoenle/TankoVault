# Query latency, September 2026 — progress report

What was done locally, what was measured, and what is still open. Production has not been
measured yet: [`2026-09-prod-diagnostics.sql`](2026-09-prod-diagnostics.sql) is waiting to be
run, and several decisions below depend on its output. Every number here comes from the local
catalogue clone described in [`2026-09-local-measurements.md`](2026-09-local-measurements.md):
2.84 M chapters, and for the admin surfaces an amplified copy with 102 providers, 274 000 failed
scan tasks and a 100 378-row merge journal. "Cold" means a Postgres restart plus a dropped VM page
cache.

## Phase 1 — infrastructure and hygiene

### 1.1 Postgres observability (done) and sizing (waiting on you)

The compose Postgres now preloads `pg_stat_statements` and `auto_explain` (plans over 2 s, with
rows and buffers) and sets `track_io_timing`. `deploy/README.md` documents how to read them.
**Memory limits are unchanged.** The proposed starting point is a 4 GB limit,
`shared_buffers=1GB`, `effective_cache_size=3GB` and `work_mem=32MB`. `feed` spills a 20 MB sort
at today's 16 MB. It waits on the production relation sizes and your hardware budget.

### 1.2 Statement lifetime (done)

| finding | fix |
|---|---|
| sqlx cannot cancel a statement. A request cut off by the 30 s timeout returned its connection to the pool mid-statement, and the next caller waited for it; the slow log's 29.9 s / 0-row entries are these. | Every pool closes a released connection that cannot answer a readiness probe within 1 s, and sets `client_connection_check_interval=2s` so the backend aborts the orphaned statement. `pool_release.rs` fails without either piece. |
| No `statement_timeout` anywhere. | API readers' routes run under 5 s, `/v1/admin` and background sweeps under 15 s on a separate pool. A cancelled statement answers `504 upstream_timeout` at WARN. |
| The two console rollups would have been cancelled by the 15 s ceiling on every refresh. | Their stale-while-revalidate refresh runs in a transaction with a local 120 s ceiling. |

### 1.3 Scan history retention (done)

The scheduler leader deletes settled runs older than
`scheduler.scan_history_retention_days` (default 30, confirmed) and their tasks. It works
in 5 000-row batches, spends at most 20 s per hourly pass, and always keeps the newest 32 finished
runs of each provider and tier, because the failure backoff reads them. On the plan-audit fixture
(300 000 tasks over 30 days) a catch-up to a 7-day retention deleted 230 000 tasks in 3.3 s of
batched passes, about 70 000 per second locally. That puts the 20 s budget at roughly 1.4 M task
rows per hourly pass, so partitioning is not needed unless production's ingest rate is far above
that.

### 1.4 Pool and the DNS failure (findings only, nothing changed)

- sqlx retries opening a connection only on `ConnectionRefused` or a transient database error. The
  logged `failed to lookup address information: Try again` is an I/O error of another kind, so it
  fails the request immediately.
- The pools use sqlx's defaults: `min_connections` 0, `idle_timeout` 10 min, `max_lifetime` 30 min.
  An idle API replica therefore drops to zero connections, and each reconnect resolves `postgres`
  through Docker's DNS again. The release probe from 1.2 adds a reconnect for every abandoned
  statement, which is the case it exists for.
- Not clearly wrong, so unchanged. Options, in order of preference:
  - keep a small warm floor (`min_connections` 2 on the API pools);
  - retry an acquire once on a name-resolution error;
  - check the Docker host's DNS if lookups fail under load, which points at the host rather than
    the pool.

## Phase 2 — duplicate Home work

### 2.1 Token refresh refetching every screen (done)

Confirmed. Every `use_resource` read the raw token signal through `api.client()`, so the silent
refresh (every ~14 minutes) restarted every resource on screen. On Home that was feed, continue
reading, stats, watchlist summary and the provider list in one burst per reader.

The session now derives an identity memo from the token's `sub`, and requests subscribe to it
rather than to the token. A renewal refetches nothing; sign-in, sign-out and a change of account
still refetch. Capabilities and the unread badge keep the renewal cadence on purpose. The pinning
test fails against the old subscription.

### 2.2 Redis cache (not built)

Recommended against in [`UNREAD_DENORMALISATION.md`](UNREAD_DENORMALISATION.md) §11: its
invalidation matrix is Phase 3's writer inventory maintained by hand, and it would still pay the
full computation on every miss.

## Phase 3 — stored unread state (done)

Design, deviations and full numbers: [`UNREAD_DENORMALISATION.md`](UNREAD_DENORMALISATION.md).
Migration 0058 adds `watchlist_unread`, one row per (reader, watched series), kept true by twelve
statement-level triggers. It is backfilled in the migration. The control-plane leader runs two
new passes:

- a 60 s sweep for rows whose early-access unlock time has passed;
- a 15-minute, 500-row reconciler that repairs and counts drift
  (`watchlist_unread_drift_total`).

Every Home and watchlist count now reads the stored row. The feed was restaged separately,
because stored counts do not help it.

| statement | cold before → after | warm before → after |
|---|---|---|
| `continue_reading` | 1 501 → 76 ms | 35.4 → 1.1 ms |
| `me_stats` | 911 → 2.5 ms | 34.9 → 0.31 ms |
| `watchlist::summary` | 791 → 155 ms | 41.6 → 7.0 ms |
| `feed` | 2 470 → 1 207 ms | 369 → 119 ms, no disk sort |
| watchlist page (released / progress) | 1 604 / 1 614 → 427 / 331 ms | 74 / 136 → 13 / 9.6 ms |
| watchlist counts / groups | 606 / 1 249 → 206 / 182 ms | 31 / 49 → 6.9 / 5.1 ms |

- **Outputs.** All eight return identical rows in identical order on the clone.
- **Estimates.** No estimate reaches the JIT threshold.
- **Write cost.** A watched chapter batch or a progress write costs about +8 ms per watcher; an
  unwatched batch costs +3 ms.
- **Backfill.** 859 rows took 274 ms. It holds the writers' locks for that long, so the
  production `watchlist_entries` count from the diagnostics should be checked before deploying
  (§9 of the design has the split path above ~20 000 rows).

The proposal's two-phase feed rewrite turned out not to be exact and was not built (§8).

## Phase 4 — admin and catalogue aggregates

| statement | change | before (warm / cold) | after (warm / cold) | est. cost before → after |
|---|---|---|---|---|
| `providers::list_public` | index `series_sources (provider_id, series_id)`, migration 0057 | 37.1 ms / 343 ms | 6.2 ms / 14 ms | Bitmap Heap Scan → Index Only, 0 heap fetches |
| `scans::provider_scan_health` | two grouped passes instead of a lateral per provider (identical rows and order) | 70 ms / 423 ms (174 ms JIT with `jit=on`) | 35 ms / 54 ms (no JIT) | 1 530 000 → 17 800 |
| `matching::list_merge_decisions`, flagged only | admin pool plans with actual parameters (`force_custom_plan`) | 48.4 ms / 506 ms | 0.03 ms / 0.08 ms | — (generic → custom) |
| `matching::list_merge_decisions`, one series | same | 50.3 ms warm | 0.66 ms warm | — |
| `scans::scan_summary` | rewrite tried and **rejected** | 32 ms unfiltered, 0.07 ms per provider | 16 ms unfiltered, but 123 ms per provider | 17 600 → 5 970 / 1 135 → 1 354 600 |

Custom plans apply to every console statement, so the other `$n IS NULL OR …` filters benefit
too, the scan triage feeds included.

**Not changed, waiting on you:**

- `stats::system_overview` and `stats::provider_stats`. `cache.rs` records a decision to keep exact
  counts behind a stale-while-revalidate cache. Replacing that with `reltuples` estimates or a
  rollup updated at scan completion is your call. Locally the overview is 649 ms warm, never
  fully cached (17 516 blocks read even warm), at cost 185 000.
- Browse total: estimate, capped count or `has_more`. This changes the API contract and the pager
  UX.

## Phase 5 — guardrails

- `repo_query_plans` has a third rule, `whole-scan-of-growing-table`. It flags a sequential scan
  of `chapters` or `scan_tasks`, or an index scan of either with no index condition. The fixture
  now includes 300 000 scan tasks over 30 days. On its first run the rule found the five statements
  that read those tables whole. All five are budgeted with reasons, and three are marked as open
  decisions.
- `pg_stat_statements` and `auto_explain` stay in the deployment (1.1).

## Remaining slow statements, ranked

By expected production impact after these changes, highest first. The ranking is a judgment from
the log and local plans, not a production measurement.

1. **`feed`.** 119 ms warm, but still 1.2 s cold locally: it has to read the watched sources'
   unread tails from `chapters`, and stored counts cannot replace a list of chapters. Fix: memory
   sizing (1.1). The other Home surfaces no longer read `chapters` for counts (Phase 3); their
   remaining cold cost is the live source ranking, 0.15–0.4 s locally.
2. **Console header rollup and provider table.** Off the request path, but each refresh reads all
   of `chapters` every 30 s while a console is open. Fix: the estimate-or-rollup decision.
3. **Browse total** (1–30 s). Fix: the pager decision.
4. **Scan triage** (failure groups, failed task list, run list total). Bounded by retention and
   now custom-planned; not re-measured at production scale.
5. **Catalogue maintenance totals and list** (~3 s). They count `chapters` whole (budgeted in the
   plan audit); `reltuples` would do for a warning figure.
6. **Recsys vector retrieval** (1.1–1.5 s). Not investigated; an HNSW index is memory-bound, so
   this is likely the cache first.
7. **`DELETE FROM providers`** (1.07 s). Cascades through sources and chapters; rare and operator
   driven. Not investigated.

## Found in passing

- `xtask/src/prune_chapters.rs` still deletes by `chapters.id`, which migration 0055 dropped; it
  fails at runtime (it uses unchecked `sqlx::query`, so nothing compiles against it).
- `ScanRun` is published by two different Rust types under one schema name, and which one
  `openapi.json` shows depends on route registration order. The router split preserves today's
  output deliberately; the collision itself remains.
- The desktop frontend does not build on this Windows host: `windows-registry`/`windows-result`
  0.100 require rustc 1.95 against the pinned 1.94. The lockfile is untouched by this work.

## Gates

Run on this branch on 2026-09-14, Windows host, `CARGO_TARGET_DIR` on a short path:

| gate | result |
|---|---|
| `cargo run -p xtask -- ci` gates 1–12 (fmt, clippy, offline tests, doc tests, rustdoc, OpenAPI drift, config contract, repo-lint, frontend fmt/test/clippy/wasm) | pass |
| `xtask ci` gate 13, frontend desktop test | **fail, environment**: `windows-registry`/`windows-result`/`windows-strings` 0.100 require rustc 1.95 against the pinned 1.94. These Windows-only crates are not compiled on CI's Linux runners, and neither lockfile changed on this branch. |
| `xtask ci` gate 14, frontend desktop clippy | not run (the gate stops at 13) |
| `cargo test -p tankovault-db -p tankovault-api -p tankovault-sync --features integration` | 565 passed, 0 failed (after Phase 3). Two earlier attempts failed 7 and 127 tests, all with Postgres `53100 could not resize shared memory segment`: the reused test container's 64 MB `/dev/shm` was full of shared statistics for ~780 leftover `tv_test_*` databases. They were dropped and the suite re-run. This is a test-harness issue, not part of this change. |
| `web/frontend`: `cargo test --bin tankovault a_renewal_does_not_rerun…` | pass; fails against the previous subscription |
| `xtask openapi --check`, `xtask config-docs --check`, `xtask config-contract` | pass (`openapi.json` unchanged) |
| `xtask sqlx-prepare` against an empty migrated database | run for every changed query; only the entries those queries explain changed |
| `just regenerate` | run for all three config-surface changes |
| `xtask ci` after Phase 3 | gates 1–12 pass; gate 13 fails on the same `windows-registry` rustc 1.95 requirement |

Not run: the `docker` image jobs, `xtask notices` (no lockfile moved), and anything against
production.
