# Stored unread state (Phase 3)

Status: **implemented** in migration `0058_watchlist_unread`, `repo::tracking::unread` and the
control plane's unlock sweeper and reconciler. The decisions in §12 were approved as proposed,
with the in-migration backfill. Where the implementation departs from the proposal, the section
says so; §13 has the before/after measurements.

## 1. The problem, in numbers

Four Home statements (`me_stats`, `continue_reading`, `watchlist::summary`, and the watchlist page
and counts) each re-derive the reader's unread state from `chapters` for every watched series on
every request. The production slow log showed them at 1.3–30 s. Local measurements on a
2.84 M-chapter clone with an 859-row watchlist
([`2026-09-local-measurements.md`](2026-09-local-measurements.md)):

| | warm | cold |
|---|---|---|
| `me_stats` + `continue_reading` + `summary`, today | 33 + 34 + 39 ms | 593 + 786 + 508 ms |
| every stored value below, recomputed for all 859 rows | 165 ms (est. cost 39 k, no JIT) | — |
| the same figures read back from a stored row per series | **0.29 ms** | an index range read |

The work does not disappear. It moves from *every read* to *every change*, and changes are
rare for any one (reader, series) pair.

## 2. What is stored

A sibling table, not new columns on `watchlist_entries`. A chapter batch would otherwise rewrite
the wide watchlist row, which the sync engine and the page query also hold. The sibling also
lets the rows be rebuilt without touching anything a reader owns.

```sql
CREATE TABLE watchlist_unread (
  user_id            uuid NOT NULL,
  series_id          uuid NOT NULL,
  unread_count       int  NOT NULL,          -- distinct whole chapters, unread and readable
  next_unread_milli  int,                    -- lowest unread readable chapter
  total_chapters     int  NOT NULL,          -- distinct whole readable chapters
  read_count         int  NOT NULL,          -- of those, below the whole frontier
  latest_milli       int,                    -- highest readable chapter
  latest_readable_at timestamptz,            -- newest readable sighting: the "released" sort key
  next_unlock_at     timestamptz,            -- earliest time a locked chapter becomes readable
  verified_at        timestamptz NOT NULL DEFAULT now(),
  PRIMARY KEY (user_id, series_id),
  FOREIGN KEY (user_id, series_id) REFERENCES watchlist_entries ON DELETE CASCADE
);
CREATE INDEX watchlist_unread_unlock ON watchlist_unread (next_unlock_at)
  WHERE next_unlock_at IS NOT NULL;
CREATE INDEX watchlist_unread_verified ON watchlist_unread (verified_at);
```

The proposal's `computed_at` became `verified_at`: the no-op guard in §3 means a recompute that
changes nothing writes nothing, so the column cannot record the last computation. It records the
last time the reconciler confirmed the row, which is the order the reconciler walks.

Deliberately **not** stored: `source_degraded`, `preferred_source_name` and `source_count`. They
are series-level, not per reader, and they depend on `series_sources.state` and `providers.state`.
Reading them live costs a few index lookups per watched series and never touches `chapters`.
Storing them would add provider state changes to the writer list for nothing.

`next_unread_title`/`next_unread_at` on the page stay a live `LIMIT 1` on the ≤ 60 rows of a page:
one primary-key probe each.

## 3. One source of truth

A single SQL function computes every stored column for a set of keys, using the unread predicate
spelled exactly as `dashboard.rs`' module doc prescribes:
- `floor()` before `::bigint`;
- `bigint`, not `int`;
- the early-access gate as an uncorrelated `ANY(ARRAY(SELECT … WHERE e.user_id = …))` InitPlan.

```sql
refresh_watchlist_unread(p_user uuid, p_series uuid[]) RETURNS void   -- upserts those rows
watchlist_unread_live(p_user uuid, p_series uuid[]) RETURNS SETOF …   -- the same values, unstored
```

`refresh_` is `INSERT … SELECT * FROM watchlist_unread_live(…) ON CONFLICT DO UPDATE … WHERE
(stored) IS DISTINCT FROM (computed)`, the same no-op guard 0055 put on the chapter upsert, so a
recompute that changes nothing writes nothing. The reconciler compares stored rows against
`watchlist_unread_live`. The Rust side (`ReadProgress::covers`) stays the oracle the differential
test compares both against.

Why a database function and not a `query!` in the repo: triggers (§4) have to call it, and a
trigger cannot call Rust.

## 4. Keeping it true: triggers, not call sites

### Writer inventory

Every statement in the tree that changes an input of the predicate, found by searching for
writes to the six input tables:

| input | writer | file |
|---|---|---|
| `chapters` insert / `access`, `unlocks_at` change | `upsert_chapters` (worker ingest) | `repo/catalog/chapters.rs:149` |
| `chapters` delete | `purge_chapters_batch` (console purge) | `repo/catalog/maintenance.rs:335` |
| `chapters` delete | `prune_chapters` (operator xtask) | `xtask/src/prune_chapters.rs:73` |
| `chapters` delete (cascade) | series delete, source delete, provider delete | `maintenance.rs:284`, `merge.rs:421`, `providers.rs:345` |
| `series_sources` insert | source registration | `repo/catalog/sources.rs:60`, `:158` |
| `series_sources.series_id` move | merge, revert, disentangle, operator repair | `matching/merge.rs:100`, `undo.rs:365`, `disentangle.rs:170`, `xtask/src/repair_series.rs:237` |
| `series_sources` delete (cascade) | series / provider delete | as above |
| `read_progress` | mark read / set / un-read, sync engine | `tracking/progress.rs:61`, `:89`, `:341`; merge `merge.rs:185`; revert `undo.rs:423`, `:433`, `:504` |
| `watchlist_entries` insert | track, bulk track, merge, revert | `watchlist/entries.rs:26`, `:60`, `:281`; `merge.rs:171`; `undo.rs:497` |
| `watchlist_entries` delete | untrack, bulk untrack, revert, account erasure | `entries.rs:143`, `:215`; `undo.rs:395`; `privacy.rs:142` (cascade) |
| `user_provider_early_access` | opt-in replace | `users/source_prefs.rs:116`, `:122` |
| *time* | an `unlocks_at` passing | nothing writes; see §5 |

That is 30 statements in 13 files, including an operator xtask, two cascades and a raw
`sqlx::query` that no compile-time check sees. An application-level "remember to recompute" at
each site is 30 chances to miss one, and a new writer would drift silently until the reconciler
noticed. Triggers cannot be bypassed by a writer that does not know about them.

### The triggers

Statement-level, with transition tables, so a 500-chapter batch is one trigger call rather than
500. Postgres refuses transition tables on a trigger with **more than one event** or with an
**`UPDATE OF` column list** (both verified on PG 18). So each table gets one trigger per event, and
an `UPDATE` trigger finds the rows that matter by joining `OLD TABLE` to `NEW TABLE` on the primary
key and comparing the predicate's columns itself.

| table | events (one trigger each) | rows that count | keys it recomputes |
|---|---|---|---|
| `chapters` | `INSERT`, `DELETE`, `UPDATE` | update: any of `series_source_id`, `number_milli`, `access`, `unlocks_at`, `discovered_at` differs | watchers of the series owning the old and new sources |
| `series_sources` | `DELETE`, `UPDATE` | update: `series_id` or `provider_id` differs | watchers of the old **and** new `series_id` |
| `read_progress` | `INSERT`, `DELETE`, `UPDATE` | all | the rows' own keys |
| `watchlist_entries` | `INSERT` | all | the rows' own keys (a delete cascades the stored row) |
| `user_provider_early_access` | `INSERT`, `DELETE` | all | that user's watched series carried by that provider |

Each trigger collects the affected `(user_id, series_id)` keys and calls
`refresh_watchlist_unread` once per user. That is twelve triggers, not the proposal's thirteen:
`series_sources` has no `INSERT` trigger, because a source is inserted before any chapter can
reference it and so changes nothing a row stores; its chapters arrive through the `chapters`
`INSERT` trigger. The `chapters` and `series_sources` `UPDATE` comparisons are wider than
proposed: a chapter moved between sources (the merge's re-point) and a source moved between
providers (which changes the early-access gate) both change stored values.

Three details that are load-bearing:

- **The `UPDATE` triggers must compare columns themselves.** `sources.rs:308` rewrites
  `chapter_count`/`last_scanned_at` on every scan, and the chapter upsert rewrites
  `title`/`path`/`published_at`. None of them change the predicate. Without the old/new comparison
  every scan would recompute every watcher; with it, those statements cost one join over their
  own transition tables and recompute nothing.
- **A converged rescan costs nothing.** 0055's `WHERE … IS DISTINCT FROM` guard means an upsert
  that changes nothing updates no row, so the transition tables are empty.
- **Keys are processed in `(user_id, series_id)` order.** A chapter batch touching several
  watchers and a reader's progress write both lock stored rows; a fixed order means they queue
  rather than deadlock.

### Cost on the write path

Per affected stored row, about 0.19 ms warm (§1). A chapter batch for a series with W watchers
adds W × 0.19 ms to the ingest transaction. The production slow log shows one heavy reader. The
diagnostics' section 7 reports the real watcher distribution, and the design should be re-checked
against it if any series has more than ~1 000 watchers.

## 5. Early-access unlock: a stored deadline and a sweeper

A locked chapter becomes readable when `unlocks_at` passes, and nothing is written when that
happens. Two options were weighed:

- **Keep the time-dependent part live on read.** Every read would still evaluate
  `unlocks_at <= now()` against `chapters`, which is exactly the scan being removed.
- **Store the deadline and sweep it (chosen).** `next_unlock_at` is the earliest `unlocks_at`
  above `now()` among chapters this reader cannot yet open. A 60-second tick on the control-plane
  leader recomputes rows `WHERE next_unlock_at <= now()`, served by a partial index. It never
  scans `chapters`, touches only rows whose answer is due to change, and bounds staleness at one
  tick. Early-access windows are measured in days, so a minute of staleness does not show.

The reconciler (§6) is the backstop if the sweeper stops.

## 6. The reconciler

Pattern of `services/control-plane/src/reconcile.rs`
([scan dispatch drift](../../services/control-plane/src/reconcile.rs)): leader-only, not gated on
the scanning flag, every `scheduler.unread_reconcile_interval_secs` (proposed 900).

- Take the next batch of 500 stored rows in `verified_at` order (oldest verification first).
- Compare against `watchlist_unread_live` for the same keys.
- For each mismatch: log the key and the fields, increment
  `watchlist_unread_drift_total{field}`, and repair with `refresh_watchlist_unread`.
- Also insert rows missing for existing watchlist entries (`missing` counts as drift).
- A row whose `next_unlock_at` has passed is due for the sweeper, not drifted, so it is repaired
  without being counted.

A healthy deployment's drift counter stays at zero. Any sustained rate means a writer the
triggers do not cover, which is precisely what the counter is for.

## 7. The reads

| statement | today | after |
|---|---|---|
| `me_stats` | `sum(lateral count)` over the watchlist | `sum(unread_count)` joined on the PK |
| `continue_reading` | two laterals over `chapters` | stored `unread_count > 0`, ordered by `latest_readable_at`; no `chapters` access |
| `watchlist::summary` | lateral count + live source rank | stored `unread_count` + live source rank |
| watchlist `fetch_page` / `fetch_counts` / `fetch_groups` | lateral predicates before `LIMIT` | stored columns for filter and sort; live source rank; live next-unread title for the page's rows |

`continue_reading`'s two-lateral doc comment is retired together with its laterals, not
contradicted: the reason it gave (keep the predicate an index condition) no longer applies to a
statement that does not read `chapters`.

`the_reading_surfaces_stay_in_the_chapter_indexes` keeps its assertion for every statement that
still reads `chapters`: the recompute function, the page's next-unread probe, the feed. It gains a
stronger one for the rewritten four: no `chapters` node at all.

## 8. The feed, independently

`feed` is not a count, so storing does not help it. It reads every unread chapter of every
watched source (326 k rows in, 97 k kept locally), sorts them to disk and window-aggregates them
to return 100.

A naive "per source, newest `$2` by `discovered_at`, then merge" is **not** equivalent. The feed
orders by each chapter's *earliest* readable sighting, `e(X) = min over sources`, while a
per-source scan orders by that source's own sighting. After a merge, a chapter a slow carrier picked
up recently but a fast one saw weeks ago can crowd a genuine top-100 chapter out of every
per-source top list.

The proposal's two-phase rewrite (bound `T` from per-source top lists, then collect every sighting
with `discovered_at ≥ T`) was **also not exact**, and was not implemented. Every chapter in the
true top has all its sightings collected, but a chapter *outside* it can have its old sightings
clipped by the bound: its `e` computed over the collected set is then its newest sighting rather
than its earliest, and it displaces a genuine top-`$2` chapter. Repairing that needs a second
lookup of every collected number's full sightings, which is most of the original work again.

What shipped is three stages, exact by construction because it computes the same `e` over the
same sightings as before, only over narrower rows:

1. **`sightings`**: per watched source, the unread readable tail with the predicate in the
   lateral's `WHERE`, so it is an index condition on `chapters_source_number_key`; three columns
   (`series_id`, `number_milli`, `discovered_at`).
2. **`newest`**: `min(discovered_at)` per `(series_id, number_milli)`, ordered, `LIMIT $2`.
3. **Carrier**: only those `$2` rows look up the carrying source (title, path, slug) by the same
   rank as before, with the same early-access gate.

`the_staged_feed_matches_the_statement_it_replaced` keeps the replaced statement verbatim as the
oracle and compares row for row on a fixture with a merged series whose slower carrier saw a
chapter later, a paywalled carrier that outranks a free one, and a limit that cuts through a tie. On the local clone the two
statements return identical rows at `LIMIT 100` and `LIMIT 5000`.

## 9. Migration and backfill

One migration, in order: the table, the functions, the twelve triggers (§4), then
`INSERT INTO watchlist_unread SELECT … FROM watchlist_unread_live(…)` for every watchlist row.

**Lock impact.** `CREATE TRIGGER` takes `SHARE ROW EXCLUSIVE` on `chapters`, `series_sources`,
`read_progress`, `watchlist_entries` and `user_provider_early_access`. Migrations run in one
transaction, so those locks are held until the backfill commits. Reads keep working; every
ingest, progress write and watchlist change **waits** for the duration.

**Duration.** About 0.19 ms per watchlist row warm on the local clone. Cold, on the production
cache, assume 5–10×.

| total watchlist rows | backfill, warm | cold (×10) |
|---|---|---|
| 5 000 | ~1 s | ~10 s |
| 50 000 | ~10 s | ~100 s |

`SELECT count(*) FROM watchlist_entries` (the diagnostics' section 7) decides which path:

- **Under ~20 000 rows: in the migration (chosen).** Writers pause for seconds; the worker's
  ingest retries through it. The reconciler's `missing` pass still exists, so a row the backfill
  somehow missed is created on its first pass.
- **Above: split.** The migration creates the table, functions and triggers but no rows. The
  reconciler's `missing` pass fills them in batches of 500, and reads fall back to the live
  predicate for a key with no stored row until the backfill reports complete. This is more code
  and a temporary dual path, so it is only worth it if the count demands it.

The down migration drops triggers, functions and table, and restores nothing else; the live
queries return with the revert of the read rewrites.

## 10. Tests

All in `crates/db/tests/`:

- **Differential, extended** (`repo_tracking`). The differential loop and the early-access steps
  additionally call `assert_stored_matches_live` (no missing row, no column differing from
  `watchlist_unread_live`) after every write they already make. No existing assertion was
  removed or loosened (rule 5).
- **Trigger coverage** (`every_writer_of_an_unread_input_keeps_the_stored_figures_true`). One
  test walks the writers through their public repo functions: track, progress, chapter upsert,
  access change, source preference, early-access opt-in, merge and revert, the repair `UPDATE`,
  chapter purge, provider delete, untrack; stored equals live after each. Mutation-checked:
  dropping the `chapters` `INSERT` trigger fails it.
- **Unlock sweeper** (`an_expired_unlock_counts_once_the_sweeper_runs`).
- **Reconciler** (`the_reconciler_repairs_drift_and_missing_rows`): corrupts one row and deletes
  another; one pass counts both and repairs both.
- **Feed equivalence** (`the_staged_feed_matches_the_statement_it_replaced`), as in §8.
- **Plans** (`repo_query_plans`): `continue_reading` and `me_stats` must have no `chapters` node;
  `watchlist_unread_live` is `EXPLAIN`ed explicitly for index-only chapter scans and a single
  opt-in InitPlan, since function bodies are not in `.sqlx`.
- **Classification**: `repo_privacy` lists `watchlist_unread` as derived and not exported;
  `repo_matching` classifies its `series_id` as cascading with the watchlist entry.

## 11. What this replaces from the plan

Phase 2.2 proposed a per-user Redis cache for the same three endpoints. Its invalidation matrix is
the writer inventory above, maintained by hand in the API instead of by triggers in the database,
and it still pays the full computation on every miss and every invalidation. With the token
refresh no longer refetching Home (Phase 2.1), the cache would buy little before this lands and
nothing after. **Recommendation: skip it.**

## 12. Decisions

1. Triggers (§4) rather than call sites. **Approved.**
2. A 60 s unlock sweeper with a stored deadline (§5), rather than live evaluation. **Approved**;
   `scheduler.unread_unlock_interval_secs`, default 60.
3. Backfill path (§9). **In the migration.** The production `watchlist_entries` count was not
   available; if it is above ~20 000, the split path in §9 is the fallback, and the migration's
   lock window should be measured first.
4. Skip the Redis cache (§11). **Approved.**

## 13. Measurements

Local clone only, the environment in
[`2026-09-local-measurements.md`](2026-09-local-measurements.md) (2.84 M chapters, one reader with
859 watchlist rows, no early-access opt-ins, `jit=off` in the server config). Before is the
statement text of the `.sqlx` cache at the commit preceding 0058, run on the same database before
the migration was applied; after is the new cache, after it. Cold is a Postgres restart plus a
dropped VM page cache, one execution. JIT is the warm run under `SET jit=on`; generic is
`plan_cache_mode = force_generic_plan`.

### Reads

| statement | cold ms (blocks read) | warm ms | generic ms | est. cost | JIT with `jit=on` |
|---|---|---|---|---|---|
| `continue_reading` | 1 501 (7 163) → **76** (601) | 35.4 → **1.1** | 42.8 → 1.2 | 12 961 → 1 062 | no → no |
| `me_stats` | 911 (5 193) → **2.5** (53) | 34.9 → **0.31** | 34.7 → 0.32 | 9 634 → 102 | no → no |
| `watchlist::summary` | 791 (5 206) → **155** (1 973) | 41.6 → **7.0** | 38.6 → 9.1 | 16 801 → 7 307 | no → no |
| `feed`, limit 100 | 2 470 (17 398) → **1 207** (16 541) | 369 → **119** | 306 → 119 | 16 475 → 14 932 (generic 37 462) | no → no |
| watchlist page, sort released | 1 604 (9 687) → **427** (3 997) | 73.9 → **13.3** | 78.5 → 18.9 | 28 194 → 13 372 | no → no |
| watchlist page, sort progress | 1 614 (10 018) → **331** (3 050) | 136 → **9.6** | 148 → 11.5 | 47 547 → 13 381 | no → no |
| watchlist page counts | 606 (5 350) → **206** (2 090) | 30.7 → **6.9** | 58.5 → 8.6 | 17 347 → 7 908 | no → no |
| watchlist page groups | 1 249 (8 174) → **182** (2 089) | 49.1 → **5.1** | 60.6 → 8.7 | 19 621 → 6 056 | no → no |

- **Identical output.** For all eight, the old and new statements return the same rows in the
  same order on the clone (`EXCEPT ALL` both ways, then a line-for-line diff of the ordered
  output).
- **What is left of each.** The summary, page, counts and groups still rank sources live
  (`source_degraded`, `preferred_source_name`), which is their remaining cold cost; the page also
  keeps its live next-unread probe for its own 50 rows. `continue_reading` and `me_stats` never
  touch `chapters`.
- **`feed`** no longer spills its sort to disk (3 040 temp blocks written before, none after). Its
  cold cost is still reading the watched sources' unread tails from `chapters`; stored state does
  not help a statement that returns individual chapters. The memory sizing in the latency report
  is what moves that number.
- No statement's estimate is near `jit_above_cost` (100 000), before or after, custom or generic.

### Writes

Each statement run seven times inside a rolled-back transaction, median, with the triggers and
with `session_replication_role = replica` (which disables all triggers, so the `read_progress` row
also loses the recsys staleness trigger it already had). The heaviest watched source has 3 898
chapters and one watcher.

| statement | without triggers | with | added |
|---|---|---|---|
| insert 500 chapters, watched source | 3.3 ms | 11.5 ms | +8.2 ms |
| insert 500 chapters, unwatched source | 2.9 ms | 5.7 ms | +2.8 ms |
| rewrite the title of 3 898 chapters (no predicate column) | 14.6 ms | 20.2 ms | +5.6 ms |
| `read_progress` upsert, heaviest series | 0.9 ms | 9.5 ms | +8.6 ms |

- **The unwatched and title-only cases** cost the transition-table scan and the old/new
  comparison, and recompute nothing.
- **A watched write** adds one recompute per watcher. That is +8 ms for the heaviest series with
  one watcher, so a series with W watchers costs about 8·W ms on its ingest transaction.
- **Recomputing all 859 rows** through `watchlist_unread_live` takes 124 ms warm (0.14 ms per row).
  The backfill in the migration took 274 ms for the same rows, including the `INSERT`. The
  reconciler's 500-row pass reads live and compares, so expect roughly 100–150 ms every 15 minutes.
