# Chapter storage — where the space goes, and what to do about it

`chapters` is the largest table in the deployment; `deploy/docker-compose.yml` records that it
"passed 1.2 GB" while sizing Postgres, against a container capped at 1 GB with 256 MB of
`shared_buffers`. Size here is not a disk-cost question — disk is cheap — it is a *cache
residency* question. The comment beside the Postgres tuning in that file already names the
symptom: "a running scan's chapter inserts evict the working set continuously", and the dashboard
and trigram statements went from 40–140 ms warm to 1.0–3.3 s cold because of it.

This document is the measured breakdown of that 1.2 GB and the staged change that roughly halves
it.

**Status: implemented**, as migration `0055_chapter_storage` plus the repo-layer rewrite that
goes with it — with one deliberate exception, §3b, which was dropped after inspection and is kept
below with the reason. Each section still names what the change cost and what it touched, because
that is what a later reader needs in order to undo or extend it.

---

## 1. How the numbers below were obtained

A `pgvector/pgvector:pg18` container, the `chapters` schema exactly as migrations 0003 and 0047
leave it, and 1 200 000 rows across 3 000 sources — 400 chapters each, realistic paths
(`manga/<slug>/chapter-<n>/`, mean 42.4 characters), `title`/`volume`/`unlocks_at` mostly NULL,
which is what the ingest actually writes. Every figure is post-`VACUUM FULL`, so the baseline is
the *best case*: a freshly packed table with no bloat at all.

Per-row figures are the relation size divided by 1 200 000. They scale linearly; multiply by your
own row count.

## 2. The baseline

| Component | Bytes/row | Share |
|---|---:|---:|
| Heap | 145.1 | 36% |
| `chapters_pkey` — the `uuid` primary key | 31.6 | 8% |
| `chapters_series_source_id_number_key` — the `UNIQUE (series_source_id, number)` index | 40.6 | 10% |
| `chapters_source_idx` — `(series_source_id, number DESC)` | 40.6 | 10% |
| `chapters_discovered` — `(discovered_at DESC)` | 22.5 | 6% |
| `chapters_source_floor_num_access_idx` — `(series_source_id, floor(number), number) INCLUDE (access, unlocks_at)` | 58.9 | 15% |
| `chapters_source_disc_access_idx` — `(series_source_id, discovered_at DESC) INCLUDE (access, unlocks_at)` | 58.9 | 15% |
| **Total** | **398.2** | |

**Indexes are 64% of the table.** Six of them, on the one relation that grows without bound. That
is where the work is, and it is not evenly distributed: three of the six index some form of
`(series_source_id, number)`.

---

## 3. Stage 1 — the unconditional re-upsert (no migration, no schema change)

This is the largest single item in the document and the cheapest to fix.

`upsert_chapters` ([`crates/db/src/repo/catalog/chapters.rs`](../crates/db/src/repo/catalog/chapters.rs))
ends with an unguarded `ON CONFLICT (series_source_id, number) DO UPDATE SET …`. Every scan of a
series re-upserts every chapter it lists. A *converged* rescan — one where the provider published
nothing and every value is byte-identical to what is already stored — still writes a new version
of every row, because an `UPDATE` that assigns the same value is still an `UPDATE`.

Measured, on the 456 MB baseline:

| | Total relation size |
|---|---:|
| Baseline | 456 MB |
| After **one** converged rescan | **1046 MB** |
| After three | 1465 MB |

One no-op rescan more than doubles the table. Autovacuum reclaims the dead tuples eventually, but
"eventually" is the point: the space is occupied, the pages are dirtied, the WAL carries all of
it (six indexes' worth of full-page writes), and the eviction pressure the compose file already
describes is manufactured on every scan cycle rather than by real discoveries.

**The fix** is a `WHERE` clause on the `DO UPDATE`, so a row is rewritten only when something
actually differs:

```sql
ON CONFLICT (series_source_id, number) DO UPDATE
   SET title = EXCLUDED.title, path = EXCLUDED.path,
       published_at = COALESCE(EXCLUDED.published_at, chapters.published_at),
       access = EXCLUDED.access, unlocks_at = EXCLUDED.unlocks_at
 WHERE chapters.title       IS DISTINCT FROM EXCLUDED.title
    OR chapters.path        IS DISTINCT FROM EXCLUDED.path
    OR chapters.access      IS DISTINCT FROM EXCLUDED.access
    OR chapters.unlocks_at  IS DISTINCT FROM EXCLUDED.unlocks_at
    OR (EXCLUDED.published_at IS NOT NULL
        AND chapters.published_at IS DISTINCT FROM EXCLUDED.published_at)
```

Same measurement with the guard in place: **456 MB before, 456 MB after three converged
rescans.** Nothing is written.

Two things this changes that a reader has to know about:

- **A suppressed update returns no row.** `upsert_chapters` reads `RETURNING … (xmax = 0) AS
  inserted` and then filters to `inserted`, so rows dropping out of the result set is harmless
  there — the discovery fan-out is unaffected. `upsert_chapter` (singular) uses `fetch_one` and
  *would* start erroring; it has **zero call sites** and should be deleted rather than fixed.
- The `published_at` arm has to mirror the `COALESCE`, or a row whose stored `published_at` is
  non-NULL and whose incoming one is NULL would be seen as "differing" forever and rewritten on
  every scan — reintroducing the exact problem in a smaller form.

### 3b. The change-detection that already exists and is never read — NOT DONE, deliberately

`series_sources.content_hash` is computed on every scan (`content_hash` in
[`services/worker/src/engine/mod.rs`](../services/worker/src/engine/mod.rs)) and written by
`update_source_scan`. `source_content_hash` in
[`crates/db/src/repo/catalog/sources.rs`](../crates/db/src/repo/catalog/sources.rs) reads it back
— and **has no callers**. `scan_series` calls `ingest_series` unconditionally.

An earlier draft of this document proposed comparing the hash and skipping the whole ingest
transaction for a converged source. **That was wrong, and it is not implemented.** The hash covers
`meta.title`, `meta.description` and each chapter's `(number, path)` — and nothing else. It does
not cover `access` or `unlocks_at`, so a chapter whose paywall had lifted would stop being
un-locked; nor tags, authors, cover, status, content type or release year, all of which
`ingest_series` writes. Gating ingest on it would silently stop propagating exactly the change
migration 0047 exists to keep current.

Widening the hash is not a free fix either: it deliberately excludes chapter titles, and says so
— "scanlation sites edit labels constantly", so a retitle must not read as a change. That is a
scan-scheduling decision, not an ingest one, and the two want different hashes.

The remaining benefit was never storage anyway — with the `WHERE` guard in §3 the converged
transaction already writes nothing. What is left is the read queries and the `tags`/`authors` row
locks. If that is worth reclaiming later, the shape is a **second** hash covering exactly what
`ingest_series` persists, stored beside the existing one, leaving `content_hash`'s meaning intact.

§3 itself is a pure code change: no migration, no downtime, no artefact regeneration.

---

## 4. Stage 2 — a redundant index (one line)

`chapters_source_idx (series_source_id, number DESC)` is a duplicate of the index the `UNIQUE
(series_source_id, number)` constraint already builds. A btree scans backwards at essentially the
same cost, so the `DESC` buys nothing; the two are the same 40.6 bytes per row twice over.

```sql
DROP INDEX CONCURRENTLY IF EXISTS chapters_source_idx;
```

**−40.6 bytes/row (−10%).** No code change, no query plan that loses an option.

---

## 5. Stage 3 — the primary key nothing references

`chapters.id uuid PRIMARY KEY DEFAULT gen_random_uuid()` is:

- **Not a foreign key target.** No table in any migration has a `chapter_id` column; `REFERENCES
  chapters` appears nowhere.
- **Not on the wire.** `ChapterDto` ([`services/api/src/series.rs`](../services/api/src/series.rs))
  carries `number`, `title`, `url`, `published_at`, `read`. No id.
- **Used in exactly three places**, all replaceable:
  1. Hydrating `Chapter.id`, which no consumer reads.
  2. `ORDER BY c.number, c.discovered_at, c.id` in
     [`crates/db/src/repo/tracking/watchlist/page.rs`](../crates/db/src/repo/tracking/watchlist/page.rs)
     — a determinism tiebreaker that `(series_source_id, number)` provides natively.
  3. `DELETE FROM chapters WHERE id IN (SELECT id FROM chapters LIMIT $1)` in
     [`crates/db/src/repo/catalog/maintenance.rs`](../crates/db/src/repo/catalog/maintenance.rs)
     — batched purge, which `ctid` does better anyway.

It is also a *random* UUID, so its index has no insert locality: every insert lands on an
arbitrary leaf page. The 31.6 bytes/row measured above is after `VACUUM FULL`; a live table's
`chapters_pkey` will be meaningfully larger than that.

Promoting `(series_source_id, number)` to the primary key removes 16 bytes from every heap tuple
and the entire `chapters_pkey` index.

**−46.5 bytes/row (−12%).** Costs: a migration that rewrites the table, the three call-site
changes above, removing `ChapterId` from the domain and from `openapi.rs`'s registered
components — which is an OpenAPI surface change, so `xtask openapi` and the oasdiff gate are
involved.

---

## 6. Stage 4 — the chapter number, and the index it lets you delete

`number numeric(10,4)` is variable-width and, critically, **`floor(number)` is not derivable from
an index on `number`**. That single fact is why `chapters_source_floor_num_access_idx` exists:
migration 0026 documents at length what the unread predicate cost (2.9 s on the Home stats query)
before `floor(number)` was indexed, and 0047 widened it again with `INCLUDE (access, unlocks_at)`
to keep the scan index-only.

Store the number as a fixed-width integer scaled by 10⁴ — `number_milli int NOT NULL`, so 1050.5
is 10 505 000 — and `floor(number) > X` becomes `number_milli >= (X + 1) * 10000`. That is a
plain range predicate on the second key column of the `(series_source_id, number)` index. The
`floor()` index has nothing left to do.

Which means **one index can replace four**:

```sql
CREATE UNIQUE INDEX chapters_source_number_key
    ON chapters (series_source_id, number_milli) INCLUDE (access, unlocks_at);
```

A unique index may carry `INCLUDE` columns (PG 11+), so this one enforces uniqueness, is the
`ON CONFLICT` arbiter, serves `ORDER BY number DESC` by scanning backwards, **and** answers the
unread predicate index-only. Verified on the fixture:

```
Index Only Scan using v_f_source_number_key on v_f
  Index Cond: ((series_source_id = '…'::uuid) AND (number_milli >= 2510000))
  Filter: ((access = 'free') OR (unlocks_at <= now()))
```

| Replaced | Bytes/row |
|---|---:|
| `chapters_pkey` | 31.6 |
| `chapters_series_source_id_number_key` | 40.6 |
| `chapters_source_idx` | 40.6 |
| `chapters_source_floor_num_access_idx` | 58.9 |
| **Sum** | **171.7** |
| Replacement: `chapters_source_number_key` | 49.7 |
| **Net** | **−122.0** |

(Stages 2 and 3 are subsumed by this one; do them first only if you want the easy wins before
committing to the invasive change.)

The heap is unchanged — a `numeric(10,4)` holding these values already occupies 8 bytes after
alignment, the same as an `int` plus padding. The entire win is the index collapse.

**What it cost, since it was the most invasive part.** `number` appeared in about forty SQL sites
across `catalog/chapters.rs`, `tracking/dashboard.rs`, `tracking/progress.rs`,
`tracking/watchlist/{page,summary}.rs`, `recsys/{features,prior}.rs` and `stats.rs`. The
translations are tabulated in `repo::catalog::chapters`' module doc and every site uses them; the
scale itself lives once, in `tankovault_domain::chapter_number`.

`read_progress.last_read_whole_number` and `last_read_part_number` were **not** migrated. They are
a small table, they are on the sync and API wire, and moving them would have tripled the blast
radius for no storage gain. Instead the bound is computed in `bigint` on the progress side of each
comparison, leaving the `chapters` column bare on the left where the index can reach it — verified
by `repo_query_plans::the_reading_surfaces_stay_in_the_chapter_indexes`.

Two things this forced that were not obvious up front:

- **`floor(…)` before any `::bigint` cast.** `numeric::bigint` *rounds*, so a
  `last_read_whole_number` of `5.5` would have become 6 and hidden chapter 6 as already read.
- **The plan-audit rule had to be fixed, not budgeted.** `repo_query_plans` flagged eight healthy
  queries at once, because it detected trigram-similarity-as-filter by text-matching `" % "` — and
  Postgres spells integer modulo the same way. The part-release test `number_milli % 10000 <> 0`
  is not a trigram match. The rule now reads the right operand to tell them apart, pinned by
  `the_similarity_rule_tells_the_operator_from_integer_modulo`.

The `CHECK (number_milli >= 0)` is not decoration: `floor(number)` is spelled as the integer
division `number_milli / 10000`, which only equals `floor` for non-negative values.

---

## 7. Stage 5 — the path prefix

`path` is the largest variable field in the row: mean 42.4 characters in the fixture, and for
most providers it nests under the series' own path —
`series_sources.source_path = 'manga/<slug>/'`, chapter path `'manga/<slug>/chapter-1050/'`. The
prefix is already stored, once per source, in a table every chapter query already joins.

Store the remainder instead. Mean length falls from 42.4 to 11.7.

**−32.9 bytes/row (−8%)**, entirely in the heap, which is also the part that has to stay in
`shared_buffers`.

**This is provider-dependent and must not be assumed.** MangaDex's series path is `/title/{uuid}`
and its chapter path is `/chapter/{uuid}` — no shared prefix at all. The encoding that handles
both without an extra column: **a value beginning with `/` is site-relative and stored whole;
anything else is relative to `series_sources.source_path`.** Adapters already emit site-relative
paths through `relativize`, which guarantees the leading slash, so the discriminator is free.

The prefix stripped is `source_path` with a **trailing slash**, never the bare string: `/manga/x`
and `/manga/x-2` are different series, and stripping on the bare prefix would silently rewrite
one's chapters as the other's. Pinned by `link::tests::a_sibling_slug_is_not_treated_as_nested`.

Compression is `tankovault_domain::compress_chapter_path`, applied once in `upsert_chapters`.
Expansion has two spellings that must stay in step — `expand_chapter_path` in Rust, and the
`chapter_url_path(source_path, stored)` SQL function migration 0055 defines, which is what the
three read queries select through so that nothing above the repo layer ever sees the stored form.

---

## 8. Stage 6 — the global `discovered_at` index

`chapters_discovered (discovered_at DESC)` serves exactly three queries, all in
[`crates/db/src/repo/stats.rs`](../crates/db/src/repo/stats.rs): the counts of chapters
discovered in the last hour, day and week on the admin overview.

Postgres 18's btree skip scan can answer those from
`chapters_source_disc_access_idx (series_source_id, discovered_at DESC)` instead, by skipping
across the leading column. Measured on the fixture, with the global index dropped:

| | Latency |
|---|---:|
| With `chapters_discovered` | 0.14 ms |
| Skip scan over the per-source index (3 001 index searches) | 19.8 ms |

**−22.5 bytes/row (−6%)** for 20 ms on a query that sits behind `ADMIN_STATS_TTL` (30 s). That is
a judgement call, not a free win, and it is listed last for that reason.

A BRIN index on `discovered_at` was measured as an alternative and is **not** recommended: it
costs 24 kB, but the planner prefers the skip scan over it anyway, so it adds a maintained index
that answers nothing. BRIN would also only be viable *after* stage 1 — the unconditional
re-upsert destroys the physical/temporal correlation BRIN depends on.

---

## 9. Where it lands

| Stage | Change | Bytes/row | Running total |
|---|---|---:|---:|
| — | Baseline | | 398.2 |
| 1 | Guard the upsert (no schema change) | 0 | 398.2 |
| 4 | One covering unique index replaces four | −122.0 | 276.2 |
| 3 | `id` removed from the heap | −14.9 | 261.3 |
| — | Column reorder for alignment | −4.2 | 257.1 |
| 5 | Path relative to source path | −32.9 | 224.2 |
| 6 | Global `discovered_at` index dropped | −22.5 | **201.7** |

**398.2 → 201.7 bytes/row, a 49% reduction** — measured end to end as 456 MB → 231 MB on the
1.2 M-row fixture, not summed from estimates.

Re-measured afterwards against the **shipped** schema — migrations run in order through 0055, the
same 3 000 sources × 400 chapters, `VACUUM FULL` — it came out slightly better than the prototype:

| | Bytes/row | Size |
|---|---:|---:|
| Heap | 84.5 | 97 MB |
| `chapters_source_number_key` (the primary key) | 49.7 | 57 MB |
| `chapters_source_disc_access_idx` | 58.9 | 67 MB |
| **Total** | **193.0** | **221 MB** |

**398.2 → 193.0 bytes/row, −51%**, and the index share falls from 64% to 56%. The heap beat the
prototype's 93.1 because the real `source_path` values are longer than the prototype's, so more of
each chapter path compresses away.

Against the 1.2 GB the compose file records, that is roughly 600 MB returned to a 1 GB container
with 256 MB of `shared_buffers`. And stage 1, which is worth none of those bytes on paper, is
worth more than all of them in practice: it is the difference between a table that grows with
discoveries and one that doubles every time the crawler goes round.

### How it was shipped

All six landed together, in `0055_chapter_storage` and the repo-layer rewrite beside it. Stages 2
and 3 are subsumed by stage 4 — the single covering unique index replaces all four of the old
ones — so splitting them would have meant rewriting the same call sites twice.

### Deploying it

The migration **rewrites the table**: a new one is built, filled from the old, and swapped. sqlx
sends the file as one simple-query string, so Postgres wraps it in an implicit transaction and the
swap is atomic — but it is **not online**. It holds an `ACCESS EXCLUSIVE` lock for the length of
the rewrite. On a deployment whose `chapters` is already at 1.2 GB, run it in a maintenance
window.

Rows outside the new storable range (`0 .. 200 000`) are **dropped, not clamped**, and the count is
raised as a `NOTICE`. They are junk by construction — the ceiling is fifty times the longest series
anyone has published — and clamping would fold a date-shaped slug onto chapter 200 000, which a
reader's progress would then be measured against.

The down-migration restores the old shape and round-trips everything except `volume`, which no
adapter ever populated, and the rows the range check dropped.

### Two things deliberately not proposed

- **TOAST compression.** Every row here is far below the ~2 kB threshold at which Postgres
  compresses anything, so no `lz4` setting changes a byte. The columns are small and numerous,
  which is the case TOAST does not address.
- **Deduplicating chapters across sources.** A series carried by four providers stores four sets
  of chapter rows. That looks like 4× redundancy until you notice `path` differs per source and
  is the largest field — a shared chapter table plus a per-source path table stores the same
  bytes with an extra join. Stage 5 attacks the same redundancy from the side that actually
  pays.

### Dead weight removed along the way

- **`chapters.volume`** was `Option<i32>` in the domain, a column in the schema, and set to `None`
  by the only writer there is. No adapter ever populated it. Dropped.
- **`upsert_chapter`** (the singular one) had no call sites, and would have started erroring under
  the §3 guard — `fetch_one` against a statement that now legitimately returns no row. Deleted
  rather than fixed.
- **`ChapterId`** is gone from the domain, from `openapi.json` and from the generated
  `crates/api-client`. It was a registered schema component that no response body ever carried.

### What a later reader should not undo by accident

- **The `WHERE` on the `DO UPDATE` is load-bearing**, and its `published_at` arm is not redundant
  with the others — see §3.
- **`floor(number)` is `number_milli / 10000` everywhere**, and that identity depends on the
  `CHECK (number_milli >= 0)`. Removing the constraint silently breaks every unread count.
- **The unread predicate's lower bound is `bigint` on purpose**, for legacy `read_progress` rows,
  not for in-range chapters. §6.
- **`chapters.path` is not a path.** It is a path *or* a suffix, discriminated by the leading
  slash. Read it through `chapter_url_path` in SQL or `expand_chapter_path` in Rust; never raw.
