# Suggestion system — design

Status: **phase 0 implemented; phases 1–4 proposed.**

| Phase | State |
|---|---|
| 0 — pgvector, widened signals, `series_merges`, the merge guard | **built** (§5.1, §9.2, §9.3, §9.6) |
| 1 — the item model: features, SVD, embeddings, HNSW | not started |
| 2 — the reader model, and replacing the stub endpoint | not started |
| 2.5 — the `Tunable` registry and the console | not started |
| 3 — collaboration and feedback | not started |

What phase 0 actually landed, against what this document specifies: the `vector` extension and
the image move; `tags.kind`/`series_count`, `series_tags.weight`/`source`,
`series.is_adult`/`external_score`/`external_popularity` as **columns only** — nothing populates
them yet, because that is the AniList selection change in the same phase and it is *not* done;
`series_merges` with path compression and both resolvers; and the differential test that holds
`merge_series` to the schema.

**The stub recommender in `tracking::dashboard::recommendations` is untouched and still serving.**
Nothing below §5.1 exists in code.

What exists today is a stub: `tracking::dashboard::recommendations` selects every series sharing
one tag with anything on the watchlist and orders by the shared-tag count, computed twice — once
in the `WHERE EXISTS`, once in the `ORDER BY` — before the `LIMIT`. It carries a 700 000 cost
budget in `crates/db/tests/repo_query_plans.rs` and the API handler documents itself as a stub.
At a catalogue of ~1M series that query reads the catalogue, per request, per user.

The target this document designs for:

| Property | Target |
|---|---|
| Catalogue size | 1 000 000 series, ~30M chapters |
| Shelf latency | p50 < 15 ms, p99 < 50 ms, **independent of catalogue size** |
| Similar-series latency | p99 < 10 ms |
| Item model rebuild (full) | < 45 min, resumable, dominated by the HNSW build (§6.4) |
| Item model rebuild (incremental, 10k changed) | < 2 min — re-embed changed digests, insert into the live index |
| Builder peak heap | single-digit MB, **independent of catalogue size** (§3.2, §6.4) |
| Item model storage | ~256 MB embeddings + HNSW graph at 1M series |
| Merge → shelf correctness | immediate; no window where a merged series can be recommended |

---

## 1. The thesis

**The catalogue is never scanned per request. Everything expensive is either precomputed or
answered by an index built for it.**

Every design decision below follows from one observation: the expensive half of a recommender is
item-side (which of a million series resemble each other), the cheap half is user-side (which
handful of them does this reader care about). The item side does not change per request. So:

- Offline, a builder turns each series into a sparse feature vector, projects those into a dense
  space, and indexes them (HNSW over pgvector).
- Online, a request runs a small, fixed number of **sublinear ANN searches** — one from the
  reader's taste profile, one per seed — plus a few index lookups, scores ≤ 1 000 candidates in
  Rust, and returns 12.

Neither half's cost grows linearly with the catalogue. That is the whole property being bought.

The current query does the opposite — it puts an item-side join on the request — which is why it
costs what it costs. Every alternative that keeps a set-similarity join in SQL (`ORDER BY
count(shared tags)`, `ORDER BY embedding <-> embedding` **without** an ANN index, a lateral over
`series_tags`) inherits the same defect at a different constant factor. Note the middle one:
pgvector alone does not fix this. An unindexed `<=>` sorts the entire catalogue, and it looks
exactly like the fixed version until the row count grows.

---

## 2. What the system can see today, and what is missing

### 2.1 Present

| Signal | Source | Quality |
|---|---|---|
| Watchlist status | `watchlist_entries.status` (`reading`/`planned`/`completed`/`dropped`/`paused`) | Strong. `dropped` is the only negative signal in the product. |
| Reading depth | `read_progress.last_read_whole_number` | Strong. Absolute chapters read is the best proxy for "this held me". |
| Recency | `watchlist_entries.updated_at`, `read_progress.updated_at` | Good. Enables taste drift. |
| Tags | `series_tags` → `tags` | **Thin.** See §2.2. |
| Authors | `series_authors` → `authors` | Good, but sparse — populated only where AniList returned staff. |
| Content type | `series.content_type` (`manga`/`manhwa`/`manhua`/`webtoon`) | Good, coarse but highly predictive of taste. |
| Status, release year | `series.status`, `series.release_year` | Weak alone, useful as filters and mild features. |
| Length / cadence | `chapters` per source, `discovered_at` | Derivable. Length preference is real and cheap to model. |
| Source breadth | `count(series_sources)` | A popularity proxy — a series carried by six providers is a series people read. |
| Description | `series.description`, `series.search_vec` | Weak signal, high noise. Low priority (§13). |
| External lists | `sync_remote_entries` (AniList) | Second interaction source, **including entries with no local match** — which is a cold-start signal for series we do not carry. |

### 2.2 Missing, and worth adding first

The single biggest quality lever is not the algorithm — it is the feature vocabulary. Today
`series_tags` holds **AniList genres only** (`genres` in
`services/sync/src/providers/anilist/graphql.rs`), a vocabulary of roughly 20 terms. "Action" and
"Fantasy" describe a third of the catalogue each; a recommender built on them cannot say anything
specific, and its neighbour lists will be dominated by whatever is popular.

Add to the AniList media selection — all free, same query, no extra requests:

```graphql
tags { name rank isMediaSpoiler }   # ~600-term vocabulary, rank 0..100
averageScore                        # quality prior
popularity                          # appeal prior
isAdult                             # a hard gate, not a feature
source                              # ORIGINAL | LIGHT_NOVEL | WEB_NOVEL | ...
relations { edges { relationType node { id } } }   # sequels/prequels/side stories (§11.4)
```

`tags` with `rank` turns a 20-term vocabulary into a ~600-term weighted one. That is the
difference between "shares Action" and "shares Regression, Dungeon, Male Protagonist, Weak to
Strong" — which is both a better neighbour and a usable explanation.

`isMediaSpoiler` tags are excluded from anything user-visible but kept for scoring.

**Not present anywhere and not proposed:** an explicit rating. The product has no rating column
and this design does not add one — implicit feedback (§4) is sufficient and does not require
asking users to do work. AniList's per-entry `score`, when a reader has linked an account, is the
one explicit signal available and is folded in at §4.3.

---

## 3. Architecture

```
                     ┌───────────────────────────────────────────┐
                     │  control-plane (leader-elected scheduler)  │
                     │  publishes  recsys.build.{stage}          │
                     └───────────────┬───────────────────────────┘
                                     │ NATS
                     ┌───────────────▼───────────────────────────┐
                     │  worker — the builder (§6)                │
                     │  A features → B idf → C neighbours        │
                     │  D co-occurrence → E priors               │
                     └───────────────┬───────────────────────────┘
                                     │ COPY / upsert
   ┌─────────────────────────────────▼──────────────────────────────────┐
   │ Postgres: rec_features, series_features, series_embedding (HNSW),   │
   │           series_cooccurrence, series_prior,                        │
   │           user_series_affinity, user_taste_profile, …               │
   └─────────────────────────────────┬──────────────────────────────────┘
                                     │ bounded, index-only reads
                     ┌───────────────▼───────────────────────────┐
                     │  api — retrieve → score → diversify (§7)  │
                     └───────────────────────────────────────────┘
```

### 3.1 Crate placement

| Unit | Where | Why |
|---|---|---|
| Feature extraction, weighting, similarity, top-K join, ranking, MMR | **new `crates/recsys`** | Pure functions over plain data. No `sqlx`, no `axum`. Proptest-able and benchable in isolation; the top-K join is the one piece that needs a criterion bench, and it cannot have one if it is welded to a repository. |
| Reads and writes for every table in §5 | `crates/db/src/repo/recsys/{items,users,build}.rs` | Matches the existing `repo/` module split. |
| The builder driver (staging, batching, progress) | `services/worker/src/recsys.rs` | Reuses the existing JetStream consumer, the pool, the health/metric wiring. |
| Scheduling | `services/control-plane` | Already leader-elected; a rebuild must be a singleton. |
| Serving | `services/api/src/me/recommendations.rs`, `services/api/src/series.rs` | — |
| DTOs | `crates/contracts/src/me.rs`, `catalogue.rs` | Mandatory: repository row structs must not carry `ToSchema` (see that module's header). |

### 3.2 Why the builder is in the worker and not its own service

Nothing in the pipeline is resident in proportion to the catalogue. Stage A streams batches;
stages B, D and E execute entirely in the database; stage C holds a ~360 KB projection basis and
one batch, and hands the expensive part — the HNSW build — to Postgres. Every write is a streamed
`COPY`. **Steady-state builder heap is single-digit megabytes.**

That removes the objection that would otherwise have decided this. A separate `services/recsys`
would be the tidier boundary, but it costs every self-hoster another container for a job that runs
nightly, and the isolation it buys is isolation from a memory profile the builder does not have.
Running it in the worker reuses the task queue and the leader-elected trigger with no new
mechanism.

The real resource question moved with the work: **the memory that matters is now Postgres's**
(`maintenance_work_mem` during the index build, and page cache to keep the graph hot), not the
builder's. Size the database, not the worker. §6.4 gives figures.

It still runs on `spawn_blocking` with a bounded pool so a CPU-bound stage cannot starve the task
consumer, and `recommendations.build.batch_size` bounds residency explicitly rather than by
assumption.

**Escape hatch:** if the profile collides in practice, the builder moves to `services/recsys`
unchanged — it is a driver over `crates/recsys` and owns no state. Keep that boundary clean so
the move stays mechanical.

---

## 4. Understanding the reader

### 4.1 Affinity

One number per `(user, series)` in `[-1, 1]`, materialised into `user_series_affinity`.

```
affinity = base(status) · engagement_scale · recency_decay  +  external_score_offset
```

**`base(status)`** — the product's own vocabulary, read literally:

| Status | Base | Reasoning |
|---|---|---|
| `completed` | `+1.00` | Finished it. The strongest available statement. |
| `reading` | `+0.80` | Currently held. |
| `paused` | `+0.35` | Ambivalent, not rejected. |
| `planned` | `+0.25` | Intent, not taste. A plan-to-read list is aspirational and full of things people never touch — it must not outweigh a series someone actually read 200 chapters of. |
| `dropped` | `-0.60 … -0.10` | See below. |

**`dropped` is not one signal.** Dropped at chapter 3 means "wrong for me". Dropped at chapter 150
means "I liked this for a long time and then it declined" — a *positive* taste signal about
everything except the ending. So:

```
dropped_base = -0.60 + 0.50 · engagement          # → -0.60 early, -0.10 deep
```

The naive `dropped = -1` is the classic mistake here and it actively poisons the profile of
anyone who reads long series.

**`engagement`** — absolute depth, log-scaled, not a fraction of total:

```
engagement = min(1, ln(1 + chapters_read) / ln(1 + 60))
```

A fraction punishes the reader of a 900-chapter ongoing series for being at chapter 300, which is
the opposite of what the data means. 60 is the knee: a reader who has cleared 60 chapters has
committed, and more chapters add nothing to the *classification*.

**`recency_decay`** — exponential with a floor:

```
recency_decay = max(0.30, 0.5 ^ (age_days / 180))
```

Taste drifts; a shelf built entirely on what someone read in 2019 is wrong. The `0.30` floor
exists because an all-time favourite is still evidence — an unfloored decay makes a dormant user's
profile collapse to noise.

**`external_score_offset`** — where an AniList account is linked and a score exists:

```
offset = 0.25 · clamp((score - user_mean_score) / 25, -1, 1)
```

Centred on the reader's own mean, because scoring habits differ by an order of magnitude between
users (some never go below 7). Requires adding `score` to the media-list selection (§2.2).

### 4.2 Taste profile

`user_taste_profile` holds two sparse vectors over the feature vocabulary:

- **positive** `p(u) = Σ_{i: a>0} a(u,i) · f(i)`, L2-normalised, truncated to the top 64 features.
- **negative** `n(u) = Σ_{i: a<0} |a(u,i)| · f(i)`, same treatment, top 32.

The negative vector is what makes "I dropped every isekai I ever opened" mean something. Without
it, a dropped series contributes nothing but a filter, and the system keeps recommending the genre
the reader has rejected four times.

The profile also carries `seeds uuid[]` — the top ~25 series by `|affinity|`, cached so the request
path does not re-rank the whole watchlist.

Derived preferences fall out of `p(u)` for free because they are features (§6.1): content type,
length bucket, series status (some readers only start completed series), decade.

### 4.3 Refresh

Affinity is a pure function of `watchlist_entries` + `read_progress` + `sync_remote_entries`,
so it is recomputed for one user in a single query. Trigger:

- On write to watchlist or progress → mark `user_taste_profile.built_at` stale (no recompute
  inline; the write path stays fast).
- The API recomputes lazily at serve time when stale, budgeted (it is one indexed query over the
  user's own rows, single-digit ms for a 500-series list).

No background per-user sweep. It would do work for users who never sign in, and the lazy path is
cheap enough that precomputation buys nothing.

---

## 5. Schema

Three migrations, in this order. They follow the `.up.sql`/`.down.sql` pair convention of 0021+.

### 5.1 `0026_recsys_signals` — widen the vocabulary

```sql
-- pgvector becomes a hard dependency of the deployment, not an optional accelerator.
CREATE EXTENSION IF NOT EXISTS vector;
```

**This is a breaking change for every existing self-hosted deployment.**
`deploy/docker-compose.yml` ships `postgres:18-alpine`, which does not carry the extension; it
must become `pgvector/pgvector:pg18` (same upstream Postgres, extension preinstalled), and any
operator running their own Postgres has to install it before migrating. The migration fails
loudly rather than degrading, which is correct — a recommender that silently returns nothing is
worse than one that refuses to start.

Two consequences worth stating before this is committed to:

- It removes the "works on stock Postgres" property the project has today. That is a real cost
  for a self-hostable product, paid once, by every operator.
- `RELEASING.md` and `deploy/README.md` need the upgrade note, and the compose change should land
  in phase 0 so operators migrate before anything depends on it.

```sql
ALTER TABLE tags
  ADD COLUMN kind         text NOT NULL DEFAULT 'genre',  -- genre|theme|demographic|derived
  ADD COLUMN series_count int  NOT NULL DEFAULT 0;        -- cached df, written by stage B

ALTER TABLE series_tags
  ADD COLUMN weight real NOT NULL DEFAULT 1.0,            -- AniList rank/100; 1.0 for a genre
  ADD COLUMN source text NOT NULL DEFAULT 'provider';

ALTER TABLE series
  ADD COLUMN is_adult            boolean NOT NULL DEFAULT false,
  ADD COLUMN external_score      real,   -- AniList averageScore, 0..100
  ADD COLUMN external_popularity int;
```

`series_tags.weight` is why this migration is first: it changes the meaning of an existing table
that the enrichment path writes, and the feature extractor depends on it.

### 5.2 `0027_recsys_item_model`

```sql
-- Interned vocabulary. int4 ids, so a per-series vector stays small enough to live inline.
CREATE TABLE rec_features (
  id        int GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
  kind      text NOT NULL,        -- tag|author|content_type|country|status|decade|length|source
  value     text NOT NULL,
  doc_count int  NOT NULL DEFAULT 0,
  idf       real NOT NULL DEFAULT 0,
  UNIQUE (kind, value)
);

-- One row per series: its L2-normalised feature vector, feature ids ascending.
CREATE TABLE series_features (
  series_id   uuid PRIMARY KEY REFERENCES series(id) ON DELETE CASCADE,
  feature_ids int[]  NOT NULL,
  weights     real[] NOT NULL,
  digest      bytea  NOT NULL,   -- hash of the inputs; unchanged digest skips a rebuild
  built_at    timestamptz NOT NULL DEFAULT now()
);

-- The dense space. `halfvec` (fp16), not `vector`: 128 dims × 2 B = 256 B per row, so the whole
-- table is ~256 MB at 1M series and the HNSW graph stays cacheable. The precision loss is far
-- below what a ranking can resolve, and it is the difference between an index that lives in
-- memory and one that does not.
CREATE TABLE series_embedding (
  series_id  uuid PRIMARY KEY REFERENCES series(id) ON DELETE CASCADE,
  embedding  halfvec(128) NOT NULL,
  generation int NOT NULL,
  built_at   timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX series_embedding_hnsw
  ON series_embedding USING hnsw (embedding halfvec_cosine_ops)
  WITH (m = 16, ef_construction = 64);

-- Reader co-occurrence, thresholded (§12.2 explains the minimum support).
CREATE TABLE series_cooccurrence (
  series_id uuid NOT NULL REFERENCES series(id) ON DELETE CASCADE,
  other_id  uuid NOT NULL REFERENCES series(id) ON DELETE CASCADE,
  support   int  NOT NULL,
  score     real NOT NULL,
  PRIMARY KEY (series_id, other_id),
  CHECK (series_id <> other_id)
);

-- Appeal priors and the recommendable gate.
CREATE TABLE series_prior (
  series_id     uuid PRIMARY KEY REFERENCES series(id) ON DELETE CASCADE,
  prior         real    NOT NULL,             -- blended, [0,1]
  watchers      int     NOT NULL DEFAULT 0,
  velocity      real    NOT NULL DEFAULT 0,   -- decayed chapters/week
  recommendable boolean NOT NULL DEFAULT true,
  built_at      timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX series_prior_top_idx ON series_prior (prior DESC) WHERE recommendable;

-- Build bookkeeping. Single row, enforced.
CREATE TABLE rec_build_state (
  id           boolean PRIMARY KEY DEFAULT true CHECK (id),
  generation   int  NOT NULL DEFAULT 0,
  stage        text NOT NULL DEFAULT 'idle',
  cursor_id    uuid,                          -- resume point within a stage
  started_at   timestamptz,
  finished_at  timestamptz,
  series_built int  NOT NULL DEFAULT 0,
  error        text
);
```

**Every table here is keyed by `series_id` with a cascading foreign key, and that is now the
whole merge story for the item model.** An earlier draft precomputed top-K neighbours into a
`uuid[]` column, which cannot carry a foreign key; a merged series then survived as a dangling id
in thousands of other rows' arrays, with no reverse index to find them, which in turn required a
repair queue and an alias-resolution pass on the read path. Querying HNSW at request time deletes
that table and every consequence of it. §9 is much shorter than it was.

`series_features` keeps its own sparse vector as `int[]` + `real[]` — feature ids, not series ids,
so nothing dangles. It is read only by primary key, for scoring and explanations.

### 5.3 `0028_recsys_user_model`

```sql
CREATE TABLE user_series_affinity (
  user_id     uuid NOT NULL REFERENCES users(id)  ON DELETE CASCADE,
  series_id   uuid NOT NULL REFERENCES series(id) ON DELETE CASCADE,
  affinity    real NOT NULL,          -- [-1, 1]
  engagement  real NOT NULL,          -- [0, 1]
  observed_at timestamptz NOT NULL,
  PRIMARY KEY (user_id, series_id)
);
CREATE INDEX user_affinity_top_idx ON user_series_affinity (user_id, affinity DESC);

CREATE TABLE user_taste_profile (
  user_id         uuid PRIMARY KEY REFERENCES users(id) ON DELETE CASCADE,
  feature_ids     int[]  NOT NULL,
  weights         real[] NOT NULL,
  neg_feature_ids int[]  NOT NULL DEFAULT '{}',
  neg_weights     real[] NOT NULL DEFAULT '{}',
  seeds           uuid[] NOT NULL DEFAULT '{}',
  stale           boolean NOT NULL DEFAULT true,
  built_at        timestamptz NOT NULL DEFAULT now()
);

CREATE TABLE recommendation_feedback (
  user_id    uuid NOT NULL REFERENCES users(id)  ON DELETE CASCADE,
  series_id  uuid NOT NULL REFERENCES series(id) ON DELETE CASCADE,
  verdict    text NOT NULL,          -- 'not_interested' | 'hide_forever'
  created_at timestamptz NOT NULL DEFAULT now(),
  PRIMARY KEY (user_id, series_id)
);

CREATE TABLE user_recommendations (
  user_id    uuid PRIMARY KEY REFERENCES users(id) ON DELETE CASCADE,
  items      jsonb NOT NULL,   -- [{series_id, score, because:{series_id, shared:[…]}}]
  profile_at timestamptz NOT NULL,
  built_at   timestamptz NOT NULL DEFAULT now()
);
```

---

### 5.4 Defined elsewhere, in the sections that argue for them

Three further tables belong to this design but are specified where their reasoning lives, because
each exists to solve a problem stated there rather than a storage problem:

| Table | Migration | Defined in |
|---|---|---|
| `tunable_overrides` | `0029_recsys_tunables` | §8.5 |
| `series_merges` | `0026_recsys_signals` — **lands in phase 0** | §9.2 |
| `rec_repair_queue` | `0027_recsys_item_model` | §9.3 |

---

## 6. The offline builder

Five stages. Each is resumable from `rec_build_state.cursor_id`, each is idempotent, and each
writes under the current `generation` so a crash mid-build leaves a coherent older model rather
than a half-new one.

### 6.1 Stage A — feature extraction

Per series, emit a weighted sparse vector:

| Kind | Weight | Notes |
|---|---|---|
| `tag:<slug>` | `series_tags.weight × idf(tag)` | The bulk of the signal. AniList rank/100 for rich tags, 1.0 for a genre. |
| `author:<slug>` | `1.0 × idf(author)` | High idf by construction — an author is a near-unique feature and a strong recommender on its own. |
| `content_type:<t>` | `0.6` | Fixed, low idf. A hint, not a discriminator. |
| `country:<cc>` | `0.4` | Correlates with content type; kept because it separates manhwa-from-Korea from Korean-published manga. |
| `status:<s>` | `0.25` | Serves "only reads completed series". |
| `decade:<d>` | `0.25` | From `release_year`. |
| `length:<bucket>` | `0.35` | `oneshot` (<10) / `short` (<50) / `medium` (<200) / `long` (<600) / `epic`. Length preference is real and nothing else models it. |
| `source:<s>` | `0.4` | AniList `source` — light-novel adaptations cluster hard. |

Weights are TF-IDF-ish: the tabled value is the term weight, multiplied by `idf = ln(N / df)` from
stage B, then the vector is L2-normalised so a series with 40 tags does not outscore one with 8
purely by mass.

`digest` is a hash over the inputs (tag ids + weights, author ids, the scalar columns). An
unchanged digest short-circuits stages A and C for that series — this is what makes the
incremental build cheap, since the enrichment sweep touches `series.updated_at` far more often
than it changes anything a feature depends on.

**Fully streaming.** Extraction is a pure per-series function, so the stage is a keyset walk in
`series.id` order: read a batch by `COPY TO STDOUT`, extract, `COPY` the sparse vectors back,
drop the batch. Peak residency is one batch plus the feature-interning
map — and that map is the one genuinely resident structure in stage A, dominated by authors
(~200k distinct, ~20 MB). Everything else is bounded by `build.batch_size`.

### 6.2 Stage B — vocabulary statistics

One aggregate pass writing `rec_features.doc_count` and `idf`, plus `tags.series_count`. A
`GROUP BY` over `series_features` executed **entirely in the database** — no rows cross the
wire, so the stage has no client-side memory cost at any catalogue size. It must run *before* C,
and a full rebuild must recompute it: idf drift is what makes a stale model quietly worse rather
than loudly broken.

### 6.3 Stage C — the embedding

pgvector is available, so this stage is a projection and an index build, not a similarity join.
The hand-rolled streaming top-K join that occupied this section in the previous draft — the dense
ordinal space, the memory-mapped vector file, the L2-resident accumulator, the df-pruned inverted
index, MaxScore — **is all deleted.** So are `rec_feature_postings` (20M rows) and
`series_neighbours` (700 MB, and the source of every merge problem in §9).

That deletion is the single largest simplification available to this design, and it is worth more
than the performance argument that motivated the machinery.

**Reduce, then index.**

1. **Truncated SVD** over the TF-IDF feature matrix from stage A, rank `build.embedding_dims`
   (default 128), computed with a randomized algorithm (two or three passes over the non-zeros).

   **Authors are excluded from the SVD input.** With ~200k distinct authors at df ≈ 2–5, they
   would blow the input dimension out by two orders of magnitude and then be annihilated anyway —
   a rank-128 approximation cannot represent a feature that appears three times. They are
   retrieved exactly instead, by R3 (§7.1). Excluding them leaves an input of ~700 dimensions:
   the ~600-term tag vocabulary plus the scalar features.

   This is why SVD earns its keep over a random projection, which would be nearly free: SVD
   *generalises*. It learns that `Regression` and `Reincarnation` co-occur and places them near
   each other, so two series sharing neither exact tag still land close. Random projection
   preserves the distances that already exist and discovers nothing.

2. **Write `series_embedding`** by streaming `COPY` in batches. The projection is a per-series
   matrix–vector product once the 700×128 basis exists, so this stage never holds more than one
   batch plus a 700 × 128 basis matrix (~360 KB).

3. **Build the HNSW index.** `m = 16`, `ef_construction = 64`.

**Approximation is a real cost, stated plainly.** Exact sparse cosine over the full feature vector
is more faithful than an ANN search over a rank-128 compression of a subset of it. Two things
recover most of the gap: R3 retrieves the rare high-precision features exactly, and the sparse
vectors are still what generate the explanation (§7.5), so what the reader is *told* stays exact
even where retrieval is approximate.

**If measurement disagrees, the fallback is known.** Should §11.2 show dense retrieval losing to
sparse-exact on `recall@12`, reinstate precomputed neighbours — but as a **normalised
`series_neighbours(series_id, neighbour_id, rank, score)` table with real foreign keys**, never
the `uuid[]` arrays of the previous draft. The array layout was chosen for storage and page-fetch
count, and it is what created the dangling-id problem, the repair queue and the read-path alias
dance in §9. Normalised rows cascade on merge for free. That trade was misjudged the first time;
do not repeat it.

**Cheap quality win, unchanged:** blocking by `content_type` as a score multiplier
(`score.cross_type_multiplier`, default 0.7) applied after retrieval, not as a partition.

### 6.4 What it actually costs

| Stage | Cost | Bound by |
|---|---|---|
| A — extraction | streaming, one batch resident | `build.batch_size` |
| B — idf | pure SQL `GROUP BY` | nothing crosses the wire |
| C — SVD | 2–3 passes over ~14M non-zeros (authors excluded); ~360 KB basis resident | streamable in batches |
| C — embedding write | ~256 MB via `COPY` | one batch |
| C — HNSW build | **the dominant cost**: tens of minutes at 1M rows, and it wants `maintenance_work_mem` | see below |
| D — co-occurrence | aggregated in Postgres, one user's list resident | `cooccurrence.max_list_entries` |
| E — priors | pure SQL | — |

**The HNSW build is now the bottleneck, and it moved into the database.** That is mostly good —
Postgres parallelises it, it is restartable, and it is not our code — but it has two properties
worth planning for:

- It wants `maintenance_work_mem` large enough to hold the graph, or it spills and slows sharply.
  With `halfvec(128)` the table is ~256 MB and the graph roughly the same again, so ~1 GB is a
  reasonable setting for a full 1M build. Document it in `deploy/README.md`; do not set it
  globally.
- `CREATE INDEX CONCURRENTLY` cannot run inside the migrator's implicit transaction — migration
  0020 already documents this trap at length for the plain indexes, and the same constraint
  applies here. The index is created by the **builder**, not by a migration, which sidesteps it
  entirely and is also where a regeneration belongs.

**Steady-state builder heap is now single-digit megabytes.** Nothing in the pipeline is resident
in proportion to the catalogue: A and C stream in batches, B, D and E execute in the database.
This is a better answer to "reduce memory and CPU as much as possible" than the streaming join
was, because the work is not merely streamed — most of it no longer exists.

**Revised target: < 45 min for a full 1M rebuild**, dominated by the HNSW build, with the
embedding write second. Incremental builds re-embed only changed digests and insert into the
existing index, which is cheap and needs no rebuild.

Still estimates. The measurement deliverable in phase 1 is unchanged in spirit but different in
content: wall clock and peak RSS per stage, the HNSW build time and index size at 100k / 500k /
1M, and **measured ANN recall@k against exact cosine on a sample** — that last one is what says
whether `ef_search` is set correctly, and there is no way to guess it.

**Levers, in order:** `retrieval.ef_search` (recall vs latency, online, no rebuild),
`build.embedding_dims` (quality vs index size, needs a full rebuild), HNSW `m` /
`ef_construction` (build time vs recall), and `retrieval.seeds` (tail latency, §7.1).


### 6.5 Stage D — co-occurrence, aggregated in the database

Item-item over `user_series_affinity`, restricted to positive affinity:

```
score(i, j) = Σ_u  a(u,i)·a(u,j) / (log(1 + |list(u)|) · sqrt(pop(i)·pop(j)))
```

The `log(1 + |list|)` denominator stops one user with a 3 000-entry imported AniList list from
dictating the matrix. The `sqrt(pop·pop)` denominator is the standard popularity normalisation —
without it everything co-occurs with *One Piece* and the model learns a popularity ranking with
extra steps.

**Stream it; do not materialise the pair set in the builder.** The naive version holds
`Σ_u |list(u)|²/2` pairs in a client-side map — 20M at 1 000 users × 200 entries, and one 3 000-entry
imported list alone contributes 4.5M. Instead:

1. Walk users in batches, one user's list resident at a time.
2. Emit `(i, j, user_id, contribution)` quadruples straight into an **unlogged temp table** via
   `COPY`.
3. Aggregate in SQL: `GROUP BY i, j HAVING count(DISTINCT user_id) >= cooccurrence.min_support`.

Postgres spills that sort to disk if it must; the builder's residency is one user's list. The
aggregation is also where the privacy threshold is enforced, which is the right place for it —
one `HAVING` clause, not a filter someone can forget downstream.

**Cap each user's contribution at `cooccurrence.max_list_entries`** (default 300, taken by
descending affinity). Quadratic growth from a single imported list is both a memory problem and a
quality problem, and one cap fixes both.

**Minimum support of 5 distinct users** before a pair may influence anything. That is a privacy
control, not a quality one — see §12.2 and §8.3 — and it is why co-occurrence degrades to nothing
on a small deployment rather than being load-bearing.

### 6.6 Stage E — priors

```
prior = w_watchers·c·norm(watchers)
      + (w_score·norm(external_score) + w_sources·norm(source_count) + w_velocity·norm(velocity))
        · (1 + w_watchers·(1 − c))
```

where `c = total_users / (total_users + 50)`.

The confidence factor is not decoration. `watchers` is pure noise below ~50 users, and it feeds
R4 — the retrieval path a brand-new deployment leans on hardest. Without `c`, the first weeks of
every deployment rank the catalogue by a handful of arbitrary early watchlists. The weight
redistributes to the three catalogue-side signals, which need no users at all, and slides back as
the population grows. Same shrinkage principle as `confidence(support)` in §7.2, same reason.

All four weights are tunables (§8), so the shape is an operator's decision, not a constant.

`recommendable = false` for: no active `series_sources`, zero chapters, fewer than
`build.min_features` features, `is_adult` (gated separately at read time, see §7.4). A series
nothing links to and nothing describes cannot be recommended usefully and should not consume a
neighbour slot.

### 6.7 Scheduling

| Build | Trigger | Frequency |
|---|---|---|
| Incremental (A/C for changed digests + the re-embed queue, B, E) | control-plane scheduler, leader-elected | every 15 min, configurable |
| Full (all stages, all series, new generation, index rebuild) | control-plane scheduler | weekly, configurable, 0 disables |
| Co-occurrence (D) | control-plane scheduler | daily |

The re-embed queue (§9.3) no longer needs its own schedule — it is drained at the head of the
incremental pass, and the incremental pass is frequent because re-embedding a handful of changed
series is an insert into a live HNSW index, not a rebuild.

The generation counter exists so a full rebuild is atomic-ish without a table swap: rows are
upserted under the new generation while readers keep querying the old ones, and a final sweep
deletes `generation < current`. A `DROP`/`RENAME` swap would be faster but takes `ACCESS
EXCLUSIVE` on a table the API reads on every request.

**One caveat the generation trick does not cover:** changing `embedding_dims` changes the column
type, so that rebuild genuinely needs a second table and a swap. It is the one `NextFullBuild`
tunable that costs more than a rebuild, and the console should say so.

---

## 7. The request path

Every number in this section — seed counts, posting caps, weights, `λ`, the TTL, the decay — is a
tunable (§8.6), not a constant. They are written inline as their shipped defaults, because a
design document that says "configurable" everywhere says nothing about what the system actually
does.

### 7.1 Retrieval — five bounded paths, unioned

| Path | Source | Yield | Purpose |
|---|---|---|---|
| **R1** seed ANN | HNSW over `series_embedding`, one search per seed, top ~8 seeds | ≤ 400 | Precision. "More like the thing you loved," and the source of the explanation. |
| **R2** profile ANN | **One** HNSW search from the reader's profile vector | ≤ 200 | Recall. Catches items no single seed is near — the reader's centre of gravity, not any one book. |
| **R3** exact author/rare-tag | `series_authors` / `series_tags` index lookups on the seeds' rarest features | ≤ 200 | Precision on exactly what the embedding destroys (§6.3). Same author is a near-certain recommendation and SVD cannot see it. |
| **R4** collaborative | `series_cooccurrence` for the top ~15 seeds | ≤ 300 | The signal content features cannot see (tone, quality, "readers of X also read Y"). |
| **R5** prior backfill | `series_prior` ordered by `prior DESC` | ≤ 100 | Cold start, and shelf-filling when the others come up short. |

R1 and R2 are HNSW searches — sublinear, not scans. R3–R5 are index range scans with a `LIMIT`.
Nothing here touches a row count that grows linearly with the catalogue.

**The latency budget, spelled out**, because R1/R2 are the one place this design does real work
per request rather than reading precomputed rows:

```
1 profile search + 8 seed searches ≈ 9 × ~1.5 ms  ≈  13 ms
+ candidate fetch (one = ANY) + scoring + MMR      ≈   5 ms
```

That fits p99 < 50 ms with room, but it is genuinely tighter than a precomputed-neighbour table
would be, and it puts the cost on the database rather than in a nightly job. `retrieval.seeds` is
capped at 8 by default for exactly this reason — it is the knob that trades tail latency for
recall, and it is the first one to look at if the p99 target is missed.

Deduplicate to a candidate set of at most ~1 000 ids, then fetch `series_features` +
`series_prior` + `series` for exactly those ids in one `= ANY($1)` query. `series_features` is
still what produces the explanation (§7.5): the embedding says *that* two series are close, and
only the sparse vectors can say *why*.

### 7.2 Scoring

Each path's contribution is **rank-normalised within its own path before blending** — the four
scales are not comparable (a cosine, a co-occurrence score, a prior), and blending raw values
means whichever has the largest natural range silently wins.

```
score(c) =  w_knn  · Σ_seeds a(u,s) · sim(s,c) · decay(s)
          + w_prof · cos(p(u), f(c))
          + w_cf   · cf(u,c) · confidence(support)
          + w_prior· prior(c)
          − w_neg  · cos(n(u), f(c))
```

`confidence(support) = support / (support + 10)` — shrinkage toward zero when the co-occurrence
evidence is thin, which on a small deployment is always. This is what makes the same code correct
for a 3-user instance and a 30 000-user one without a config switch.

All five weights live in `RecommendationsConfig` and are hot-reloadable via the existing reload
supervisor, so tuning does not need a redeploy.

### 7.3 Diversity

Greedy MMR over the candidate set:

```
pick = argmax [ λ·score(c) − (1−λ)·max_{s ∈ picked} cos(f(c), f(s)) ]     λ = 0.7
```

Plus hard caps: at most 2 per author, at most 3 sharing the dominant tag. Twelve near-identical
series is the failure mode a pure score ranking produces and it reads as broken even when every
individual pick is defensible.

### 7.4 Filters

Applied before scoring where they are indexed, after where they are not:

- already in `watchlist_entries` for this user;
- in `recommendation_feedback` (`not_interested` decays after 90 days; `hide_forever` does not);
- `series_prior.recommendable = false`;
- `series.is_adult` unless the user has opted in — a per-user preference, defaulting off, not a
  deployment-wide flag;
- no active `series_sources` (recommending something unreadable is worse than recommending
  nothing);
- direct sequels/prequels of tracked series → **excluded from the discovery shelf and routed to a
  separate "next in the series" rail**. A shelf that tells a reader of *Vinland Saga* about
  *Vinland Saga* is not a recommendation. Needs AniList `relations` (§2.2).

### 7.5 Explanation

Every item returns why:

```json
{ "series_id": "…", "score": 0.83,
  "because": { "kind": "similar_to", "series_id": "…", "title": "Solo Leveling",
               "shared": ["Regression", "Dungeon", "Weak to Strong"] } }
```

`shared` is the top-3 features by contribution to the pair's cosine, spoiler tags excluded. This
is nearly free — the extractor already has both vectors — and it is the difference between a shelf
users trust and a shelf they ignore.

### 7.6 Caching

`user_recommendations` holds the last computed shelf. Serve it when
`profile_at == user_taste_profile.built_at` and it is younger than the TTL (default 6 h);
otherwise recompute inline and write it back.

**Compute inline, cache opportunistically — do not build a per-user precompute sweep.** The inline
path is ~25 primary-key fetches, one `= ANY` fetch of ~2 000 rows, and a few million float
operations. That is comfortably inside the p99 target, and a precompute sweep does work for users
who never sign in. If measurement contradicts this, the escape hatch is to precompute for users
active in the last 7 days — but do not build it before the numbers demand it.

### 7.7 Endpoints

| Route | Feature flag | Notes |
|---|---|---|
| `GET /v1/me/recommendations` | `catalogue.recommendations` (exists) | Replaces the stub. Gains `limit`, `exclude_tracked`, cursor. |
| `GET /v1/series/{id}/similar` | `catalogue.recommendations` | One HNSW search, no user context, publicly cacheable. Cheap and high value — it also gives the system a surface that works for signed-out visitors, and it is the cheapest way to sanity-check the embedding in production. |
| `POST /v1/me/recommendations/{series_id}/feedback` | `catalogue.recommendations` | `{"verdict":"not_interested"}`. Writes `recommendation_feedback`; doubles as the offline evaluation signal. |
| `GET /v1/me/taste` | `catalogue.recommendations` | The reader's own profile, rendered — top tags, authors, length and type preference. Users like seeing it, and it is the fastest way to debug a bad shelf in production without reading anyone's watchlist. |

---

## 8. Tuning: every value is an operator decision

Not one weight, base or threshold in this document is a constant in the source. They are a
registry, stored as overrides, edited in the console, audited, and refreshed into every replica.

**The design is a deliberate copy of `Feature`** (`crates/domain/src/features.rs` +
`crates/service/src/flags.rs` + `crates/db/src/repo/flags.rs` + `services/api/src/admin/flags.rs`
+ `web/frontend/src/views/console/flags.rs`). That vertical already solves compiled-default
resolution, override storage, timed refresh, permission gating, audit and the console panel. A
second mechanism for the same problem would be a second thing to get wrong.

### 8.1 What is a tunable and what is configuration

> **If changing it changes what a reader sees, it is a tunable. If it changes what the server
> consumes, it is configuration.**

| | Tunables | `RecommendationsConfig` (env) |
|---|---|---|
| Examples | weights, affinity bases, K, candidate caps, half-lives, support thresholds | work directory, batch and block sizes, build intervals, resident-memory cap, whether the builder runs at all |
| Stored in | `tunable_overrides` (database) | environment, `docs/CONFIGURATION.md` |
| Changed by | an operator in the console, at runtime | a redeploy |
| Layers | compiled default → database override | compiled default → env |

Two layers, not three. Features do exactly this and it is enough. Adding an env layer under the
database one would mean every question about a live value has two places to look and a precedence
rule to remember.

`build.block_size` is the instructive borderline: it changes peak memory and nothing about the
output, so it is configuration. `build.max_candidates` changes recall, so it is a tunable even
though it also moves cost.

### 8.2 The registry

```rust
pub struct TunableSpec {
    pub key:         &'static str,   // "recsys.score.weight.knn"
    pub group:       TunableGroup,
    pub title:       &'static str,
    pub description: &'static str,   // read immediately before someone changes production
    pub default:     f64,
    pub range:       RangeInclusive<f64>,
    pub kind:        TunableKind,    // Ratio | Weight | Count | Days | Seconds
    pub applies:     Applies,
}
```

Values are stored and transported as `f64` regardless of kind, so there is one table and one API
shape; the registry supplies the typing and the accessors clamp on the way out
(`tunables.neighbour_k() -> usize`). The alternative — a typed column per kind, or a JSON blob —
buys nothing and costs a schema change per new knob.

### 8.3 Ranges are enforced, and one of them is a privacy floor

Every write is validated against `range` **in the API**, not only in the UI. The console cannot be
the only thing standing between an operator and a bad value; a `curl` is not an attack.

`recsys.cooccurrence.min_support` has a range floor of **5 and no way to go below it**. It is the
k-anonymity threshold from §12.2, not a tuning knob, and an admin panel that accepts `1` in that
field is a privacy bug with a user interface. This mirrors `Feature::is_locked` exactly, including
its reasoning: refused at the door *and* clamped by the reader, because the stored row that should
not exist is a different failure from the request that should not succeed.

One cross-field rule: a write that would set every `score.weight.*` to zero is refused. The five
weights need not sum to anything — sub-scores are rank-normalised per retrieval path before
blending (§7.2), so their scale is free — but all-zero produces an arbitrary shelf with no error
anywhere.

### 8.4 When a change takes effect

Each spec carries `Applies`, and **the console shows it on every row**:

| `Applies` | Meaning | Examples |
|---|---|---|
| `Immediately` | next request, once the snapshot refreshes | every score weight, diversity, affinity, retrieval caps, serving |
| `NextBuild` | next incremental build | `build.max_candidates`, `build.min_features`, co-occurrence knobs |
| `NextFullBuild` | next full rebuild — the stored model was computed under the old value | `build.neighbour_k`, `build.df_prune_fraction` |

This is the most likely way the panel fails a user. Someone raises `neighbour_k`, sees no change,
raises it again, and concludes the page is broken — when the truth is that 32 neighbours per
series are what is physically stored. The badge, and a "rebuild to apply" affordance next to it,
are not polish.

### 8.5 Storage and refresh

```sql
CREATE TABLE tunable_overrides (
  key        text PRIMARY KEY,
  value      double precision NOT NULL,
  note       text,
  updated_by uuid REFERENCES users(id) ON DELETE SET NULL,
  updated_at timestamptz NOT NULL DEFAULT now()
);
```

Same shape and same properties as `feature_flag_overrides`: only deviations are stored, so an
empty table is a fully working deployment, and a key from a retired build stays visible on the
one page that can delete it rather than vanishing.

`TunableSet` lives in `crates/service/src/tunables.rs` beside `FeatureGate`, refreshes on the same
timer, and **keeps its previous snapshot on a failed refresh**. Reverting to compiled defaults
because one query failed would silently discard an operator's tuning — the identical hazard
`flags.rs` already documents for overrides, and the reason its comment says callers must not
collapse `Err` into an empty list.

### 8.6 The surface

Thirty-five values, all editable. Ranges are enforced per §8.3.

**Affinity** (§4.1) — all `Immediately`

| Key | Default | Range |
|---|---|---|
| `recsys.affinity.base.completed` | 1.00 | 0 … 1 |
| `recsys.affinity.base.reading` | 0.80 | 0 … 1 |
| `recsys.affinity.base.paused` | 0.35 | 0 … 1 |
| `recsys.affinity.base.planned` | 0.25 | 0 … 1 |
| `recsys.affinity.dropped.floor` | −0.60 | −1 … 0 |
| `recsys.affinity.dropped.span` | 0.50 | 0 … 1 |
| `recsys.affinity.engagement_knee` | 60 | 5 … 1000 chapters |
| `recsys.affinity.recency_half_life_days` | 180 | 7 … 3650 |
| `recsys.affinity.recency_floor` | 0.30 | 0 … 1 |
| `recsys.affinity.external_score_weight` | 0.25 | 0 … 1 |

**Retrieval** (§7.1) — all `Immediately`

| Key | Default | Range |
|---|---|---|
| `recsys.retrieval.seeds` | 8 | 1 … 64 |
| `recsys.retrieval.ef_search` | 60 | 10 … 1000 |
| `recsys.retrieval.ann_limit_per_seed` | 50 | 5 … 500 |
| `recsys.retrieval.ann_limit_profile` | 200 | 10 … 2000 |
| `recsys.retrieval.exact_feature_limit` | 200 | 0 … 2000 |
| `recsys.retrieval.cooccurrence_seeds` | 15 | 0 … 100 |
| `recsys.retrieval.candidate_cap` | 1000 | 100 … 20000 |

`seeds` and `ef_search` are the two that move p99 (§7.1). `ef_search` is the one knob that trades
ANN recall against latency **without a rebuild** — it is the first thing to reach for in either
direction, and the only way to set it honestly is against the measured recall in §6.4.

**Scoring** (§7.2) — all `Immediately`

| Key | Default | Range |
|---|---|---|
| `recsys.score.weight.knn` | 1.00 | 0 … 10 |
| `recsys.score.weight.profile` | 0.70 | 0 … 10 |
| `recsys.score.weight.collaborative` | 0.60 | 0 … 10 |
| `recsys.score.weight.prior` | 0.25 | 0 … 10 |
| `recsys.score.weight.negative` | 0.50 | 0 … 10 |
| `recsys.score.cross_type_multiplier` | 0.70 | 0 … 1 |
| `recsys.score.cf_shrinkage_k` | 10 | 1 … 1000 |

**Diversity** (§7.3) — all `Immediately`

| Key | Default | Range |
|---|---|---|
| `recsys.diversity.lambda` | 0.70 | 0 … 1 |
| `recsys.diversity.max_per_author` | 2 | 1 … 12 |
| `recsys.diversity.max_per_tag` | 3 | 1 … 12 |

**Prior** (§6.6) — all `NextBuild`

| Key | Default | Range |
|---|---|---|
| `recsys.prior.weight.watchers` | 0.40 | 0 … 1 |
| `recsys.prior.weight.external_score` | 0.25 | 0 … 1 |
| `recsys.prior.weight.source_count` | 0.20 | 0 … 1 |
| `recsys.prior.weight.velocity` | 0.15 | 0 … 1 |
| `recsys.prior.watcher_confidence_k` | 50 | 1 … 100000 |

**Build** (§6.4)

| Key | Default | Range | Applies |
|---|---|---|---|
| `recsys.build.embedding_dims` | 128 | 32 … 512 | `NextFullBuild` |
| `recsys.build.hnsw_m` | 16 | 4 … 64 | `NextFullBuild` |
| `recsys.build.hnsw_ef_construction` | 64 | 16 … 512 | `NextFullBuild` |
| `recsys.build.min_features` | 3 | 1 … 20 | `NextBuild` |

All three `NextFullBuild` values are baked into stored data — the embedding and the graph — so
changing one and expecting the next request to differ is the §8.4 failure mode in its purest
form. `embedding_dims` in particular multiplies both table size and index memory: at 512 the
table alone is ~1 GB. Put that number in the spec's `description`, where the operator reads it
before typing.

**Co-occurrence** (§6.5) — all `NextBuild`

| Key | Default | Range |
|---|---|---|
| `recsys.cooccurrence.min_support` | 5 | **5** … 1000 (floor is a privacy clamp, §8.3) |
| `recsys.cooccurrence.max_list_entries` | 300 | 10 … 5000 |

**Serving** (§7.6) — all `Immediately`

| Key | Default | Range |
|---|---|---|
| `recsys.serve.shelf_size` | 12 | 1 … 60 |
| `recsys.serve.shelf_ttl_seconds` | 21600 | 0 … 604800 |
| `recsys.serve.feedback_decay_days` | 90 | 1 … 3650 |

---

## 9. Merges

Merges are frequent here — `merge_candidates.outcome` admits `auto_merged`, and the standing
duplicate sweep performs destructive merges without an operator. A recommender that assumes
series ids are stable is a recommender that is wrong most of the time.

### 9.1 What a merge actually breaks

Reading `merge_series` in `crates/db/src/repo/matching.rs`: it is one long transaction that
**hand-folds every table holding a `series_id`**, each with its own conflict rule, and then
`DELETE`s the absorbed row. Three consequences.

**One is a correctness bug, not staleness.** `recommendation_feedback` is keyed
`(user_id, series_id)` with a cascading foreign key. Left alone, a merge silently deletes the
loser's row — so a user who said "never show me this" has that decision quietly undone by a
catalogue merge they never saw. This is precisely the hazard the existing code already calls out
for `series_sync_overrides`: *a user's decision is theirs, not the catalogue's, and must survive
the catalogue deciding two rows were one.* Same rule, same conservative reading, and it must be
folded **before** the delete.

**One was structural, and pgvector deleted it.** The earlier `uuid[]` neighbour arrays could not
carry a foreign key, so a merged id survived in the arrays of thousands of other series with no
reverse index to find them — which is what the repair queue and the read-path alias dance existed
to work around. Retrieval is now a live HNSW search over a table with a cascading foreign key: the
absorbed row's embedding disappears in the same transaction that deletes the series, and no query
can return it afterwards. **There is no window in which a merged series can be recommended.**

**One is quality, and it is the smallest.** Two merged series were duplicates, so they sat almost
on top of each other in the embedding space. A merge barely perturbs ranking. The damage is lost
user decisions, not worse recommendations.

### 9.2 `series_merges` — the alias table the product already needed

Today the absorbed row is deleted with no forwarding record. That is a gap wider than this
feature: with automatic merges running continuously, every merged id is a hard 404 for bookmarks,
shared links, external references and any client holding a stale id.

```sql
-- Deliberately no FK on `merged_id`: the row it names is gone. That is the point.
CREATE TABLE series_merges (
  merged_id   uuid PRIMARY KEY,
  survivor_id uuid NOT NULL REFERENCES series(id) ON DELETE CASCADE,
  merged_at   timestamptz NOT NULL DEFAULT now(),
  merged_by   uuid REFERENCES users(id) ON DELETE SET NULL
);
CREATE INDEX series_merges_survivor_idx ON series_merges (survivor_id);
```

**Path-compress on write, never resolve transitively on read.** When B is merged into C, also:

```sql
UPDATE series_merges SET survivor_id = $C WHERE survivor_id = $B;
```

That is what `series_merges_survivor_idx` is for. The map stays one hop deep forever, so
resolution is a single lookup instead of a recursive CTE that is both slower and able to spin on a
cycle. Cycles cannot form: the survivor always exists and the merged id is always deleted.

The table is tiny — one row per merge in the deployment's history — and read-mostly, so the API
holds it as an **in-memory map refreshed on the same timer as the feature snapshot**. Resolution
then costs the request nothing.

Scope note: this is slightly wider than the recommender. It is proposed here because this is what
surfaced it, and because `GET /v1/series/{merged_id}` returning `301` to the survivor is worth
more than the recommender is.

### 9.3 The re-embed queue

```sql
CREATE TABLE rec_repair_queue (
  series_id   uuid PRIMARY KEY REFERENCES series(id) ON DELETE CASCADE,
  reason      text NOT NULL,   -- 'merged' | 'features_changed'
  enqueued_at timestamptz NOT NULL DEFAULT now()
);
```

Much smaller than it was. With neighbour arrays gone there are no inbound references to repair,
so the queue holds **only the survivor** — whose tags and authors changed when it absorbed the
loser, so its digest changed and its embedding is stale. One row per merge, not ~2 000.

The primary key on `series_id` is free deduplication, which is what makes it survive frequent
merges: a popular series enqueued forty times in an hour is one row. The incremental build drains
the queue before it looks at digests.

A stale embedding is a *quality* issue with a bounded window (minutes), not a correctness one —
the survivor is still retrievable and still roughly in the right place, because it absorbed a
near-duplicate. That is a materially weaker requirement than the previous draft's, and it is why
this queue no longer needs its own drain schedule separate from the incremental build.

### 9.4 Fold rules, table by table

Every one of these is a line to add to `merge_series`, or a deliberate decision that the cascade
is correct. Both need to be written down, because "nothing to do here" and "nobody thought about
it" look identical in a diff.

| Table | Cascades? | Action |
|---|---|---|
| `series_features` | yes | Nothing. The survivor absorbed the loser's tags and authors, so its digest changes and stage A re-extracts it. Enqueue it for re-embedding (§9.3). |
| `series_embedding` | yes | Nothing. The cascade removes it in the same transaction, so no HNSW search can return the absorbed series afterwards. This is the row that used to be a `uuid[]` problem. |
| `series_prior` | yes | Nothing; stage E recomputes the survivor's `watchers`. |
| `series_cooccurrence` | yes, both columns | Let both cascade; stage D recomputes. **Do not re-point.** `UPDATE … SET series_id = keep` violates the `series_id <> other_id` CHECK for a `(loser, survivor)` pair, and double-counts support for every pair both series shared. Someone will try it; say why not, in the code. |
| `user_series_affinity` | yes | Nothing directly — it is derived from watchlist and progress, which `merge_series` already folds correctly. Mark the affected profiles stale and let it be recomputed from the folded truth. Merging derived rows by hand is how the two diverge. |
| `user_taste_profile` | no (user-keyed) | `UPDATE … SET stale = true` for users who tracked either series. Bounded by the watcher count. |
| `recommendation_feedback` | yes — **and that is the bug** | Fold **before** the delete: `INSERT … SELECT … ON CONFLICT (user_id, series_id) DO UPDATE` keeping the stronger verdict (`hide_forever` > `not_interested`). §9.1. |
| `user_recommendations` | no (user-keyed) | Nothing. The cached shelf holds the dead id, and the `profile_at` staleness check regenerates it as soon as the profile above is marked stale. |

### 9.5 Read-path resolution

Retrieval no longer produces dangling ids, so the alias map is not load-bearing for correctness
here. It still has two jobs on the request path:

1. **Seeds.** A reader's watchlist, taste-profile `seeds[]` and cached `user_recommendations`
   JSON can all hold ids merged since they were written. Resolve them through `series_merges`
   before use — **resolve, do not drop**: the survivor is the thing the reader wanted.
2. **Deduplicate, then drop self-references.** After resolution two seeds can be the same series,
   and a seed can resolve onto a candidate — a series recommending itself is the most visible
   possible bug, and it becomes reachable precisely when a merge happens.

The join back to `series` for the `recommendable` and moderation filters stays as the backstop.

### 9.6 The test that stops the next table from breaking merges

`merge_series` is a hand-maintained list of every table with a `series_id`. It grows by ten tables
in this design, and the failure mode when someone adds an eleventh and forgets is silent.

Add a differential test — the same instinct as the unread-predicate test in `repo_tracking` and
the adapter-picker test in the console: **enumerate every table with a `series_id` column from
`information_schema`, and assert each is either folded by `merge_series` or named in an explicit
`cascade_is_correct()` list carrying its reason.** A new table fails the test until someone decides
which it is.

Given how frequent merges are, this is the highest-value test in the whole design.

---

## 10. Operator console

The recommender gets its own console entity. Without one it is a black box with thirty-five
invisible knobs and a build that either ran or did not.

### 10.1 Where it lives

A new `Entity::Recommendations` in `web/frontend/src/views/console/mod.rs`, in the
`console.group.catalogue` group beside `Merge`:

| Wiring | Value |
|---|---|
| `label_key` | `console.tab.recommendations` |
| `icon` | `Icon::Sparkle` (add to `icons.rs`) |
| `slug` | `recommendations` |
| `requires()` | `(Permission::RecsysRead, Feature::AdminRecommendations)` |
| `is_master_detail()` | `false` — one wide pane |
| `auto_refreshes()` | **`false`** |

`auto_refreshes()` is `false` for the same reason `Providers`, `Users` and `Flags` are: this is a
mid-edit work surface, and a background refetch landing on a half-filled tuning form discards it.
The module doc already states that rule. The consequence is that the model-health card needs its
own local reload and a manual refresh control, since the shared tick will not drive it.

`RailCount` shows the repair-queue depth with `CountTone::Attention` when non-zero — the same
treatment `pending_merges` gets, and for the same reason. Needs a field on `SystemStats`.

New permissions `recsys.read` / `recsys.write` in `crates/domain/src/permissions.rs`
(`PermissionGroup::Catalogue`), plus a migration seeding them into both lists the 0018 pattern
maintains. New `Feature::AdminRecommendations`, in the operations group beside `AdminSync` and
`AdminFeatureFlags`.

### 10.2 Panel — model health

What an operator needs before touching anything:

- current `generation`, stage, and whether a build is running now;
- last full and last incremental build: when, how long, how many series, and **peak RSS** (the
  number §6.4 promises to measure is worth showing, not just benchmarking);
- coverage: series with features, series with neighbours, series marked `recommendable`, and the
  gap between them — a large gap means `build.min_features` is doing more than intended;
- co-occurrence: total pairs and **how many clear `min_support`**, which is the honest answer to
  "is collaborative filtering doing anything on this deployment?";
- repair-queue depth;
- a staleness banner when `model_age > 2 × full_interval`, matching the metric alert in §12.5.

Actions, both `Permission::RecsysWrite`, both audited, both routed through the existing
`.expensive(…)` rate-limit class — there is a precedent at
`.expensive("/v1/admin/matching/rebuild-keys")`:

- `POST /v1/admin/recommendations/rebuild` `{"mode":"incremental"|"full"}`
- `POST /v1/admin/recommendations/repair` — drain the queue now.

### 10.3 Panel — tuning

Modelled directly on `FeatureFlagsPanel`, which already solves this layout. Grouped by
`TunableGroup`; each row carries title, description, an input bounded by the registry's
`min`/`max`, the default, a "changed from default" badge, a reset control, an optional note, and
the `Applies` badge from §8.4.

Read-only for a reader holding `recsys.read` without `recsys.write`, exactly as the flags panel
degrades.

`GET /v1/admin/recommendations/tunables` returns the compiled registry joined to stored overrides
— the same construction as `FlagView`, and for the same reason: the page can then never show a
value the server does not honour, or omit one it does.

**A "what would change?" affordance is worth building here and nowhere else.** Editing a weight is
a blind action otherwise. Pair the tuning panel with §10.4 so an operator can change a weight and
immediately re-run the explain view against their own account.

### 10.4 Panel — explain

The panel that makes this debuggable in production. For a chosen series, or for the operator's own
account, show: the seeds and their affinities, what each retrieval path contributed, the candidate
count at each stage, the final ranked list, and a per-item score breakdown across the five terms.

**It operates on the operator's own account and on arbitrary series — never on another user's
account.** A cross-user version would be more useful and would also be an operator-facing window
onto individual reading histories, which is exactly the profiling exposure §12.1 requires
disclosing. Restricting it to self plus catalogue keeps essentially all the debugging value and
removes the problem rather than auditing around it. If cross-user inspection is ever genuinely
needed, it needs a distinct permission, its own audit action, and a line in the privacy policy.

### 10.5 Frontend mechanics and gates

`web/frontend` is a separate workspace and inherits nothing; `openapi.json` is the only connector.
The order is fixed:

1. API handlers land;
2. `cargo run -p xtask -- openapi`;
3. `crates/api-client` regenerates (never hand-edited — rule 6);
4. the frontend compiles against the new `crate::wire::types`.

Also required:

- **Every new key in both `en.json` and `de.json`.** A missing key renders as the key itself, so
  the failure ships silently.
- **A membership test for the tuning panel**, mirroring `the_picker_offers_every_adapter_kind`:
  read the published `Tunable` vocabulary out of `openapi.json` and assert the panel renders every
  one. This is the identical bug class — a hand-maintained frontend vocabulary against a generated
  backend one — and this repo already learned it once, when a wrong token registered every new
  provider as `Custom`.
- **A wording test**, mirroring `every_offered_adapter_kind_is_worded`: every tunable group and
  every `Applies` variant has an `i18n::has_key` label.
- No `document::eval` (banned in `web/frontend/clippy.toml`); typed wrappers go in `browser.rs`.
- The new permission and feature must flow through `/v1/me/capabilities`, or the rail entity is
  invisible to the operator who holds it.

---

## 11. Evaluation

A recommender with no measurement is a random number generator with a good story.

### 11.1 Offline harness

`crates/recsys/tests/eval.rs`, run against the workload fixture:

- **Temporal leave-one-out**: hide each user's most recently added positive watchlist entry,
  rebuild the profile from the rest, ask for 50, measure whether the held-out item is in there.
- Metrics: `recall@12`, `recall@50`, `MRR`, **catalogue coverage** (fraction of the catalogue that
  appears in anyone's top-50 — the number that catches a system recommending the same 200 series
  to everyone), **novelty** (mean `−log popularity` of returned items), **intra-list diversity**
  (mean pairwise feature distance).

### 11.2 Acceptance gate

The new system must beat **both** baselines on `recall@12` on the fixture:

1. popularity-only (`ORDER BY prior DESC`), and
2. the current tag-overlap query.

Beating (1) is not automatic — popularity baselines are famously hard to beat on recall, and a
content model that loses to it is a content model that is not working. Beating (2) is the point of
the exercise.

Record the numbers in the PR. A ranking change that moves them is a ranking change that has to
say so.

### 11.3 Production signals

`recommendation_feedback` acceptance rate (added to watchlist within 7 days of appearing) is the
only honest online metric available without a click log. Track it per generation so a bad model
rollout is visible.

### 11.4 What this design does *not* claim

- No learned ranker. There is no click log and no impression log, so there is no training data for
  one. Adding impression logging to enable a learn-to-rank model is a legitimate phase-4 project;
  it is also a per-request write on the hottest path and a privacy surface, and it should not be
  built on speculation.
- No dense embeddings in phase 1 (§13).
- No cross-user personalisation on a deployment below the §12.2 support threshold. It degrades to
  content-based, correctly and silently.

---

## 12. Privacy, safety, and the gates

### 12.1 GDPR

Three new tables hold personal data and **must** be wired into the existing paths, or the export
is incomplete and the erasure is a data-retention bug:

- `crates/db/src/repo/privacy.rs` — the export JSON builder gains `user_series_affinity`,
  `user_taste_profile`, `recommendation_feedback`, `user_recommendations`.
- Erasure is covered by `ON DELETE CASCADE` on `users(id)` — verify, do not assume.

**Automated profiling must be disclosed.** `deploy/legal/privacy.en.md` is operator-published and
currently describes no profiling. Building a taste profile from reading behaviour is exactly what
GDPR Art. 13(2)(f) requires disclosing. This is an operator action with a template, not something
code can close — flag it in the PR and add the template text.

### 12.2 Co-occurrence leaks membership

On a small deployment, "readers of X also read Y" is a statement about *identifiable people*. With
three users, a co-occurrence edge is a disclosure of one person's watchlist to another.

Mitigation: a pair needs **≥ 5 distinct contributing users** before it is written to
`series_cooccurrence` at all (not merely down-weighted — not written). Below that, the pair does
not exist and the system falls back to content similarity. The threshold is configurable upward,
never downward; put the reason in the config doc comment, because the next person to see it will
read it as a tuning knob.

### 12.3 Adult content

`is_adult` is a hard gate defaulting to excluded, opted in per user, never inferred from tags.

### 12.4 Repository gates this touches

Not optional, and each one has bitten this repo before:

| Change | Gate |
|---|---|
| New endpoints | `cargo run -p xtask -- openapi`, **plus** a row in `me_gates()` / `public_gates()` in `services/api/tests/me_access_matrix.rs`. Regenerating `openapi.json` alone leaves the matrix red. |
| New `query!`/`query_as!` | `cargo run -p xtask -- sqlx-prepare` against a migrated database |
| New queries | `crates/db/tests/repo_query_plans.rs` — every new query must pass the 100 000 cost ceiling **with no budget entry**. If one needs a budget, the design is wrong; go back to §7.1. |
| Old query removed | **Delete the `tracking::dashboard recommendations` budget entry.** A budget that matches nothing fails the audit — deliberately. |
| New metrics | A row in `tankovault_service::metrics::CATALOGUE` and a `names::*` constant, or `repo-lint` fails. Then the reading guide in `docs/OBSERVABILITY.md`. |
| New config section | `docs/CONFIGURATION.md` (`cargo run -p xtask -- config-docs` prints the surface) |
| New feature flags | `crates/domain/src/features.rs`, its control-plane grouping, and the OpenAPI regeneration that follows |
| New DTOs | `crates/contracts` — never a repository row struct with `ToSchema` |
| `Cargo.lock` moved (new crate, the SVD dependency, criterion) | `cargo run -p xtask -- notices` |
| New permissions (`recsys.read`/`recsys.write`) | `crates/domain/src/permissions.rs` + a migration seeding **both** lists the 0018 pattern maintains, or the permission exists in code and can never be granted |
| New tables with a `series_id` | Folded in `merge_series` or named in `cascade_is_correct()`, enforced by the §9.6 differential test |
| Console entity | `Entity` wiring in `console/mod.rs`, capability flow through `/v1/me/capabilities`, keys in **both** `en.json` and `de.json`, and the two vocabulary tests in §10.5 |
| Frontend at all | `openapi.json` → regenerate `crates/api-client` → then the frontend compiles. It is a separate workspace and inherits nothing (rule 7). |

### 12.5 Metrics to add

| Metric | Type | Labels |
|---|---|---|
| `recsys_build_duration_seconds` | histogram (`WORK_BUCKETS`) | `stage` |
| `recsys_build_series_total` | counter | `stage`, `result` (`built`/`skipped`/`failed`) |
| `recsys_model_series` | gauge | `table` |
| `recsys_model_age_seconds` | gauge | — (staleness alert: model older than 2× the full interval) |
| `recsys_serve_duration_seconds` | histogram | `path` (`cached`/`computed`) |
| `recsys_shelf_size` | histogram | — (an empty-shelf rate is the earliest symptom of a broken model) |
| `recsys_candidates` | histogram | `retrieval` (`knn`/`profile`/`cf`/`prior`) |
| `recsys_build_peak_rss_bytes` | gauge | — (the §6.4 promise, kept in production and not only in a bench) |
| `recsys_ann_recall` | gauge | — (measured ANN recall against exact cosine on a sample; the only way to know `ef_search` is set right, §6.4) |
| `recsys_repair_queue_depth` | gauge | — (a queue that only grows means the drain is not keeping up with the merge rate) |
| `recsys_merge_repairs_total` | counter | `reason` (`merged`/`inbound_merge`/`features_changed`) |
| `recsys_tunable_overrides` | gauge | — (how far this deployment has drifted from the shipped defaults; the first question to ask about a bad shelf) |

---

## 13. Rejected and deferred

**~~pgvector~~ — adopted, and it is now required (§5.1).** An earlier draft deferred it because
it forces a Postgres image change on every self-hoster and costs ~1 GB of resident index. Both
costs are real and unchanged; the operator confirmed the extension is available, and taking it
turned out to buy far more than the ANN search itself.

What adopting it **deleted**: the hand-rolled streaming top-K join, the dense ordinal space, the
memory-mapped vector file, the L2-resident accumulator, the df-pruned inverted index, the
MaxScore optimisation, the `memmap2` dependency, `rec_feature_postings` (20M rows),
`series_neighbours` (700 MB), the neighbour-repair queue's inbound half, and the read-path alias
resolution that existed only to paper over `uuid[]` columns that could not carry a foreign key.

That last one is the lesson worth keeping: **the merge problems in §9 were not inherent to
recommendation — they were created by the array storage layout**, chosen for page-fetch count.
Removing the layout removed the problem class.

What it **costs**, stated so it is not forgotten: retrieval is now approximate rather than exact,
the rare high-precision features SVD annihilates need their own exact path (R3), and per-request
work moved onto the database. §11.2 is what decides whether that trade was right.

**What it newly enables**, not yet designed: "more like *this arbitrary vector*" — a free-text or
mood search, or a shelf built from three series a reader picks without any stored profile. That
is a different feature, and it is now one query away rather than one architecture away.

**Text embeddings of descriptions.** Descriptions are provider-scraped, inconsistently present,
frequently machine-translated, and often marketing copy. An embedding of that is an embedding of
noise, and it would be the most expensive part of the pipeline. If descriptions are used at all,
use them as extra sparse features (rare noun phrases) — not as a dense space.

**Matrix factorisation / implicit ALS.** Correct and standard *given interaction density*.
A self-hosted tracker with a few hundred users has a matrix that is ~99.99% empty, where MF
overfits to popularity and produces exactly the shelf a popularity baseline gives for a fraction
of the effort. The co-occurrence stage (§6.5) captures the recoverable part of that signal.
Revisit above ~10 000 active readers.

**Recomputing similarity in SQL.** Every formulation — `count(shared tags)`, a lateral over
`series_tags`, an array-overlap operator, `ts_rank` over `search_vec` — is the current stub with a
different constant factor. The catalogue is on the wrong side of the join.

**A nightly per-user precompute sweep.** Deferred, not rejected. See §7.6.

---

## 14. Phasing

Each phase is independently shippable and independently valuable.

**Phase 0 — the extension, the signal, and merges.** Migration 0026. **Move the Postgres image to
`pgvector/pgvector:pg18` and `CREATE EXTENSION vector` first** (§5.1) — it is the one breaking
change for existing operators, and everything below assumes it. Extend the AniList selection
(rich tags with rank, `averageScore`, `popularity`, `isAdult`, `source`); backfill through the
existing enrichment sweep. **Land `series_merges` (§9.2) and the §9.6 differential test here, not
later.** The alias table pays for itself immediately as a `301` for merged series, the test starts
guarding `merge_series` before ten tables are added to it rather than after, and every phase below
would otherwise have to retrofit merge handling. Nothing else user-visible ships.

**Phase 1 — the item model.** `crates/recsys` with stages A/B/C/E, the SVD projection, the
embedding write, the HNSW build, the builder in the worker, the control-plane trigger, migration
0027, and `GET /v1/series/{id}/similar`. Ships a real, signed-out-visible feature. **The wall
clock, the HNSW build time and index size, and — above all — the measured ANN recall against exact
cosine are all validated here** (§6.4). Everything later inherits whatever they turn out to be,
and the recall number is what decides whether the §6.3 fallback is needed.

**Phase 2 — the reader model.** Migration 0028, affinity, taste profile, retrieval R1/R2/R4,
scoring, MMR, explanations, and the replacement of `/v1/me/recommendations`. **Delete the stub
query and its plan budget in this phase.** Ships the actual product.

**Phase 2.5 — tuning and the console.** The `Tunable` registry (§8), `tunable_overrides`, the
snapshot in `crates/service`, the admin endpoints, and the console entity (§10). Deliberately
*after* phase 2, not before: shipping thirty-five knobs against an unvalidated ranker means tuning
noise. The compiled defaults are the ones this document argues for, and they should have to work
first.

**Phase 3 — collaboration and feedback.** Stage D with database-side aggregation, retrieval R3
with shrinkage and the support threshold, `recommendation_feedback` (and its merge fold — §9.4),
`GET /v1/me/taste`, the "next in the series" rail (needs AniList relations).

**Phase 4 — only if measured.** Impression logging and a learned ranker; free-text / mood search
over the embedding (§13); a second embedding space if the SVD proves too lossy. Each needs a
number from phase 1–3 justifying it.

---

## 15. Open decisions

Each has a default that works, so none of these blocks a start.

1. **Builder placement.** Default: in `services/worker`, triggered by control-plane. Alternative:
   a dedicated `services/recsys` — one more container for every self-hoster, in exchange for
   isolation from a memory profile the streaming design largely removed (§3.2). The code boundary
   is the same either way; only the `main.rs` moves.
2. **Embedding width.** Default `halfvec(128)` — ~256 MB of table plus a similar graph at 1M
   series. 64 dims halves both and costs generalisation; 256 doubles them. This is the value that
   sets how much RAM the database needs to keep the index hot, so decide it against the cluster
   you have. It is a tunable, but a `NextFullBuild` one (§8.4).
3. **Full-rebuild cadence.** Default weekly. A catalogue growing by tens of thousands of series a
   week wants it more often; a static one wants it monthly. The incremental build covers changed
   series either way — the full rebuild exists for idf drift and for neighbours that changed
   because *something else* changed.
4. **`series_merges` scope.** It is proposed as a product-wide alias table (§9.2), which means a
   `301` on `GET /v1/series/{merged_id}` and a change to how the catalogue answers for ids it no
   longer holds. If you would rather keep it private to the recommender, it works unchanged — but
   the redirect is worth more than the recommender is, and the table already exists either way.
5. **Explain panel scope.** Default: the operator's own account plus arbitrary series (§10.4).
   Cross-user inspection is more useful for support and is an operator-facing window onto
   individual reading histories; if you want it, it needs its own permission, its own audit
   action, and a line in the privacy policy.
