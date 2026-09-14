# Discover/browse latency: measured redesign proposal

Status: **proposal, awaiting approval**. Nothing in this document is implemented.

Scope: `GET /v1/series` (`services/api/src/series.rs::list`) and its six statements in
`crates/db/src/repo/catalog/browse.rs`. All numbers are local (`tv_perf`, 53 872 series, 2.84 M
chapters, 102 providers, 859-row watchlist, PG 18.4, `shared_buffers=256MB`, `work_mem=16MB`,
`random_page_cost=1.1`, NVMe under WSL2). **Nothing here is a production measurement.** Prototypes
ran on a `STRATEGY file_copy` clone (`tv_browse`), dropped afterwards.

The measurement harness, raw results and prototype SQL were kept out of the repository; the method
is the one in [`2026-09-local-measurements.md`](2026-09-local-measurements.md).

## 1. The surface

### Statements (the `.sqlx` entries measured)

| statement | `.sqlx` hash | used when |
|---|---|---|
| count, no search | `eb51848…` | every request without `query` |
| count, search | `9f485ed…` | every request with `query` |
| page, recency, no search | `36494e9…` | `sort` ∈ updated/rating/relevance, no `query` |
| page, sort token, no search | `367a496…` | title/chapters/sources/year, no `query` |
| page, recency, search | `f8c60bf…` | `sort=updated|rating` with `query` |
| page, sort token, search | `3ae5331…` | title/chapters/sources/year with `query` |
| page, relevance | `eb455d7…` | `query` and `sort` relevance (the default when searching) |

`list_series_filtered` runs page and count concurrently on two pool connections (`try_join!`), for
**every** request, on the reader pool (no `force_custom_plan`, 5 s statement ceiling since PR #390).

Shared predicate, `$1`–`$11`: content type, status, year min/max, provider slug (`EXISTS` join),
min chapters (correlated `max(chapter_count)`), include tags (correlated `NOT EXISTS (unnest EXCEPT
…)`), exclude tags (`NOT EXISTS … = ANY`), adult gate (`NOT adult_gated OR $9`, resolved server-side),
tracking (`$11 = EXISTS (watchlist_entries …)`, user from token). Search adds the `matched` CTE
(trigram on `series.normalized_title`, FTS on `search_vec`, trigram on `series_titles`).

### Response contract

Body `SeriesSummary[]`. `X-Total-Count` = total (always). `X-Next-Cursor` = `page + 1` when
`offset + returned < total`, **derived from the count**. Both are in `openapi.json`. After the page,
`SeriesSummary::page` runs three batched reads for the returned ids (not measured here).

### Consumers of the total (from code)

| consumer | reads `X-Total-Count` | reads `X-Next-Cursor` | notes |
|---|---|---|---|
| `web/frontend/src/views/discover/mod.rs` | yes, only for the count line ("first–last of total") | yes, end-of-list (`exhausted`) | **Every page fetch (tail and head) recounts with identical filters**; `total.set` on every merge. A window of 600 items at 24/page is 25 counts. If the header were missing it falls back to `r.len()`, i.e. it would display the page length as the total. |
| `web/frontend/src/views/search/mod.rs` | no | no | one page, total discarded, count still runs |
| `views/console/sync/inspector.rs` (≥2 chars, limit 12), `console/sync/queues.rs` (≥3 chars, limit 8) | no | no | typeahead per query change; count still runs |
| `components/pagination.rs` | not used by Discover | | |
| desktop app | same frontend code, but installers are versioned independently of the server | | an old installer keeps talking to a new API |
| `crates/api-client` | generated; exposes the headers via `ResponseValue` | | |
| `services/sync` | no use found | | |

## 2. Workload and before numbers

31 shapes. Times in ms from `EXPLAIN (ANALYZE, BUFFERS)` of the committed statement
text, `PREPARE`d with the `.sqlx` parameter types. **warm**: second execution, `force_custom_plan`.
**cold**: `docker restart` + `drop_caches`, one execution. **generic**: `force_generic_plan`. **JIT**:
re-prepared with `jit=on` (production compose has `jit=off`). Page = 24 rows, offset 0 unless noted.

| shape | rows | count warm | count cold | count generic | count cost custom / generic | JIT ms with `jit=on` (custom / generic) | page warm | page cold | page generic | page cost custom / generic |
|---|---|---|---|---|---|---|---|---|---|---|
| unfiltered | 52530 | 16.1 | 40.2 | 18.2 | 8,329 / 1,866,607 | - / 196.0 | 0.1 | 8.8 | 0.1 | 90 / 467,025 |
| unfiltered, page at offset 2400 | | | | | | | 6.7 | - | 6.9 | 9,038 / 467,025 |
| adult_on | 53872 | 3.1 | - | 22.0 | 1,017 / 1,866,607 | - / 162.2 | 0.1 | - | 0.1 | 90 / 467,025 |
| type_manhwa | 1795 | 13.2 | - | 14.6 | 8,338 / 1,866,607 | - / 156.7 | 0.2 | - | 0.2 | 182 / 467,025 |
| type_unknown | 43174 | 15.2 | - | 16.5 | 8,438 / 1,866,607 | - / 166.2 | 5.5 | - | 6.0 | 91 / 467,025 |
| status_completed | 7149 | 4.3 | 59.8 | 16.3 | 5,375 / 1,866,607 | - / 173.2 | 0.1 | - | 0.2 | 115 / 467,025 |
| year_2015_2020 | 3798 | 3.3 | - | 16.8 | 3,563 / 1,866,607 | - / 168.8 | 0.1 | - | 0.3 | 143 / 467,025 |
| provider_kunmanga | 706 | 16.6 | 41.4 | 17.9 | 218,768 / 1,866,607 | 6.2 / 186.1 | 2.1 | - | 2.3 | 287 / 467,025 |
| provider_missing | 0 | 14.0 | - | 15.3 | 218,768 / 1,866,607 | 5.1 / 157.8 | 24.6 | 849 | 22.5 | 287 / 467,025 |
| minch_100 | 4782 | **140** | **442** | 98.1 | 198,507 / 1,866,607 | 4.1 / 165.5 | 0.6 | - | 0.6 | 360 / 467,025 |
| minch_500 | 204 | 87.3 | - | 91.6 | 198,507 / 1,866,607 | 4.5 / 154.0 | 14.7 | **985** | 13.7 | 360 / 467,025 |
| tag_romance | 19604 | **273** | **376** | 204 | 808,529 / 1,866,607 | 37.7 / 161.2 | 0.6 | - | 0.6 | 826 / 467,025 |
| tag_romance, page at offset 2400 | | | | | | | 52.1 | - | 42.4 | 83,360 / 467,025 |
| tag_rare | 200 | **253** | - | 202 | 808,529 / 1,866,607 | 40.0 / 160.0 | 44.1 | **954** | 36.6 | 826 / 467,025 |
| tags_two | 8076 | **276** | - | 211 | 807,721 / 1,866,607 | 37.7 / 153.1 | 2.0 | - | 1.5 | 825 / 467,025 |
| exc_romance | 32926 | 25.2 | 81.8 | 153 | 233,314 / 1,866,607 | 5.1 / 144.8 | 6.9 | - | 0.3 | 300 / 467,025 |
| tracked_yes | 859 | 15.6 | - | 38.7 | 83,415 / 1,866,607 | - / 148.9 | 0.8 | - | 1.1 | 163 / 467,025 |
| tracked_no | 51671 | 18.0 | 46.0 | 41.8 | 83,415 / 1,866,607 | - / 150.5 | 0.2 | - | 0.2 | 163 / 467,025 |
| q_common_love | 3600 | 43.1 | 239 | 41.6 | 9,825 / 75,902 | - / - | 93.2 | 472 | 94.7 | 20,310 / 75,919 |
| q_rare_title | 49 | 23.0 | - | 23.6 | 187 / 75,902 | - / - | 25.9 | - | 27.2 | 367 / 75,919 |
| q_short_yu | 212 | 51.5 | 177 | 54.1 | 869 / 75,902 | - / - | 59.2 | - | 55.5 | 1,812 / 75,919 |
| q_long | 6 | 32.8 | - | 32.0 | 530 / 75,902 | - / - | 33.5 | - | 32.3 | 710 / 75,919 |
| q_love_recency | 3600 | 42.4 | - | 43.1 | 9,825 / 75,902 | - / - | 41.8 | - | 44.7 | 10,018 / 75,909 |
| q_love_chapters | 3600 | 42.2 | - | 42.5 | 9,825 / 75,902 | - / - | 51.0 | - | 54.3 | 25,024 / 75,930 |
| combo_manhwa_romance_minch | 616 | 31.9 | 359 | 28.3 | 998,865 / 1,866,607 | 67.2 / 153.6 | 0.8 | - | 1.0 | 59,364 / 467,025 |
| combo_completed_exc_tracked_no | 2738 | 12.5 | - | 45.3 | 51,439 / 1,866,607 | - / 147.5 | 7.2 | - | 0.5 | 3,778 / 467,025 |
| combo_prov_tag_year | 195 | 16.0 | - | 17.5 | 462,650 / 1,866,607 | 7.5 / 152.5 | 3.6 | - | 3.5 | 4,240 / 467,025 |
| combo_q_love_romance_manhwa | 246 | 43.2 | - | 41.5 | 74,207 / 75,902 | - / - | 48.5 | - | 47.6 | 74,686 / 75,919 |
| sort_title | 52530 | 15.2 | - | 16.5 | 8,329 / 1,866,607 | - / 141.7 | 0.1 | - | 31.7 | 91 / 1,866,685 |
| sort_title, page at offset 2400 | | | | | | | 8.8 | - | 52.3 | 9,176 / 1,866,685 |
| sort_chapters | 52530 | 15.1 | - | 17.1 | 8,329 / 1,866,607 | - / 159.4 | **109** | **382** | 117 | 195,311 / 1,866,685 |
| sort_sources | 52530 | 14.6 | - | 16.8 | 8,329 / 1,866,607 | - / 144.7 | **124** | - | 135 | 195,968 / 1,866,685 |
| sort_year | 52530 | 15.4 | - | 22.7 | 8,329 / 1,866,607 | - / 149.8 | 25.0 | - | 30.1 | 9,750 / 1,866,685 |
| sort_chapters_romance | 19604 | **275** | - | 208 | 808,529 / 1,866,607 | 37.3 / 146.0 | **324** | **790** | 262 | 902,105 / 1,866,685 |

**Plan cache under `plan_cache_mode=auto`:** after six executions of the same prepared statement,
every one of the 62 statement/shape pairs was still on a **custom** plan. The generic estimates are
20–200× the custom ones, so the auto heuristic never switches. The `$n IS NULL OR …` generic-plan
cliff that hit the console **does not occur on these statements**; its only effect is the estimate
(and JIT, if `jit` were ever turned on: 140–200 ms per statement under a generic plan).

## 3. Root causes

1. **Every count reads the whole `series` heap.** The unfiltered count is a seq scan of 7 659 pages
   (60 MB) — `series` rows average ~1.1 KB on the heap because of `description` and the stored
   `search_vec`. Filters are `Filter`s on that scan, never index conditions: under `OR` guards the
   sublinks cannot be pulled up into joins, so even a 706-row provider filter reads all 7 659 pages.
   Cold that is 40–60 ms locally; on production storage inside a 1 GB container competing with the
   crawler, this is the part that scales to seconds.
2. **Include-tags is a correlated set operation per row.** `NOT EXISTS (unnest($7) EXCEPT SELECT
   slug …)` cannot become a hashed subplan: `SubPlan 1` runs 52 530 times, each an index probe on
   `series_tags_pkey` plus one `tags_pkey` probe per tag — **755 368 buffer touches**, 250–276 ms
   warm regardless of how many rows match (200 or 19 604). Pure CPU; slower CPUs and concurrency
   scale it linearly.
3. **Min-chapters is a correlated aggregate per row.** `(SELECT max(chapter_count) … WHERE
   series_id = s.id) >= $6`: 52 530 aggregate executions, 243 599 buffers, 87–140 ms warm, 442 ms
   cold (it reads `series_sources` in random order).
4. **Sort by chapters/sources evaluates the same correlated aggregates for every matching row before
   the top-N sort** (195 k cost, 109–124 ms warm, 382 ms cold; 324/790 ms combined with a tag).
5. **Selective filters on the recency page walk `series_updated_idx` until 24 rows pass.** A rare
   tag, a high chapter floor or an unknown provider inspects most of the table in index order with a
   random heap fetch per row: 849–985 ms cold for 0–204 matches.
6. **Request volume multiplies all of it.** Discover recounts on every scroll page with identical
   filters; the search screen and two console typeaheads count and discard. The count is ≥ the page's
   cost on every non-search shape.
7. **Search**: count and page each rebuild the `matched` set (trigram bitmap recheck, 23–52 ms warm,
   177–239 ms cold each). That is the documented lossy-GIN recheck, not the filter predicate, and it
   is paid twice per request.

Exclude-tags, provider and tracking already plan as hashed subplans (one scan of the subquery, then
a hash probe per row); their cost is cause 1, not their own.

## 4. Options, with prototype numbers

Equivalence for every prototype: a script runs the committed statement and the prototype for all
31 workload shapes plus 15 edge shapes (unknown tag, duplicate tags, known+unknown tag, unknown
exclude, include and exclude the same tag, `min_chapters` 0 / −5 / 1, adult + tracked, `ymax` only,
chapters sort + floor, sources sort + provider, year sort + search, title sort + tag, relevance +
tag). Each page runs with `LIMIT 100000`, so the **entire ordered result** is compared byte for
byte, plus the count. **P1: 46/46 identical. P2: 46/46. P3: 46/46.**

### O1. Custom plans on the reader pool

Measured: no effect. `auto` already stays on custom plans for all 62 pairs (§2). Forcing it adds
planning (0.1–1 ms) to every hot-path statement. **Not recommended** for browse. It would matter only
if a rewrite makes generic and custom estimates close — which P3 does for search pages, where the
two measured the same (101 vs 105 ms).

### O2 (P1). Rewrite the two correlated predicates, no schema change

`min_chapters` → `$6 <= 0 OR s.id IN (SELECT series_id FROM series_sources WHERE chapter_count >= $6)`;
include-tags → `s.id IN (SELECT series_id … WHERE slug = ANY($7) GROUP BY series_id HAVING count(*) =
(SELECT count(DISTINCT x) FROM unnest($7) x))`. Both become hashed subplans.

| | now | P1 |
|---|---|---|
| count tag_romance warm / cold | 273 / 376 | 32 / 127 |
| count minch_100 warm / cold | 140 / 442 | 29 / 68 |
| count combo_manhwa_romance_minch cold | 359 | 166 |
| page sort_chapters_romance warm | 324 | 75 |
| page tag_rare cold | 954 | 673 |

Still reads the full `series` heap per count (cause 1), leaves causes 4 and 5, and regresses some
pages (tag page 0.6 → 11.5 ms, tags_two page 2.0 → 45.6 ms). Worse: its **generic** plans for the
search pages are pathological — `q_common_love` page 94 → **13 110 ms**, `q_love_chapters` 54 →
**13 825 ms** — kept away only by `auto`'s cost comparison. **Rejected**: a plan-cache heuristic
between the service and a 13 s page is not a fix.

### O3 (P2/P3). Maintained narrow projection `series_browse`

One row per series with exactly the filter and sort keys, kept by triggers:

```sql
series_browse (series_id uuid PK → series ON DELETE CASCADE, updated_at, content_type, status,
               release_year, adult_gated, canonical_title, max_chapters int, source_count int,
               provider_ids uuid[], tag_ids uuid[])
-- btree (updated_at DESC, series_id DESC), (max_chapters DESC, updated_at DESC, series_id DESC),
-- (source_count DESC, …), (canonical_title, updated_at DESC, series_id DESC),
-- (release_year DESC NULLS LAST, …); GIN (tag_ids), GIN (provider_ids)
```

Predicate on the projection (same `$1`–`$11`, same types):

```sql
($5::text IS NULL OR sb.provider_ids @> ARRAY[COALESCE((SELECT id FROM providers WHERE slug = $5), NIL)])
($6::int IS NULL OR sb.max_chapters >= $6)
(cardinality($7) = 0 OR sb.tag_ids @> ARRAY(SELECT COALESCE(t.id, NIL) FROM unnest($7) x LEFT JOIN tags t ON t.slug = x))
(cardinality($8) = 0 OR NOT sb.tag_ids && ARRAY(SELECT id FROM tags WHERE slug = ANY($8)))
```

`NIL` is the all-zero uuid, which no row carries. It is load-bearing: `ARRAY(SELECT t.id FROM tags
WHERE slug = ANY($7))` silently drops an unknown slug, so `tag=romance&tag=typo` would return every
romance series instead of none (edge case `edge_known_plus_unknown_tag`).

- **P2** joins `series` for the projection and sorts the joined rows.
- **P3** orders and limits on the projection alone, then looks up the 24 `series` rows (late row
  lookup). This fixed P2's deep-offset plans: the `@>` estimate is a default (249 rows vs 19 604
  real) and P2 joined 19 604 wide rows before its top-N (tag_romance offset 2400: 57 ms, 78 k buffers).

Warm (ms), now / P1 / P3:

| shape | count ms: now / P1 / P3 | count cost: now / P3 | page ms: now / P1 / P3 | page generic ms: now / P1 / P3 |
|---|---|---|---|---|
| unfiltered | 16.05 / 18.29 / **5.75** | 8,329 / 2,258 | 0.09 / 0.09 / **0.06** | 0.12 / 0.13 / 0.12 |
| adult_on | 3.14 / 2.91 / **3.28** | 1,017 / 1,175 | 0.09 / 0.13 / **0.09** | 0.12 / 0.15 / 0.11 |
| type_manhwa | 13.18 / 16.14 / **4.18** | 8,338 / 2,268 | 0.24 / 0.17 / **0.16** | 0.24 / 0.2 / 0.19 |
| type_unknown | 15.18 / 19.29 / **5.71** | 8,438 / 2,367 | 5.45 / 6.78 / **2.5** | 6.03 / 9.79 / 2.37 |
| status_completed | 4.33 / 6.77 / **4.6** | 5,375 / 2,282 | 0.15 / 0.08 / **0.07** | 0.16 / 0.14 / 0.13 |
| year_2015_2020 | 3.33 / 3.82 / **1.2** | 3,563 / 1,733 | 0.14 / 0.22 / **0.1** | 0.26 / 0.23 / 0.13 |
| provider_kunmanga | 16.6 / 19.62 / **0.56** | 218,768 / 275 | 2.09 / 2.6 / **0.61** | 2.27 / 2.72 / 0.75 |
| provider_missing | 13.97 / 17.1 / **0.04** | 218,768 / 275 | 24.63 / 23.35 / **13.21** | 22.46 / 25.55 / 14.36 |
| minch_100 | 140.1 / 28.65 / **1.19** | 198,507 / 1,736 | 0.58 / 9.1 / **0.19** | 0.58 / 9.06 / 0.24 |
| minch_500 | 87.27 / 25.91 / **0.18** | 198,507 / 215 | 14.74 / 13.33 / **0.18** | 13.69 / 13.72 / 2.27 |
| tag_romance | 273.33 / 31.64 / **4.24** | 808,529 / 275 | 0.6 / 11.54 / **0.12** | 0.57 / 44.12 / 0.31 |
| tag_rare | 252.99 / 24.06 / **0.23** | 808,529 / 275 | 44.12 / 8.03 / **1.62** | 36.61 / 44.98 / 2.19 |
| tags_two | 275.72 / 49.74 / **3.35** | 807,721 / 278 | 1.99 / 45.57 / **0.28** | 1.49 / 42.98 / 0.31 |
| exc_romance | 25.22 / 30.54 / **8.05** | 233,314 / 2,394 | 6.87 / 7.48 / **0.08** | 0.32 / 0.39 / 0.15 |
| tracked_yes | 15.61 / 18.56 / **6.91** | 83,415 / 77,344 | 0.79 / 1.47 / **0.82** | 1.08 / 0.83 / 1.15 |
| tracked_no | 17.99 / 18.87 / **8.3** | 83,415 / 77,344 | 0.25 / 0.26 / **0.24** | 0.17 / 0.28 / 0.15 |
| q_common_love | 43.1 / 42.78 / **44.75** | 9,825 / 6,258 | 93.18 / 105.39 / **105.25** | 94.71 / 13110.44 / 101.62 |
| q_rare_title | 23.04 / 24.22 / **24.66** | 187 / 189 | 25.95 / 24.9 / **25.51** | 27.2 / 284.66 / 26.22 |
| q_short_yu | 51.47 / 51.68 / **50.48** | 869 / 841 | 59.21 / 52.47 / **54.0** | 55.45 / 970.68 / 55.82 |
| q_long | 32.79 / 33.98 / **33.34** | 530 / 532 | 33.51 / 31.52 / **32.45** | 32.32 / 120.02 / 34.48 |
| q_love_recency | 42.37 / 52.37 / **43.43** | 9,825 / 6,258 | 41.78 / 44.28 / **45.74** | 44.7 / 66.65 / 42.83 |
| q_love_chapters | 42.2 / 47.53 / **41.69** | 9,825 / 6,258 | 50.98 / 54.95 / **41.93** | 54.28 / 13825.03 / 40.01 |
| combo_manhwa_romance_minch | 31.89 / 46.27 / **2.95** | 998,865 / 233 | 0.8 / 23.61 / **3.13** | 1.02 / 60.42 / 0.56 |
| combo_completed_exc_tracked_no | 12.49 / 15.22 / **6.94** | 51,439 / 77,560 | 7.18 / 7.75 / **0.24** | 0.46 / 0.59 / 0.25 |
| combo_prov_tag_year | 15.99 / 28.34 / **1.01** | 462,650 / 17 | 3.58 / 14.77 / **0.95** | 3.52 / 47.4 / 2.02 |
| combo_q_love_romance_manhwa | 43.2 / 65.87 / **38.66** | 74,207 / 4,215 | 48.49 / 70.19 / **42.39** | 47.63 / 313.79 / 47.1 |
| sort_title | 15.19 / 18.41 / **6.61** | 8,329 / 2,258 | 0.12 / 0.21 / **0.11** | 31.66 / 34.88 / 15.24 |
| sort_chapters | 15.05 / 17.74 / **5.64** | 8,329 / 2,258 | 109.08 / 110.64 / **8.07** | 116.6 / 117.18 / 13.38 |
| sort_sources | 14.61 / 16.59 / **5.83** | 8,329 / 2,258 | 124.46 / 136.15 / **8.58** | 135.12 / 177.0 / 14.28 |
| sort_year | 15.44 / 15.84 / **6.61** | 8,329 / 2,258 | 25.03 / 26.82 / **0.15** | 30.06 / 32.1 / 13.9 |
| sort_chapters_romance | 275.14 / 33.46 / **4.36** | 808,529 / 275 | 323.78 / 75.11 / **5.94** | 261.84 / 107.53 / 12.12 |

Deep pages (offset 2400), P3: unfiltered 6.7 → 1.1 ms, tag_romance 52.1 → 6.1, sort_title 8.8 → 1.5.
`sort_chapters` still sorts (8 ms) because the prototype's `CASE … ::int8` cast hides
`series_browse_chapters_idx`; dropping the cast should make it an index walk (not measured).

Cold (ms; restart + drop caches, one execution each):

| statement | now | P1 | P2 | P3 |
|---|---|---|---|---|
| count unfiltered | 40.2 (7 659 blocks) | – | 17.0 | **12.5 (1 588 blocks)** |
| count tag_romance | 376 | 127 | 22.7 | **10.6** |
| count minch_100 | 442 | 68 | 211¹ | **7.9** |
| count provider_kunmanga | 41.4 | – | 48.1 | – |
| count exc_romance | 81.8 | – | 17.9 | – |
| count tracked_no | 46.0 | – | 15.0 | **18.5** |
| count combo_manhwa_romance_minch | 359 | 166 | 26.1 | **11.5** |
| count q_common_love | 239 | – | 197 | – |
| page unfiltered | 8.8 | – | 10.5 | **8.1** |
| page sort_chapters | 382 | 403 | 29.1 | **23.3** |
| page sort_chapters_romance | 790 | – | – | **23.9** |
| page tag_rare | 954 | 673 | 121 | **190** |
| page minch_500 | 985 | – | – | **17.1** |
| page provider_missing | 849 | – | – | **199** |
| page q_common_love (relevance) | 472 | – | – | 360 |

¹ P2 was measured before the projection was `VACUUM FULL`ed and re-analysed for P3; the plan read
the chapters index then the heap in random order. Not re-run under P2.

Generic plans under P3: counts cost 79 054 (below `jit_above_cost`), no-search pages 159 534
(recency) / 79 057 (token). `auto` still chose custom everywhere except the `q_common_love` relevance
page, where generic and custom measured 101.6 vs 105.3 ms.

Size: projection heap 12 MB, 29 MB with seven indexes (vs `series` heap 60 MB + 63 MB TOAST).
Backfill 0.78 s (plus 0.54 s for `canonical_title`), indexes ~0.15 s.

**Write path** (each statement in a rolled-back transaction, with triggers vs
`session_replication_role = replica`; row-level triggers):

| writer | without | with | added |
|---|---|---|---|
| `update_source_scan`, chapter_count changed | 0.06 ms | 2.18 ms | +1.5 ms (series_browse trigger; the existing `watchlist_unread` trigger is 0.5 ms of the rest) |
| `update_source_scan`, chapter_count unchanged | 0.04 ms | 0.26 ms | 0 from series_browse (`WHEN` filters it); the 0.19 ms is `watchlist_unread` |
| enrichment: 12 tags for one series, one statement | 0.08 ms | 1.15 ms | +0.8 ms |
| merge: move 6 sources | 0.16 ms | 1.51 ms | +1.0 ms |
| series metadata update (`updated_at`, `status`) | 0.16 ms | 0.71 ms | +0.17 ms |
| series insert | 0.11 ms | 0.55 ms | +0.27 ms |
| delete one tag from 20 754 series (vocabulary cleanup) | 16 ms | 391 ms | +372 ms; a statement-level variant with a transition table measured 562 ms, so row-level stays |
| full drift check (`series` vs recomputed projection) | | 177 ms | 0 drift rows after backfill and after the writers above |

Code cost: one migration (table, 5 btree + 2 GIN indexes, ~8 triggers, 3 functions, backfill),
the six statements rewritten (the `browse_statement!` macro keeps one predicate copy), a reconciler
pass in the control plane on the `watchlist_unread` pattern (drift metric + repair), and new
`SERIES_REFERENCES` handling in `repo_matching`. No API or frontend change.

### O4. Count once per filter set

Measured from code, not from traffic: a Discover scroll session issues one count per page fetch,
e.g. 25 counts for a 600-item window at 24/page, all with identical filters; the search screen and
both console typeaheads count on every request and discard it.

- **Frontend asks for the total only once per window.** Needs the API to be able to skip the count,
  and `X-Next-Cursor` to stop depending on it: fetch `limit + 1` rows and set the cursor iff the
  extra row exists (exact, and cheaper than the count on every shape). Skipping `X-Total-Count`
  unconditionally on `page > 0` would break an **older desktop installer**, which falls back to
  displaying the page length as the total. Hence an opt-out parameter (e.g. `with_total=false`,
  default `true`): additive, old clients unchanged, `openapi.json` gains one optional parameter.
- **API cache of totals keyed by the normalised filter with a short TTL** (process-local, like
  `cache.rs`). No contract change, but the total can lag by the TTL — visibly for `tracking` right
  after a watchlist change — and the cursor must move to `limit + 1` anyway. With O3 every count is
  1–10 ms warm and ≤ 20 ms cold, so this buys little. **Not recommended now.**
- **A cursor carrying the total** (opaque token): contract change to `X-Next-Cursor`'s type
  (currently `i64`), deep links use `at=` item indexes and would still need a first count. **Not
  recommended.**

### O5. Count from the search branch's matched set

Fold the total into the page as `count(*) OVER ()` for the search statements only (the matched set
is bounded, and relevance order sorts all of it anyway). Warm page cost changes by −2 to +8 ms
(`q_common_love` 93 → 101, `q_love_recency` 42 → 44), and the separate count (23–52 ms warm,
177–239 ms cold) disappears. Cold, the combined statement measured 366 ms vs 472 (page) + 239
(count) today. Exact. Needs a fallback count when the requested page is past the end (no row to
carry the window). Not applied to non-search statements: there the window forces full
materialisation before `LIMIT`, which is what the existing doc comment rejects.

### O6. Capped count / estimate

Not needed: exact totals are cheap for every measured shape under O3. No shape was found where
exactness has to be given up.

## 5. Recommended plan

1. **`series_browse` projection + P3 statements.** *No feature/contract change.* Removes causes 1–5:
   worst warm count 276 → 8 ms, worst cold count 442 → 19 ms, chapters/sources sort 109–124 → 8 ms,
   worst cold page 985 → 199 ms (unknown provider; next-worst 190 ms rare tag). Includes: `WHEN`
   clauses so an unchanged scan costs nothing; `canonical_title` in the series trigger's `WHEN`
   (`repo::matching::keys` and `merge_metadata` write titles; the prototype backfilled the column but
   did not maintain it); the NIL-uuid mapping for unknown include slugs; a control-plane reconciler
   reporting drift. Lower the `repo_query_plans` budget for the browse pages (generic 159 534 on
   `tv_perf`; the fixture is ~1/3 the size, so re-derive the ceiling from the audit's own run), and
   check whether the counts leave the budget entirely.
2. **`X-Next-Cursor` from `limit + 1`; optional `with_total=false`.** *Contract change, additive.*
   Discover sends `with_total=false` on every request after the first of a window and keeps the
   total it has; search and the console typeaheads always send it. Regenerate `openapi.json` /
   `api-client` (`cargo run -p xtask -- openapi`). The route's operation id is unchanged, so the
   access matrices need no new row; run `me_access_matrix` anyway since the published document moves.
3. **Search statements carry the total as a window count** when `with_total` is set, with a fallback
   count for an out-of-range page. *No feature/contract change.* Halves trigram work per search
   request.
4. **Revisit only with production data**: a total cache (O4), a dedicated narrow title table for the
   trigram recheck (memory note on the lossy GIN, the remaining 180–360 ms cold search cost).

Not recommended: reader-pool custom plans (O1, no measured effect), P1 (13 s generic search pages),
capped/estimated counts (O6).

## 6. Tests that should pin it

Each with a doc comment naming the bug, per the repo's rule 8.

- `crates/db/tests/repo_browse.rs`
  - `the_browse_projection_follows_every_writer` — after each writer (source insert/delete, scan with
    a changed `chapter_count`, merge moving sources, disentangle/undo moving them back, tag
    insert/delete incl. undo's `jsonb_populate_recordset` re-insert, `merge_metadata`, `adult_inferred`,
    `normalized/canonical_title` rewrite, series delete) the projection equals a recomputation from
    `series`/`series_sources`/`series_tags`. Bug pinned: a writer the triggers do not see makes
    Discover filter and sort on stale keys with no error.
  - extend `tags_require_all_and_exclude_tags_remove_any` with an unknown slug next to a known one
    (must match nothing) and a duplicated slug. Bug pinned: `@> ARRAY(SELECT id … WHERE slug = ANY)`
    drops unknown slugs and widens the result.
  - extend the `min_chapters` test with `0` and a negative floor keeping a sourceless series. Bug
    pinned: `max_chapters >= $6` vs the old `COALESCE(max, 0) >= $6` for series with no sources.
  - `every_page_and_count_statement_selects_the_same_rows` stays as the differential; add the
    window-count search statement and the `limit + 1` cursor to its matrix.
- `crates/db/tests/repo_query_plans.rs`
  - `the_browse_statements_read_the_projection` — no `series_tags`, `series_sources` or `tags` node
    under a per-row `SubPlan` in any browse count or page plan, and the count plans do not scan
    `series`. Bug pinned: correlated `EXCEPT` / `max()` per series row (755 k buffers for one count).
    Shape rule, not a timing, as the audit already does for the reading surfaces.
  - lower the browse `Budget` ceiling; the existing stale-budget check then catches a regression.
- `crates/db/tests/repo_matching.rs`: `SERIES_REFERENCES` entry for `series_browse` (cascade).
- `services/api` integration test: `with_total=false` omits `X-Total-Count`, `X-Next-Cursor` is
  present exactly when another row exists, including when the result size is an exact multiple of
  `limit`; without the parameter both headers behave as today. Bug pinned: a cursor derived from a
  count that was not taken ends the list one page early or offers an empty page.
- `web/frontend` (`cargo test --bin tankovault`): a page response without `x-total-count` keeps the
  window's total. Bug pinned: the current `unwrap_or_else(|| r.len())` fallback would show
  "1–24 of 24".
- Control plane: the reconciler's drift counter, on the `watchlist_unread_drift_total` pattern.

Suites to run for this change (Docker): `repo_browse`, `repo_query_plans` (~13 min, background),
`repo_matching`, `me_access_matrix`; plus `xtask sqlx-prepare` and `xtask openapi`.

## 7. Open questions

- **Production has not been measured.** The local cold worst case is ~1 s; production reports 1–30 s.
  The gap is assumed to be storage/memory pressure and concurrency multiplying the block reads and
  buffer touches in §3; `2026-09-prod-diagnostics.sql` plus `pg_stat_statements` for these seven
  query ids would confirm which shapes dominate there.
- Is production actually `jit=off` (compose says so; an earlier note says stock defaults)? With JIT on,
  today's tag/min-chapters counts pay 4–67 ms of JIT on custom plans.
- `max_chapters` keeps today's "richest carrier" semantics. The projection is the natural place for
  the card's union count that `browse.rs` says it wants; changing it is a visible sort/filter change
  and is out of scope here.
- Trigger shape: row-level measured cheapest for single-series writes; a bulk tag cleanup pays
  ~18 µs per affected series. Acceptable for migrations/operator actions? Any bulk writer running on
  the ingest path should be checked.
- Parameter spelling for step 2 (`with_total`, `count=none`, …) and whether the console typeaheads
  should move to a lighter endpoint instead.
- Not measured: `SeriesSummary::page`'s three batched reads, API-level latency, pool contention with
  page and count on separate connections, and the dropping of `::int8` on the chapters/sources sort keys.
