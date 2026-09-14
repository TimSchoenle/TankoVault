# Query latency, September 2026 — local measurements (Phase 0)

Companion to [`2026-09-prod-diagnostics.sql`](2026-09-prod-diagnostics.sql). These numbers come
from a local catalogue, **not production**. They say which statements are expensive by shape and
how much a cold cache costs; they do not reproduce the 6–30 s production latencies, and nothing
here should be read as a production measurement.

## Environment

| | |
|---|---|
| Database | `STRATEGY file_copy` clone of the local stack's catalogue (last crawled August 2026), migrated 42 → 56 with the committed `.up.sql` files, then `VACUUM (ANALYZE)` |
| Scale | 2 838 719 chapters (554 MB: 260 MB heap, 294 MB indexes), 53 872 series, 80 777 series_sources, 148 676 scan_tasks, 3 238 merge_decisions, 3 providers; database 1 128 MB |
| Measured user | 859 watchlist rows, no early-access opt-ins (production's user: ~550 rows) |
| Server | `pgvector/pgvector:pg18`, the compose file's settings: 1 GB container limit, `shared_buffers=256MB`, `effective_cache_size=768MB`, `work_mem=16MB`, `random_page_cost=1.1`, `jit=off` |
| Host | Docker Desktop / WSL2, NVMe, 48 GB VM; no concurrent write load |

**Cold** = `docker restart` (empties `shared_buffers`) plus `echo 3 > /proc/sys/vm/drop_caches`
in the VM (empties the page cache), then one execution. **Warm** = second execution immediately
after. **Generic** = `plan_cache_mode = force_generic_plan`, which is what a pooled sqlx
connection runs once it has executed a prepared statement five times and the generic plan's
estimate is not worse. **JIT** = the warm run repeated with `jit=on`. Statement text is the
`.sqlx` cache entry, parameters as the diagnostics file binds them.

## Per statement

| statement | cold ms (blocks read) | warm ms | generic ms | est. cost (custom / generic) | JIT with `jit=on` |
|---|---|---|---|---|---|
| `tracking::feed` | **1 444** (17 119) | **286** | 360 | 16 417 / 16 417 | no |
| `tracking::continue_reading` | 786 (6 914) | 34 | 32 | 12 952 / 12 952 | no |
| `tracking::me_stats` | 593 (4 918) | 33 | 32 | 9 629 / 9 629 | no |
| `watchlist::summary` | 508 (4 927) | 39 | 37 | 15 521 / 15 521 | no |
| `providers::list_public` | 132 (2 444) | 49 | 51 | 12 511 | no |
| `stats::system_overview` | 998 (35 819) | **649** (17 516) | 668 | **185 113** | **yes** (23 ms) |
| `stats::provider_stats` | 309 (35 579) | 345 (32 176) | 300 | 96 183 | no (just under 100 000) |
| `browse::count_filtered`, unfiltered | 27 (7 659) | 14 | 18 | 8 329 / **1 972 741** | generic plan: yes |
| `browse` page, recency, unfiltered | 16 | 0.14 | 0.16 | 179 / **493 559** | generic plan: yes |
| `matching::list_merge_decisions` | 6.6 | 0.78 | **19.4** | 72 / 750 | no |
| `… flagged only` | 0.11 | 0.03 | 0.83 | 7 / 750 | no |
| `scans::scan_summary`, no window | 236 (2 145) | 9.2 | 10.8 | 2 520 | no |
| `scans::provider_scan_health` | 246 (2 161) | 24 | 27 | 9 574 | no |
| `scans::active_run_activity` | 125 (4 168) | 74 | 83 | 14 181 | no |
| `scans::failed_tasks_filtered` | 281 (2 152) | 18 | 15 | 3 664 | no |
| `scans::failure_groups` | 272 (2 150) | 39 | 36 | 5 729 | no |
| `scans::list_runs_filtered` | 2.5 | 0.85 | 1.2 | 188 | no |

"Blocks read" is `Buffers: shared read` at the plan root: 8 KiB pages that were not in
`shared_buffers`. Raw plans for every run are kept out of the repository; the diagnostics file
reproduces them.

## Home burst: the five statements concurrently

Fired together, as the ~14-minute refresh does.

| statement | cold exec ms | warm exec ms |
|---|---|---|
| feed | 1 256 | 338 |
| continue_reading | 437 | 41 |
| watchlist_summary | 295 | 45 |
| me_stats | 282 | 41 |
| providers_public | 113 | 62 |

## What this shows

1. **Cold cache costs 10–20× on the reading surfaces** (continue_reading 34 → 786 ms, me_stats
   33 → 593 ms) at identical plans. That is pattern 1 of the brief — variance at constant row
   count is cache state — and it holds without any write load. It does **not** explain 30 s on
   its own: locally the worst cold Home statement is 1.4 s on an idle NVMe-backed instance whose
   whole database (1.1 GB) nearly fits the container. Production's gap to that is some
   combination of a larger database, the crawler's concurrent writes evicting the working set,
   slower storage, and pool/connection contention; the production diagnostics are what separate
   them. Not reproduced locally.
2. **`stats::system_overview` and `stats::provider_stats` are never warm.** Their warm runs still
   read 17 516 and 32 176 blocks: they aggregate all of `chapters` (554 MB) through 256 MB of
   `shared_buffers`, so every execution goes back to the page cache — which, inside a 1 GB
   cgroup, competes with Postgres's own memory. The overview's estimate (185 113) is past
   `jit_above_cost`; with production's `jit=off` that costs nothing today, but the cost is the
   whole-table count, which grows with the data (pattern 4 confirmed).
3. **`feed` is the one Home statement slow warm.** The `read_progress` join sits above the
   chapter scan, so the unread predicate is a `Filter` after reading every chapter of every
   watched source (325 617 rows in, 97 252 kept), then two window aggregates sort those rows
   with an **external merge spilling 20 MB to disk** (`work_mem=16MB`). Under IO starvation the
   spill is paid again on every call. Its shape is not index-only by design (it returns titles
   and paths), but it reads the whole watched catalogue where it needs only the newest 100.
4. **Generic plans matter for the admin surfaces.** sqlx keeps prepared statements per
   connection, so after five executions a connection may switch to the generic plan.
   `list_merge_decisions`' generic plan seq-scans `merge_decisions` and evaluates the
   `jsonb_each(undo)` lateral for **every row** before the sort and `LIMIT` (3 238 laterals for
   50 rows returned: 0.78 → 19.4 ms here, linear in the table and in `undo`'s TOAST size). That
   matches production's "1–30 s, often 0 or 1 rows" far better than the custom plan does. The
   browse count and page generic estimates (1.97 M, 494 k) are the `$n IS NULL OR …` arms
   costed without values; they run fast here but would JIT if `jit` were ever re-enabled.
5. **The unread predicate's four copies are cheap warm (32–39 ms each) and 0.3–0.8 s cold.**
   Four of them per Home load quadruples the cold-read cost rather than the CPU cost (pattern 3
   confirmed as a multiplier, not as the root cause).
6. **Scan triage is small here** (148 k tasks). Its cold cost is dominated by reading
   `scan_tasks` whole; production's 10–30 s is consistent with unbounded history, which the
   diagnostics' row counts will confirm or refute.

## Other findings from reading the code

- The slow log's **29.9 s / 0-row entries are cancellations, not completions.** sqlx logs from
  `QueryLogger::drop`, and `TANKOVAULT_SECURITY__REQUEST_TIMEOUT_SECS` defaults to 30. The request
  timeout drops the future; nothing sends a cancel to Postgres, so the backend keeps executing
  until it next writes to the closed socket — the load outlives the request.
- **No `statement_timeout` exists anywhere.** `DatabaseConfig::acquire_timeout_secs`' doc comment
  says "Statement/acquire timeout", but `crates/db/src/pool.rs` only passes it to
  `acquire_timeout`.
- The pool sets no `min_connections`, `idle_timeout` or `max_lifetime` (sqlx defaults: 0, 10 min,
  30 min). With `min_connections = 0`, a quiet API closes idle connections and the next request
  resolves `postgres` through Docker DNS again — a plausible path for the one "failed to lookup
  address information" 500, unconfirmed. Phase 1 item 4; not changed.
- Pattern 2 (token refresh re-rendering Home) is **not yet verified** against the frontend code.

## Not done in Phase 0

- No production data: nothing above is a production number.
- No write load was simulated during the cold runs.
- Recsys vector retrieval, catalogue maintenance and `DELETE FROM providers` were not measured.
