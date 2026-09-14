# Stored unread state — design (Phase 3, awaiting approval)

Status: **proposal**. No migration or query in this document has been written. It exists so the
mechanism, the writer inventory and the backfill can be agreed before they are.

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
  computed_at        timestamptz NOT NULL DEFAULT now(),
  PRIMARY KEY (user_id, series_id),
  FOREIGN KEY (user_id, series_id) REFERENCES watchlist_entries ON DELETE CASCADE
);
CREATE INDEX watchlist_unread_unlock ON watchlist_unread (next_unlock_at)
  WHERE next_unlock_at IS NOT NULL;
```

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
| `chapters` | `INSERT`, `DELETE`, `UPDATE` | update: `access` or `unlocks_at` differs | watchers of the series owning the changed sources |
| `series_sources` | `INSERT`, `DELETE`, `UPDATE` | update: `series_id` differs | watchers of the old **and** new `series_id` |
| `read_progress` | `INSERT`, `DELETE`, `UPDATE` | all | the rows' own keys |
| `watchlist_entries` | `INSERT` | all | the rows' own keys (a delete cascades the stored row) |
| `user_provider_early_access` | `INSERT`, `DELETE` | all | that user's watched series carried by that provider |

Each trigger collects the affected `(user_id, series_id)` keys and calls
`refresh_watchlist_unread` once per user.

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

- Take the next batch of 500 stored rows in `computed_at` order (oldest verification first).
- Compare against `watchlist_unread_live` for the same keys.
- For each mismatch: log the key and the fields, increment
  `watchlist_unread_drift_total{field}`, and repair with `refresh_watchlist_unread`.
- Also insert rows missing for existing watchlist entries (`missing` counts as drift).

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
per-source top list. The exact rewrite has two phases:

1. **Bound.** Per watched source, take the `$2` newest unread readable sightings (a backward range
   read of `chapters_source_disc_access_idx`), compute `e` exactly for those numbers, and let
   `T` be the `$2`-th largest `e` among them. Candidates are a subset of all chapters, so the true
   `$2`-th largest `e` is ≥ `T`.
2. **Collect.** Take every unread readable sighting with `discovered_at ≥ T` from those sources.
   Any chapter in the true top `$2` has `e ≥ T`, so *all* its sightings have `discovered_at ≥ T`
   and all of them are collected. Compute `e` and the carrier rank over that set, exactly as
   today, then order and `LIMIT`.

The result is identical by construction. The proof obligation goes in a test: the current
statement is kept verbatim in the test file as the oracle and compared row for row (series,
number, carrier slug, path, `discovered_at`) on the `repo_tracking` fixture plus a merged-series
case built to trip the naive version.

## 9. Migration and backfill

One migration, in order: the table, the two functions, the twelve triggers (§4), then
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

- **Under ~20 000 rows: in the migration.** Writers pause for seconds; the worker's ingest
  retries through it.
- **Above: split.** The migration creates the table, functions and triggers but no rows. The
  reconciler's `missing` pass fills them in batches of 500, and reads fall back to the live
  predicate for a key with no stored row until the backfill reports complete. This is more code
  and a temporary dual path, so it is only worth it if the count demands it.

The down migration drops triggers, functions and table, and restores nothing else; the live
queries return with the revert of the read rewrites.

## 10. Tests

- **Differential, extended.** `the_sql_and_the_rust_predicate_agree_on_every_chapter` and the
  early-access, parts, floor/bigint and merge scenarios each additionally assert
  `stored == live == Rust` for every key, after every write they already make. No existing
  assertion is removed or loosened (rule 5).
- **Trigger coverage, per writer row in §4.** A test per writer that performs the write through
  its public repo function and asserts the stored row changed. The xtask writers are covered by
  issuing their statement text.
- **Unlock sweeper.** A chapter with `unlocks_at` in the past but a stored `next_unlock_at` not yet
  swept; one sweep makes it count.
- **Reconciler.** Corrupt a stored row directly; one pass repairs it and increments drift.
- **Feed equivalence**, as in §8.
- `repo_query_plans`: the new function bodies are not in `.sqlx` and so not swept by default. Its
  `EXPLAIN` coverage gets them added explicitly.

## 11. What this replaces from the plan

Phase 2.2 proposed a per-user Redis cache for the same three endpoints. Its invalidation matrix is
the writer inventory above, maintained by hand in the API instead of by triggers in the database,
and it still pays the full computation on every miss and every invalidation. With the token
refresh no longer refetching Home (Phase 2.1), the cache would buy little before this lands and
nothing after. **Recommendation: skip it.**

## 12. Decisions needed before implementation

1. Triggers (§4) rather than call sites. The alternative is 30 call sites plus reliance on the
   reconciler.
2. A 60 s unlock sweeper with a stored deadline (§5), rather than live evaluation.
3. Backfill path (§9), once the production `watchlist_entries` count is known.
4. Skip the Redis cache (§11).
