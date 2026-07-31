# Kanpai / TankoVault — Performance & Speed Audit

> **Frozen snapshot — not a current description of the code.** Audited at `2c9a22e`
> (2026-07-29). Every finding below is written in the present tense as it was true at that
> commit, and none has been edited since. Most are now fixed.
> **[`PROGRESS.md`](./PROGRESS.md) is the authoritative status of every finding in this
> report** — check a finding there before acting on it. In particular, the measurements quoted
> below predate the fixes and are no longer reproducible.

Scope: backend runtime + build. Database, async runtime, allocations, HTTP client, caching,
serialization, compile time, WASM payload size. Security/architecture/testing/frontend-structure
are explicitly out of scope.

All findings are evidence-based against the tree at `E:\Rust\manga-tracker-v3`. Items I could not
verify by reading code are marked **UNVERIFIED**.

---

## 1. Worker rebuilds the entire HTTP fetch stack (and a new TLS client) for every scan task

**Impact: High**

**Evidence**

`services/worker/src/engine.rs:300-305`

```rust
pub(crate) async fn dispatch_task(&self, provider: &Provider, task: &ScanTaskMessage) -> anyhow::Result<()> {
    let (adapter, ctx) = self.provider_context(provider)?;
```

`services/worker/src/engine.rs:40-65` — `provider_context` calls `build_provider_fetcher(fetch_cfg)?`
on every invocation.

`crates/fetch/src/builder.rs:99-127`

```rust
pub fn build_provider_fetcher(cfg: ProviderFetchConfig) -> Result<Arc<dyn Fetcher>, FetchError> {
    let base = BaseHttpFetcher::new(cfg.user_agent, cfg.emulation, cfg.connect_timeout, cfg.request_timeout)?;
    ...
    let rated = RateLimitedFetcher::new(solving, cfg.rps, cfg.concurrency, cfg.crawl_delay_ms, cfg.throttle);
```

`crates/fetch/src/base.rs:90-101` — each `BaseHttpFetcher::new` calls `wreq::Client::builder()...build()`,
constructing a fresh BoringSSL-backed client **with its own connection pool**.

**Cost model**

Every `TaskKind::Series` task (one per catalogue entry — the memory notes `max_catalog_pages = 500`
and catalogues in the hundreds of thousands) gets a brand-new client. Consequences, per task:

- **Zero keep-alive reuse.** Every task pays TCP handshake + full TLS 1.3 handshake (BoringSSL,
  browser-emulation ClientHello) = 2–3 RTT before the first byte. At 80 ms RTT to a provider that is
  ~200 ms of pure handshake per task, and `process_series` issues *two* fetches
  (`fetch_series` + `fetch_chapters`, engine.rs:76-77) that cannot even share a connection with each
  other across tasks. For a 500k-series full scan: 500k handshakes that should have been ~`concurrency`
  handshakes.
- **Rate limiting is defeated across tasks.** `RateLimitedFetcher` owns a fresh `governor` cell and a
  fresh `tokio::sync::Semaphore` (`crates/fetch/src/ratelimit.rs:18-21`). A per-task limiter means the
  configured `rps` and `concurrency` are enforced *within one task only*. N concurrent worker tasks
  therefore offer N × rps to the provider — which is exactly what produces the 429 storms the
  `Throttle`/`BackoffFetcher` layers then spend wall-clock absorbing.
- **The self-imposed throttle penalty is discarded** every task (`Throttle::new` resets `penalty` to
  `Duration::ZERO`, ratelimit.rs:70-79), so the adaptive backoff never accumulates across the run.
- Client construction itself is not free (TLS config assembly, emulation profile, resolver `Arc`).

**Fix**

Build the fetch stack **once per provider** and cache it on the `Engine`:

```rust
pub(crate) struct Engine {
    ...
    fetchers: Arc<tokio::sync::RwLock<HashMap<ProviderId, (u64 /*config_version*/, Arc<dyn Fetcher>)>>>,
}
```

`provider_context` looks up by `provider.id`, rebuilding only when the provider's politeness/config
row changes (compare a hash of `provider.politeness` + `provider.config`). The adapter
(`build_adapter`) is cheap and stateless and can stay per-call, but the `Arc<dyn Fetcher>` must be
shared. This single change restores connection reuse *and* makes the per-provider rate limit actually
per-provider, which is what `crates/fetch/src/ratelimit.rs:4-7` claims ("The fetch stack is built per
provider, so a single direct limiter is exactly the per-provider limiter the design calls for") — that
comment is currently false.

**Effort: M**

---

## 2. `count(*) OVER()` on the Discover browse query forces a full scan+sort of `series` on every page load

**Impact: High**

**Evidence**

`crates/db/src/repo/catalog.rs:1148-1180` (the default `updated` sort; the same shape is repeated in
all five sort variants at :950, :999, :1049, :1098, :1148)

```sql
SELECT s.id, ..., 
       (SELECT count(DISTINCT ss.provider_id) FROM series_sources ss WHERE ss.series_id = s.id) AS "source_count!",
       count(*) OVER() AS "total!"
FROM series s
WHERE ($1::text IS NULL OR ...)
  AND ($2::text IS NULL OR s.content_type::text = $2)
  ...
ORDER BY s.updated_at DESC
LIMIT $10 OFFSET $11
```

Called from `services/api/src/series.rs:133` on `GET /v1/series` — the public, unauthenticated,
highest-traffic route. Default `limit = 40` (series.rs:60-62).

**Cost model**

Three compounding problems in one statement:

1. **`count(*) OVER()` defeats the `LIMIT`.** A window function without `PARTITION BY` is evaluated
   over the entire result set, so Postgres must materialise *every* matching row before the Limit node
   can take 40. An unfiltered browse on a 500k-row `series` table reads and sorts 500k rows to return
   40. O(n log n) per request where O(limit) was intended.
2. **No index backs `ORDER BY s.updated_at DESC`.** `migrations/0003_catalog.sql:35-37` creates only
   `series_search_gin`, `series_title_trgm`, `series_status_idx`. There is no btree on `updated_at`, so
   even without the window function this is a full sort.
3. **`OFFSET $11` deep pagination.** `page * limit` (series.rs:131) — page 500 discards 20 000 rows
   server-side. O(offset) per request.

The `($n IS NULL OR predicate)` guard style also blocks index usage even where an index exists: with
`$2` bound to a real value, `($2::text IS NULL OR s.content_type::text = $2)` is not recognised as a
sargable equality, and `s.content_type::text` casts away the enum index anyway.

**Fix**

- **Drop `count(*) OVER()` from the page query.** Return `X-Total-Count` from either (a) a separate,
  *cached* count issued only when `offset == 0`, or (b) an approximate count
  (`SELECT reltuples::bigint FROM pg_class WHERE relname='series'`) for the unfiltered case. The
  frontend already tolerates the header being absent (series.rs:135-144).
- Add the sort indexes (see the Missing indexes table): at minimum
  `CREATE INDEX series_updated_idx ON series (updated_at DESC, id DESC);`
- Move to **keyset pagination** on `(updated_at, id)` — the codebase already knows how
  (`list_series` is documented as "keyset pagination on `(created_at, id)`" at catalog.rs:789, though
  its body at :846 actually orders by `updated_at` with no cursor at all, so the doc comment is stale).
  Replace `OFFSET` with `WHERE (s.updated_at, s.id) < ($cursor_ts, $cursor_id)`.
- Rewrite the enum filters to be sargable: bind the enum type directly
  (`AND ($2::content_type IS NULL OR s.content_type = $2)`) rather than `::text` on both sides.

**Effort: M** (L if keyset pagination changes the public contract)

---

## 3. Notifier issues 3 sequential queries per watcher per new chapter

**Impact: High**

**Evidence**

`services/notifier/src/main.rs:177-225`

```rust
let watchers = tankovault_db::repo::tracking::watchers_for_series(pool, event.series_id).await?;
for watcher in watchers {
    ...
    let claimed = tankovault_db::repo::tracking::dedup_claim(pool, watcher.user_id, event.series_id, event.chapter_number).await?;
    if !claimed { continue; }
    ...
    let notification_id = tankovault_db::repo::tracking::notification_create(pool, watcher.user_id, "new_chapter", &payload).await?;
    if features.is_enabled(Feature::NotificationsLive) {
        push_live(pool, bus, watcher.user_id, notification_id, &payload).await;
    }
}
```

`push_live` (main.rs:245-259) adds a third query: `notifications_unread_count`.

**Cost model**

Per new chapter on a series with W watchers: **3W sequential round trips**, plus W pool acquisitions
(each of which, per finding #6, costs an extra ping RTT). At W = 10 000 and 0.5 ms/RTT that is ≥15 s of
serialized latency for one chapter event — and the worker fan-out publishes one `chapter.discovered`
per new chapter, so a series that drops 6 parts at once multiplies it. Also allocates a fresh
`serde_json::json!` payload per watcher (main.rs:205-211) plus `payload.clone()` inside `push_live`
(main.rs:264) — 2 heap JSON values per watcher for what is one immutable document.

**Fix**

Three set-based statements instead of 3W:

1. `dedup_claim` → batch insert with `UNNEST` + `ON CONFLICT DO NOTHING ... RETURNING user_id` to get
   the claimed subset in one round trip (`crates/db/src/repo/scans.rs:253-264` already demonstrates
   exactly this `UNNEST` + `ON CONFLICT` + `RETURNING` pattern).
2. `notification_create` → one `INSERT ... SELECT * FROM UNNEST(...) RETURNING id, user_id`.
3. `notifications_unread_count` → one grouped query for all claimed users:
   `SELECT user_id, count(*) FROM notifications WHERE user_id = ANY($1) AND read_at IS NULL GROUP BY user_id`.

Build the payload once outside the loop and pass `&payload` (it is identical for every watcher — only
`user_id` varies).

**Effort: M**

---

## 4. `series(updated_at)` is missing, and the enrichment sweep pages with OFFSET

**Impact: High**

**Evidence**

`crates/db/src/repo/catalog.rs:216-222`

```sql
SELECT id, canonical_title, description, cover_url FROM series
ORDER BY updated_at ASC LIMIT $1 OFFSET $2
```

Driven by `services/sync/src/engine.rs:1082-1107`:

```rust
let mut offset: i64 = 0;
while report.scanned < max_series {
    let rows = catalog::list_series_for_enrichment(&self.pool, batch_size, offset).await?;
    ...
    offset += i64::try_from(fetched).unwrap_or(i64::MAX);
```

**Cost model**

No index on `series.updated_at` (`migrations/0003_catalog.sql:35-37`), so **every batch** is a full
sequential scan + full sort of `series`, then discards `offset` rows. Sweeping a 500k catalogue at
`batch_size = 100` is 5 000 iterations × (500k-row sort) — quadratic in the catalogue size, O(n²/batch).
This is a background sweep so it does not block a request, but it saturates the database for the whole
duration and competes with the API's own scans of the same table.

Worse, the sweep **mutates `updated_at`** (`apply_enrichment` sets `updated_at = now()`, catalog.rs:268),
so the `ORDER BY updated_at ASC` key moves under the cursor: enriched rows jump to the end of the
ordering and the OFFSET skips *unenriched* rows. The sweep silently misses series.

**Fix**

```sql
CREATE INDEX series_updated_idx ON series (updated_at DESC, id DESC);
```

and replace the OFFSET walk with a keyset cursor that is stable under the mutation — page on
`(id)` ascending, or on `(updated_at, id)` captured against a fixed `now()` snapshot:

```sql
SELECT id, canonical_title, description, cover_url FROM series
WHERE (updated_at, id) > ($1, $2) AND updated_at < $3  -- $3 = sweep start timestamp
ORDER BY updated_at ASC, id ASC LIMIT $4
```

**Effort: S** (index) + **M** (cursor rewrite)

---

## 5. CSS selectors are recompiled for every element in every adapter parse loop

**Impact: High**

**Evidence**

`crates/adapters/src/html.rs:55-77` — both extractors parse the selector on **every call**:

```rust
pub fn extract_first(root: ElementRef<'_>, spec: &str) -> Result<Option<String>, AdapterError> {
    let (sel_str, attr) = split_attr(spec);
    let sel = parse_selector(sel_str)?;      // <-- Selector::parse per call
```

`crates/adapters/src/generic.rs:73-79` — called inside the per-item loop:

```rust
for el in root.select(&item_sel) {
    let Some(path) = extract_href(el, &self.config.catalog.link, &resp.url)? else { continue };  // parse_selector
    let title = extract_first(el, &self.config.catalog.title)?.unwrap_or_default();               // parse_selector
```

`generic.rs:102-121` (`list_latest`) does the same 3× per item.

Grep confirms there is **no `LazyLock`, `OnceLock`, `lazy_static`, or `once_cell` anywhere in
`crates/`** — the only hit for the whole family is `Selector::parse` itself at html.rs:19.

**Cost model**

`Selector::parse` runs the full `cssparser` tokenizer + selector-list parser and allocates a
`SelectorList`. It is on the order of 1–10 µs per call. A catalogue page with 100 items costs 200
parses in `list_catalog`; a sitemap-shard page (the module docs cite kunmanga at 20 000 entries,
worker/engine.rs:20-23) costs **40 000 parses of two constant strings** — tens to hundreds of
milliseconds of pure re-parsing per page, repeated for every page of every scan.

Note the selectors are *not* compile-time constants (they come from `providers.config`), so a plain
`LazyLock<Selector>` will not do — they must be cached per adapter instance.

**Fix**

Pre-compile once in `GenericConfigAdapter::new` and store `Selector` values on the struct:

```rust
pub struct GenericConfigAdapter {
    config: AdapterConfig,
    catalog_item: Selector,
    catalog_link: (Selector, Option<String>),
    catalog_title: (Selector, Option<String>),
    // ... one per configured spec
}
```

`new` becomes fallible (`Result<Self, AdapterError>`), which is strictly better: an invalid selector
in `providers.config` is caught once at adapter construction instead of mid-scan on element 40 000.
Keep the `extract_first(root, spec)` helpers for the one-shot `fetch_series` path but add
`extract_first_with(root, &Selector, attr)` for the loops.

For any truly constant patterns elsewhere, use `std::sync::LazyLock<Selector>`.

**Effort: M**

---

## 6. Every pool acquisition pays an extra round trip (`test_before_acquire` left at its default)

**Impact: High**

**Evidence**

`crates/db/src/pool.rs:19-23`

```rust
let pool = PgPoolOptions::new()
    .max_connections(max_connections)
    .acquire_timeout(Duration::from_secs(acquire_timeout_secs))
    .connect(url)
    .await?;
```

sqlx 0.9 default, verified in the vendored source (`sqlx-core-0.9*/src/pool/options.rs:149`):

```rust
test_before_acquire: true,
```

**Cost model**

With `test_before_acquire = true`, sqlx issues a liveness ping to Postgres on **every `acquire()`**.
Because the repo layer takes `E: PgExecutor` and handlers pass `&state.pool` per call, each repository
function is its own acquisition. `GET /v1/series/{id}` (finding #11) makes ~8 repo calls ⇒ ~8 extra
round trips purely for liveness checks — roughly doubling the request's database RTT count. The
notifier loop (finding #3) pays it 3W times.

Also missing: `min_connections` (cold pool ⇒ every burst pays TCP+TLS+auth to Postgres),
`idle_timeout`, `max_lifetime`.

**Fix**

```rust
let pool = PgPoolOptions::new()
    .max_connections(max_connections)
    .min_connections(min_connections)                 // new config knob; suggest max/4
    .acquire_timeout(Duration::from_secs(acquire_timeout_secs))
    .idle_timeout(Some(Duration::from_secs(600)))
    .max_lifetime(Some(Duration::from_secs(1800)))
    .test_before_acquire(false)
    .connect(url)
    .await?;
```

Disabling `test_before_acquire` is safe here: sqlx still detects a broken connection on use and the
`DbError` path already surfaces transport failures. If a belt-and-braces check is wanted, use
`before_acquire` with a "only ping if idle > 30s" closure instead of pinging unconditionally.

Separately, prefer passing a single `&mut PgConnection` through a composite read (see #11) so one
acquisition serves the whole request.

**Effort: S**

---

## 7. Frontend service ships the WASM bundle uncompressed, uncached, and without ETags

**Impact: High**

**Evidence**

`services/frontend/src/main.rs:145-175`

```rust
let bundle = ServeDir::new(static_dir).fallback(ServeFile::new(index));
let static_service = ServiceBuilder::new()
    .layer(SetResponseHeaderLayer::if_not_present(header::X_CONTENT_TYPE_OPTIONS, ...))
    .layer(SetResponseHeaderLayer::if_not_present(header::REFERRER_POLICY, ...))
    .layer(SetResponseHeaderLayer::if_not_present(header::X_FRAME_OPTIONS, ...))
    .service(bundle);
```

No `CompressionLayer`, no `precompressed_gzip()`/`precompressed_br()`, no `Cache-Control`, no ETag.
The API service *does* have compression (`crates/service/src/http.rs:97`,
`.layer(CompressionLayer::new())`) — the static tier is the one that missed it. The workspace already
enables `compression-full` on `tower-http` (`Cargo.toml:86`), so the codec is compiled in.

**Cost model**

A Dioxus 0.7 WASM bundle is typically 1.5–4 MB uncompressed and compresses ~4:1 with gzip, ~5:1 with
brotli. Serving it raw costs roughly **1–3 MB of extra transfer on every cold load**, which on a
10 Mbit connection is 1–2 s of added time-to-interactive.

Without `Cache-Control`, `ServeDir` falls back to `Last-Modified` + `If-Modified-Since` only. Every
repeat visit issues a conditional request per asset (wasm, js, css, index.html) and waits a full RTT
for the 304 before it can start — so a warm load still costs N round trips it should not.

**Fix**

```rust
use tower_http::compression::CompressionLayer;
use tower_http::set_header::SetResponseHeaderLayer;

let bundle = ServeDir::new(static_dir)
    .precompressed_br()
    .precompressed_gzip()
    .fallback(ServeFile::new(index));

let static_service = ServiceBuilder::new()
    .layer(CompressionLayer::new().br(true).gzip(true))
    .layer(SetResponseHeaderLayer::if_not_present(
        header::CACHE_CONTROL,
        HeaderValue::from_static("public, max-age=31536000, immutable"),
    ))
    ... // existing hardening headers
    .service(bundle);
```

Two caveats to get right:

- `index.html` must **not** be `immutable` — it is the SPA shell and changes every deploy. Route it
  through a separate branch with `Cache-Control: no-cache` so the hashed asset URLs inside it are
  always re-read. `dx` emits content-hashed filenames for the wasm/js, which is what makes
  `immutable` correct for the rest.
- Prefer `.precompressed_br()` with brotli files emitted at build time over on-the-fly compression:
  compressing a 3 MB wasm on every request burns CPU for a byte-identical result.

**Effort: S**

---

## 8. `reqwest::Client::new()` in the API has no timeout, and outbound pushes are spawned unbounded

**Impact: High**

**Evidence**

`services/api/src/main.rs:214`

```rust
http: reqwest::Client::new(),
```

`reqwest::Client::new()` sets **no request timeout and no connect timeout**. This client backs the
control-plane proxy, the sync proxy, and:

`services/api/src/me/progress.rs:256-283`

```rust
pub(super) fn spawn_targeted_push(state: &AppState, user_id: UserId, series_id: SeriesId) {
    ...
    let http = state.http.clone();
    tokio::spawn(async move {
        let url = format!("{}/v1/sync/push-series", sync_url.trim_end_matches('/'));
        match http.post(url).json(&body).send().await { ... }
    });
}
```

**Cost model**

Fire-and-forget `tokio::spawn` per mark-read request, with no semaphore and no timeout. If the `sync`
service hangs (rather than refusing connections), every mark-read leaks a task and a connection that
never completes. Task and socket count grows without bound until the process hits its fd limit — and
because these are detached, nothing sheds them under load. The user-facing handlers that proxy through
the same client will hang for the full TCP timeout (minutes) instead of failing fast.

Compare `services/frontend/src/main.rs:119-123`, which does set `connect_timeout` and deliberately
omits only the *request* timeout (correct — it proxies SSE). The API's client has neither, and it is
not proxying SSE on the `spawn_targeted_push` path.

**Fix**

```rust
http: reqwest::Client::builder()
    .connect_timeout(Duration::from_secs(5))
    .timeout(Duration::from_secs(30))
    .pool_idle_timeout(Duration::from_secs(90))
    .build()?,
```

If the same client must also carry the `/v1/me/stream` SSE proxy, build **two** clients: a
timeout-bounded one for RPC and an untimed one for streaming.

For `spawn_targeted_push`, bound the concurrency with a shared `Arc<Semaphore>` on `AppState` and
`try_acquire_owned()` — dropping the push when saturated is correct for a best-effort side effect and
strictly better than unbounded accumulation:

```rust
let Ok(permit) = state.push_limiter.clone().try_acquire_owned() else {
    tracing::debug!("targeted push shed: too many in flight");
    return;
};
tokio::spawn(async move { let _permit = permit; /* ... */ });
```

**Effort: S**

---

## 9. HTML parsing and hashing run CPU-bound on the async executor

**Impact: Medium-High**

**Evidence**

`crates/adapters/src/generic.rs:68`, `:97`, `:127`, `:233` — every adapter method does:

```rust
let resp = ctx.fetch(&path).await?;
let doc = Html::parse_document(&resp.body);
```

No `spawn_blocking` anywhere in the workspace — grep for `spawn_blocking|block_on|blocking_` returns
only `std::fs` calls in `xtask` and frontend tests.

`crates/fetch/src/base.rs:163` also does `String::from_utf8_lossy(&buf).into_owned()` on up to
`MAX_BODY_BYTES = 8 MiB` (base.rs:33), and `services/worker/src/engine.rs:546-558` (`content_hash`)
SHA-256s the whole chapter list, all inline.

**Cost model**

`Html::parse_document` on a large catalogue page is html5ever's full tokenize + tree-build: for a
500 KB–2 MB page this is ~5–50 ms of uninterruptible CPU on a Tokio worker thread. During that window
the thread serves no other task. The worker runs N of these concurrently, so with the default
multi-thread runtime sized to core count, a handful of concurrent large-page parses stalls *every*
async task on that runtime — including the JetStream ack deadlines the queue module is careful about
(`services/worker/src/queue.rs:74-81`).

`from_utf8_lossy(...).into_owned()` additionally copies the whole body a second time (buf → String) —
up to 8 MiB of memcpy per fetch, on top of the `Vec` growth in the streaming loop (base.rs:155-161),
which starts at `Vec::new()` and reallocates ~log2(n) times.

**Fix**

- Wrap the parse+extract phase in `tokio::task::spawn_blocking`. `scraper::Html` is not `Send` across
  await points, so move the whole "parse → extract → return owned `Vec<CatalogItem>`" block inside the
  closure:

  ```rust
  let body = resp.body;
  let items = tokio::task::spawn_blocking(move || -> Result<Vec<CatalogItem>, AdapterError> {
      let doc = Html::parse_document(&body);
      // ... extraction, returns owned data
  }).await.map_err(|e| AdapterError::…)??;
  ```

  This composes cleanly with #5 (pre-compiled selectors are `Send + Sync` and can be cloned/Arc'd in).
- In `base.rs`, pre-size the buffer from `Content-Length` when present:
  `let mut buf = Vec::with_capacity(len.min(MAX_BODY_BYTES));` and avoid the second copy by returning
  `Bytes`/`Vec<u8>` and decoding once at the parse site.

**Effort: M**

---

## 10. `GET /v1/series/{id}` is an N+1 over provider groups plus 6 more serialized queries

**Impact: Medium**

**Evidence**

`services/api/src/series.rs:212-261`

```rust
let series = ...::get_series(&state.pool, id).await?;                        // 1
let sources = ...::list_sources_for_series(&state.pool, id).await?;          // 2
let groups = group_sources_by_provider(&sources);
for group in &groups {
    chapter_counts.push(...::count_full_chapters_across(&state.pool, &group.member_ids).await?);   // N
}
for (i, group) in groups.iter().enumerate() {
    let provider = ...::providers::get(&state.pool, group.provider_id).await?;                     // N
    ...
}
let alt_titles = ...::list_series_titles(&state.pool, id).await?;            // 3
let tags       = ...::list_series_tags(&state.pool, id).await?;              // 4
let authors    = ...::list_series_authors(&state.pool, id).await?;           // 5
let anilist_id = ...::sync::mapping_external_for_series(&state.pool, id, "anilist").await?;  // 6
```

**Cost model**

`6 + 2N` sequential round trips for one page, where N = distinct providers on the series (realistically
2–6, so 10–18 queries). Each is a separate pool acquisition, so per finding #6 each carries an extra
liveness ping — effectively **20–36 round trips** for one series page. None of them depend on each
other except through `groups`.

`providers::get` inside the loop is pure N+1: the provider table is tiny, immutable-ish reference data.

**Fix**

Two steps, in order of value:

1. **Cache the provider table.** Providers change on operator action, not per request. Put a
   `FeatureGate`-style snapshot behind an `RwLock` with a timed refresh — `crates/service/src/flags.rs`
   is already exactly this pattern and can be generalised. This removes the N `providers::get` calls
   here *and* the per-request provider lookups elsewhere (`source_provider_base_url`, catalog.rs:1315).
2. **Batch the counts.** Replace the loop with one grouped query:

   ```sql
   SELECT ss.provider_id, count(DISTINCT floor(c.number)) AS cnt
   FROM series_sources ss JOIN chapters c ON c.series_source_id = ss.id
   WHERE ss.series_id = $1
   GROUP BY ss.provider_id
   ```

3. Run the four independent tail reads concurrently with `tokio::try_join!` (they touch different
   tables and share nothing):

   ```rust
   let (alt_titles, tags, authors, anilist_id) = tokio::try_join!(
       catalog::list_series_titles(&state.pool, id),
       catalog::list_series_tags(&state.pool, id),
       catalog::list_series_authors(&state.pool, id),
       sync::mapping_external_for_series(&state.pool, id, "anilist"),
   )?;
   ```

**Effort: M**

---

## 11. Ingest holds one transaction open across an unbounded per-row INSERT loop

**Impact: Medium**

**Evidence**

`crates/db/src/repo/catalog.rs:1367-1402` (`ingest_series`) opens `pool.begin()` and then, inside it:

- `add_series_titles` — `for (title, normalized) in titles { INSERT ... }` (catalog.rs:297-310)
- `add_series_tags` — `for name in tags { INSERT tags RETURNING id; INSERT series_tags }`
  — **two** statements per tag (catalog.rs:339-360)
- `add_series_authors` — same shape, two per author (catalog.rs:371-392)
- `for ch in &scanned.chapters { upsert_chapter(&mut *tx, ...).await? }` (catalog.rs:1392-1397)

**Cost model**

For a series with 1 200 chapters, 15 tags, 4 authors, 8 alt titles: `1200 + 30 + 8 + 8 = 1246`
round trips, all inside one transaction. At 0.3 ms RTT that is ~370 ms of transaction lifetime holding
row locks on `series`, `tags`, `authors`, and `chapters` — and `tags`/`authors` are *globally shared*
rows, so two concurrent workers ingesting series that share a genre serialise on the same `ON CONFLICT
(slug) DO UPDATE` row lock for the whole 370 ms. That is the classic recipe for lock convoying across
the worker pool, and it scales with catalogue width.

`ON CONFLICT (slug) DO UPDATE SET name = tags.name` (catalog.rs:346) is a no-op update whose only
purpose is to make `RETURNING id` fire — but it still takes a row-level write lock and burns a dead
tuple per call, bloating `tags` and forcing autovacuum churn.

**Fix**

Replace all four loops with set-based statements. `crates/db/src/repo/scans.rs:253-264` already shows
the house pattern:

```sql
-- chapters
INSERT INTO chapters (id, series_source_id, number, volume, title, path, published_at)
SELECT * FROM UNNEST($1::uuid[], $2::uuid[], $3::numeric[], $4::int[], $5::text[], $6::text[], $7::timestamptz[])
ON CONFLICT (series_source_id, number) DO UPDATE
  SET title = EXCLUDED.title, path = EXCLUDED.path,
      published_at = COALESCE(EXCLUDED.published_at, chapters.published_at)
RETURNING number::float8, (xmax = 0) AS "inserted!";
```

That preserves the `xmax = 0` new-chapter detection (catalog.rs:651-652) exactly, in **one** round trip
instead of 1 200.

For tags/authors, split into two set-based statements and avoid the write-lock-for-RETURNING trick:

```sql
-- 1) ensure rows exist, no-op conflicts do NOT update
INSERT INTO tags (slug, name) SELECT * FROM UNNEST($1::text[], $2::text[])
ON CONFLICT (slug) DO NOTHING;
-- 2) link, resolving ids by slug
INSERT INTO series_tags (series_id, tag_id)
SELECT $1, t.id FROM tags t WHERE t.slug = ANY($2::text[])
ON CONFLICT DO NOTHING;
```

**Effort: M**

---

## 12. `floor(number)` predicates are non-sargable across every tracking query

**Impact: Medium**

**Evidence**

`crates/db/src/repo/tracking.rs:720-723` (`watchlist_detailed`), `:789-792` and `:800`
(`continue_reading`), `:846-853` (`me_stats`), `crates/db/src/repo/catalog.rs:706` and `:744`:

```sql
WHERE ss.series_id = w.series_id
  AND floor(c.number) > COALESCE(rp.last_read_whole_number, 0)
```

The only chapter index is `chapters_source_idx ON chapters (series_source_id, number DESC)`
(`migrations/0003_catalog.sql:84`).

**Cost model**

Postgres cannot prove `floor(number) > k` is equivalent to `number >= k+1`, so the `number` half of the
index is unusable as a range bound. Every one of these becomes: index-scan the source's chapters by
`series_source_id`, then filter **all** of them. For a 1 200-chapter series that is 1 200 rows read to
answer "how many are above 900".

`continue_reading` (tracking.rs:781-803) is the worst case: **four** correlated subqueries per
watchlist row — `next_number`, `unread`, the `EXISTS` guard, and the `ORDER BY max(c.discovered_at)` —
each re-scanning the same chapter set. A user with 200 watchlist entries × 1 200 chapters =
**960 000 row reads** for one `GET /v1/me/continue`.

`me_stats` (tracking.rs:846-853) is worse still: `SELECT DISTINCT w.series_id, floor(c.number)` across
the user's entire watchlist joined to every chapter of every source — a full materialisation with a
sort/hash-aggregate, per request, uncached.

**Fix**

Add an expression index so the predicate becomes sargable:

```sql
CREATE INDEX chapters_source_floor_idx ON chapters (series_source_id, (floor(number)));
```

and rewrite `continue_reading` to compute all four values in **one** pass with a lateral join instead
of four correlated subqueries:

```sql
FROM watchlist_entries w
JOIN series s ON s.id = w.series_id
LEFT JOIN read_progress rp ON rp.user_id = w.user_id AND rp.series_id = w.series_id
CROSS JOIN LATERAL (
  SELECT min(c.number)::float8                          AS next_number,
         count(DISTINCT floor(c.number))                AS unread,
         max(c.discovered_at)                           AS last_activity
  FROM series_sources ss JOIN chapters c ON c.series_source_id = ss.id
  WHERE ss.series_id = w.series_id
    AND floor(c.number) > COALESCE(rp.last_read_whole_number, 0)
) agg
WHERE w.user_id = $1 AND w.status IN ('reading','planned','paused') AND agg.unread > 0
ORDER BY agg.last_activity DESC NULLS LAST, w.series_id
```

That is a 4× reduction in chapter scans on its own, before the index.

Longer term, the honest fix is a **materialised per-(series) chapter summary** —
`series_chapter_stats(series_id, whole_chapter_count, max_number, last_discovered_at)` maintained by
the ingest path — so the dashboard reads one indexed row per series instead of aggregating chapters.
`docs/READING_PROGRESS_AND_SYNC.md` already proposes a chapter ledger (per the memory index); this is
the read-model half of it.

**Effort: S** (index) + **M** (lateral rewrite) + **L** (summary table)

---

## 13. Sync reconcile issues ~6 sequential queries per remote entry, plus a title-match query per synonym

**Impact: Medium**

**Evidence**

`services/sync/src/engine.rs:533-573` — per remote entry:

```rust
for entry in &entries {
    let matched = self.resolve_series(slug, entry).await?;             // 1 + K (see below)
    sync::upsert_remote_entry(&self.pool, ...).await?;                 // 2
    ...
    sync::upsert_mapping(&self.pool, series_id, slug, &entry.external_id).await?;  // 3
    self.reconcile_series(...).await?;                                 // 4,5,6+
```

`reconcile_series` (engine.rs:637-646) opens with three more:

```rust
if tracking::is_sync_excluded(&self.pool, user_id, series_id, slug).await? { ... }
let local_state = tracking::progress_state(&self.pool, user_id, series_id).await?;
let local_status_opt = tracking::watchlist_status_get(&self.pool, user_id, series_id).await?;
```

And `resolve_series` (engine.rs:1003-1032) runs `matching::find_candidates` **once per distinct
normalized title** — AniList entries carry romaji/english/native + synonyms, so K is typically 3–8.
Each `find_candidates` (`crates/db/src/repo/matching.rs:40-60`) is a trigram scan of `series` with two
correlated `array_agg` subqueries per candidate row.

The local-driven pass (engine.rs:578-602) then repeats the pattern per watchlist entry.

**Cost model**

For a 500-entry AniList library: `500 × (6 + K)` ≈ **4 500–7 000 fully sequential round trips**, plus
500 × K trigram scans. At 0.5 ms/RTT that is 3+ seconds of pure database latency before any AniList
API time, all inside one user's sync run. The three `reconcile_series` preamble queries in particular
are pure per-row lookups against tables keyed by `(user_id, series_id)` — trivially batchable.

**Fix**

- Hoist the three preamble lookups out of `reconcile_series` and prefetch them **once per run** for the
  whole user: `is_sync_excluded`, `progress_state`, and `watchlist_status_get` are all
  `WHERE user_id = $1` tables. Load them into three `HashMap<SeriesId, _>` before the loop and pass
  the maps down. That removes 3 round trips × entries.
- Batch `upsert_remote_entry` and `upsert_mapping` with `UNNEST` after the resolve pass instead of
  per entry.
- In `resolve_series`, issue the K candidate queries **as one query** with
  `normalized = ANY($1::text[])` and `similarity` computed against the best-matching element, rather
  than K sequential scans.

**Effort: M**

---

## 14. `/scalar` re-serializes the 253 KB OpenAPI document on every request

**Impact: Medium**

**Evidence**

`services/api/src/lib.rs:181`

```rust
.merge(Scalar::with_url("/scalar", api))
```

`utoipa-scalar-0.3/src/lib.rs:237-243`:

```rust
pub fn to_html(&self) -> String {
    self.html.replace(
        "$spec",
        &serde_json::to_string(&self.openapi).expect(...),
    )
}
```

`openapi.json` on disk is 253 779 bytes (generated at build time by `xtask openapi`,
`xtask/src/main.rs:120-127` — that part is correct and costs nothing at runtime).

**Cost model**

Every `GET /scalar` walks the entire in-memory `OpenApi` tree with serde, allocates a ~253 KB `String`,
then `String::replace` allocates a **second** ~260 KB buffer for the templated HTML. That is ~0.5 MB
of allocation and several milliseconds of CPU per request, on an unauthenticated route. It is a cheap
amplification target and it is pure waste — the document is immutable after boot.

**Fix**

Render once at startup and serve the resulting bytes as a static response:

```rust
static SCALAR_HTML: OnceLock<String> = OnceLock::new();
let html = SCALAR_HTML.get_or_init(|| Scalar::new(api.clone()).to_html());
router.route("/scalar", get(|| async { Html(html.as_str()) }))
```

Same for any `/api-docs/openapi.json` route: precompute the serialized `String` (or `Bytes`) at boot.
The `CompressionLayer` at `crates/service/src/http.rs:97` will then gzip a constant body, which is
also cacheable with an ETag.

**Effort: S**

---

## 15. `register_source_stubs` opens one transaction per new entry

**Impact: Medium**

**Evidence**

`crates/db/src/repo/catalog.rs:516-547`

```rust
let known: HashSet<String> = sqlx::query_scalar!(
    "SELECT source_path FROM series_sources WHERE provider_id = $1 AND source_path = ANY($2)", ...
).fetch_all(pool).await?...;

for (path, title) in entries {
    if known.contains(*path) { continue; }
    match register_source_stub(pool, provider_id, path, title).await { ... }
}
```

and `register_source_stub` (catalog.rs:468-502) does `pool.begin()` → existence check →
`resolve_canonical_series` → `upsert_source` → `commit` — 4+ round trips and a full transaction each.

**Cost model**

The batched existence check is genuinely good and the doc comment is right that it makes a *re-scan*
cheap. But a **first** scan is all-new: 20 000 fresh entries on a sitemap page ⇒ 20 000 transactions ×
~5 round trips = 100 000 round trips, ~30–60 s for one page. That is what will blow the JetStream ack
deadline the queue module warns about (`services/worker/src/queue.rs:74-81`), causing redelivery and
duplicated work — a self-amplifying slowdown on exactly the scans that matter most.

Also note the redundant existence check: `register_source_stub` re-queries
`SELECT id FROM series_sources WHERE provider_id = $1 AND source_path = $2` (catalog.rs:477-483) even
though the caller already filtered on `known`.

**Fix**

Batch the whole thing. Canonicalisation genuinely cannot be fully batched (the comment at
catalog.rs:509-511 is correct — each new series resolves against those created before it), but the
transaction boundary can:

- Open **one** transaction per chunk of ~500 entries and run the per-entry canonicalisation inside it,
  rather than one transaction per entry. Cuts the begin/commit round trips by 500×.
- Drop the redundant per-entry existence check when called from `register_source_stubs` (split into
  `register_source_stub_in_tx(tx, ...)` without the pre-check).
- The trivially-batchable tail — `upsert_source` for entries whose canonical series already resolved
  to an existing row — can go through one `UNNEST` insert.

**Effort: M**

---

## 16. `FairQueue` polls every lane sequentially, so pickup latency scales with provider count

**Impact: Low-Medium**

**Evidence**

`services/worker/src/queue.rs:237-265`

```rust
for _ in 0..lane_count {
    let lane = &self.lanes[take_turn(&mut self.cursor, lane_count)];
    match take_one(lane).await { ... }
}
```

`take_one` (queue.rs:289-296) issues a `no_wait` fetch — one NATS round trip — and awaits it before the
next lane is tried.

**Cost model**

An idle round costs `providers × 2 tiers` sequential NATS round trips. At 25 providers and 1 ms RTT
that is 50 ms per poll round, per worker task, forever — and the `IDLE_POLL_MIN..MAX` backoff
(queue.rs:46-49) sits *on top* of it. Worse, a task arriving in the last-polled lane waits the full
round. It is chatter proportional to fleet size × provider count.

**Fix**

Within a tier, issue the lane fetches concurrently and take the first non-empty result — fairness is
preserved because the cursor still decides *ordering* and only one message is consumed:

```rust
let results = futures::future::join_all(order.iter().map(|i| take_one(&self.lanes[*i]))).await;
```

This does mean a concurrent round may pull >1 message; the module doc (queue.rs:74-81) is right that
buffering a claimed task is unsafe. The safe variant is to `nak()` the extras immediately so they are
redelivered without waiting out the ack deadline, or to keep a strict `select_ok` over the first tier
only. If neither is acceptable, at minimum raise `IDLE_POLL_MIN` and make the round short-circuit on
tiers known to be empty from the previous round.

**UNVERIFIED**: whether `BrokerConsumer::fetch` supports a batched multi-subject pull in this
`async-nats` version, which would be the cleanest fix.

**Effort: M**

---

## 17. Dev profile is entirely untuned

**Impact: Medium (developer throughput, not runtime)**

**Evidence**

Root `Cargo.toml` contains **only** `[profile.release]` (Cargo.toml:160-165). There is no
`[profile.dev]`, no `[profile.test]`, no `[profile.dev.package."*"]`, and no `.cargo/config.toml`
(verified absent).

Dependency scale: 594 entries in `Cargo.lock`. The tree includes both TLS stacks — `boring-sys2`
(Cargo.lock:515, a C/asm BoringSSL build) *and* `rustls` (Cargo.lock:5465 region) — plus `chromiumoxide`,
`scraper`, and the full `sqlx` macro machinery.

Version duplication that costs compile time: `hashbrown` ×4 (0.12.3, 0.14.5, 0.16.1, 0.17.1),
`getrandom` ×3 (0.2.17, 0.3.4, 0.4.3).

**Cost model**

Defaults for `dev` are `opt-level = 0`, `debug = 2` (full DWARF), `incremental = true`. Full debuginfo
across ~600 crates produces very large object files and dominates **link** time, which is paid on every
single edit-rebuild cycle. `opt-level = 0` dependencies also make tests crawl — `argon2` (Cargo.toml:135)
is deliberately expensive by design and is typically 10–50× slower unoptimised, so every auth test pays
for it.

**Fix**

```toml
[profile.dev]
debug = "line-tables-only"   # keeps backtraces, drops the bulk of DWARF
incremental = true

# Dependencies change rarely: optimise them once, keep workspace crates at opt-level 0.
[profile.dev.package."*"]
opt-level = 2
debug = false

# argon2 is intentionally slow; unoptimised it dominates test wall-clock.
[profile.test.package.argon2]
opt-level = 3
```

Optionally add a faster linker in `.cargo/config.toml` (`lld` on Windows/Linux). This is usually the
single largest edit-rebuild win in a workspace this size.

For **CI release** builds, note `lto = "thin"` + `codegen-units = 1` (Cargo.toml:162-163) is a
deliberate runtime-speed choice with a real build-time cost. That trade-off is defensible; just be
aware it is why release CI is slow, and consider a separate `[profile.ci]` inheriting release with
`lto = false, codegen-units = 16` for PR builds that only need to compile and test.

**Effort: S**

---

## 18. The `api` binary links two complete TLS stacks

**Impact: Low-Medium (build time + image size)**

**Evidence**

`services/api/Cargo.toml`:

```
20: tankovault-adapters = { workspace = true }
22: tankovault-fetch    = { workspace = true }   # -> wreq -> boring-sys2 (BoringSSL, C/asm)
38: reqwest             = { workspace = true }   # -> rustls
```

`services/frontend`, `services/sync`, `services/notifier` pull only `reqwest`; only `worker` and `api`
pull `tankovault-fetch`.

**Cost model**

`boring-sys2` compiles BoringSSL from C and assembly — typically 2–5 minutes of cold build, and it is
rebuilt for every target/profile combination. Dragging it (plus `scraper`/html5ever) into the `api`
binary inflates cold CI build time and the static musl binary size, for what is — reading
`services/api/src/admin/providers.rs` — the operator-only "Test adapter" dry-run.

**Fix**

Put the adapter dry-run behind a Cargo feature on the `api` crate, default-off in the production image:

```toml
[features]
default = []
adapter-dryrun = ["dep:tankovault-fetch", "dep:tankovault-adapters"]
```

Or move the dry-run endpoint into the `worker`/`control-plane` service (which already carries the
stack) and have the API proxy to it — it already proxies to control-plane and sync
(`AppState::control_plane_url`, `sync_url`, state.rs:40-42), so the pattern exists.

**UNVERIFIED**: exact binary-size delta; measure with `cargo bloat --release -p tankovault-api`.

**Effort: M**

---

## 19. Miscellaneous allocation and hot-path waste

**Impact: Low**

- **`register_source_stubs` allocates a `Vec<String>` copy of every path** purely to bind the `ANY($2)`
  parameter (`crates/db/src/repo/catalog.rs:525`):
  ```rust
  let paths: Vec<String> = entries.iter().map(|(path, _)| (*path).to_owned()).collect();
  ```
  For a 20 000-entry page that is 20 000 heap allocations discarded immediately after. sqlx accepts
  `&[&str]` for `text[]`; bind the borrowed slice.

- **`Vec::new()` in parse loops** — `crates/adapters/src/generic.rs:75` (`items`), `:101` (`updates`),
  `:241` (`chapters`) all start empty and grow by repeated doubling. `Vec::with_capacity` from the
  selector match count is free information (`root.select(&sel).count()` is a second pass, but
  `with_capacity(64)` as a floor costs nothing).

- **`FetchResponse.headers` materialises every response header into owned `String`s**
  (`crates/fetch/src/base.rs:142-151`) — 2 allocations per header × ~20 headers × every fetch, when
  the adapters read almost none of them.

- **Double body copy in `base.rs:155-163`** — `Vec<u8>` accumulation then
  `String::from_utf8_lossy(&buf).into_owned()`. Covered in #9.

- **`ScannedSeries` construction clones the whole parsed payload**
  (`services/worker/src/engine.rs:83-110`): `meta.title.clone()` ×2, `meta.description.clone()`,
  `meta.cover_url.clone()`, `meta.tags.clone()`, `meta.authors.clone()`, and a full re-map of
  `chapters` cloning `title` and `path` per chapter. `meta` and `chapters` are owned locals that are
  only read afterwards for the `chapter.discovered` fan-out (engine.rs:116-136) — restructure to move
  the fields in and look up the fan-out data by index instead of re-searching
  `chapters.iter().find(...)` per new chapter (which is O(new × total) — engine.rs:117-120).

- **`services/notifier/src/main.rs:205-211`** rebuilds an identical `serde_json::json!` payload per
  watcher, then `payload.clone()` again inside `push_live` (main.rs:264). Build once, borrow.

**Effort: S each**

---

## 20. WASM payload configuration — already correct

**Impact: n/a (positive finding)**

`web/frontend/Cargo.toml` release profile is properly tuned and the reasoning is documented:

```toml
[profile.release]
opt-level = "s"      # measured byte-identical to "z" after dx's wasm-opt -Oz; "s" compiles faster
lto = "fat"
codegen-units = 1
panic = "abort"
strip = "symbols"
```

`[profile.dev] opt-level = 1, debug = false` is also right for `dx serve` iteration.

No change recommended to the profile. The remaining WASM-size levers are outside it:

- The bundle carries **both** `gloo-net` and `reqwest` (`web/frontend/Cargo.toml:26-28`) — two HTTP
  client abstractions over the same browser `fetch`. Dropping one would remove a meaningful chunk of
  the wasm. **UNVERIFIED**: which call sites use which; needs `cargo bloat`/`twiggy` to size.
- Serving the bundle without compression (finding #7) wastes far more bytes on the wire than any
  further `opt-level` tuning could recover — fix #7 first.

---

# Missing indexes

Cross-referenced against every `CREATE INDEX` in `migrations/` (0003, 0004, 0005, 0006, 0007, 0008,
0012, 0014, 0017, 0018 — verified exhaustively).

| Table | Columns needed | Supporting query (file:line) | Why the existing indexes miss it |
|---|---|---|---|
| `series` | `(updated_at DESC, id DESC)` | `catalog.rs:1179` `ORDER BY s.updated_at DESC`; `catalog.rs:846` same; `catalog.rs:219` `ORDER BY updated_at ASC` | Only `series_search_gin`, `series_title_trgm`, `series_status_idx` exist. **Highest priority** — backs 3 distinct hot paths. |
| `series` | `(canonical_title)` | `catalog.rs:981` `ORDER BY s.canonical_title ASC` (`sort=title`) | No btree on the title; full sort per page. |
| `series` | `(release_year)` | `catalog.rs:1163-1166` `year_min`/`year_max` filters; `sort=year` variant | No index; range filter forces a scan. |
| `series` | `(created_at DESC, id DESC)` | `catalog.rs:789` documents keyset pagination on `(created_at, id)` | The documented cursor has no index (and the query no longer uses it — see #2). |
| `chapters` | `(series_source_id, (floor(number)))` — expression index | `tracking.rs:720-723`, `:789-792`, `:800`, `:852`; `catalog.rs:706`, `:744` | `chapters_source_idx (series_source_id, number DESC)` cannot serve `floor(number) > k`. See #12. |
| `chapters` | `(series_source_id, discovered_at DESC)` | `tracking.rs:801-803` `ORDER BY (SELECT max(c.discovered_at) ... WHERE ss.series_id = ...)` | `chapters_discovered (discovered_at DESC)` is global, not per-source; the correlated `max()` scans all of a source's rows. |
| `notifications` | `(user_id, created_at DESC)` | `tracking.rs:492-493` `WHERE user_id = $1 ORDER BY created_at DESC LIMIT $2` | `notifications_user_unread` is **partial** (`WHERE read_at IS NULL`, migrations/0004:50-51); the list endpoint returns read + unread, so it cannot use it. |
| `watchlist_entries` | `(user_id, added_at DESC)` | `tracking.rs:728` `WHERE w.user_id = $1 ORDER BY w.added_at DESC` | PK is `(user_id, series_id)`; the sort key is unindexed. |
| `watchlist_entries` | `(user_id, status)` | `tracking.rs:796` `WHERE w.user_id = $1 AND w.status IN (...)`; `tracking.rs:842-843` `AND status = 'reading'` / `'completed'` | PK prefix gets `user_id` but `status` is filtered post-scan. Low value at small W; add if watchlists get large. |
| `series_tags` | `(tag_id, series_id)` | `tracking.rs:885-889` `liked_tags` CTE, and `:896-902` reverse tag lookups | PK is `(series_id, tag_id)` — a reverse lookup by `tag_id` has no leading-column index. **UNVERIFIED** which direction the planner actually chooses; confirm with `EXPLAIN` before adding. |
| `refresh_tokens` | `(user_id, expires_at) WHERE revoked_at IS NULL` | `users.rs:573`, `user_admin.rs:177` `WHERE user_id = $1 AND revoked_at IS NULL AND expires_at > now()` | `refresh_user_idx (user_id)` exists but the two extra predicates filter post-scan. Low volume; low priority. |

### DDL

```sql
-- 0020_perf_indexes.sql
CREATE INDEX CONCURRENTLY series_updated_idx        ON series (updated_at DESC, id DESC);
CREATE INDEX CONCURRENTLY series_created_idx        ON series (created_at DESC, id DESC);
CREATE INDEX CONCURRENTLY series_title_sort_idx     ON series (canonical_title);
CREATE INDEX CONCURRENTLY series_release_year_idx   ON series (release_year) WHERE release_year IS NOT NULL;

CREATE INDEX CONCURRENTLY chapters_source_floor_idx ON chapters (series_source_id, (floor(number)));
CREATE INDEX CONCURRENTLY chapters_source_disc_idx  ON chapters (series_source_id, discovered_at DESC);

CREATE INDEX CONCURRENTLY notifications_user_all_idx ON notifications (user_id, created_at DESC);
CREATE INDEX CONCURRENTLY watchlist_user_added_idx   ON watchlist_entries (user_id, added_at DESC);
```

`CONCURRENTLY` cannot run inside a transaction — sqlx's migrator wraps each migration in one, so
either mark the migration `-- no-transaction` (sqlx supports this directive) or drop `CONCURRENTLY`
and accept the lock on a maintenance window.

---

# Quick wins (< 30 minutes each)

1. **`test_before_acquire(false)` + `min_connections` in `crates/db/src/pool.rs:19-23`** (#6) — removes
   one round trip from *every* repository call in every service. One-line change, largest
   effort-to-impact ratio in this report.
2. **Add the index migration above** (#4, #2, #12) — `series_updated_idx` alone fixes the Discover
   default sort, the `list_series` fallback, and the enrichment sweep's sort.
3. **`CompressionLayer` + `Cache-Control` on `services/frontend/src/main.rs:145-167`** (#7) — ~4:1 on
   the WASM bundle and eliminates per-asset revalidation round trips on warm loads.
4. **Timeouts on `reqwest::Client::new()` at `services/api/src/main.rs:214`** (#8) — 4 lines; prevents
   unbounded task/socket accumulation when `sync` or `control-plane` hangs.
5. **Precompute the Scalar HTML once at boot** (`services/api/src/lib.rs:181`, #14) — removes ~0.5 MB
   of allocation and a full serde walk per `/scalar` request.
6. **Drop `count(*) OVER()` when `offset > 0`** (#2) — even without the full keyset rewrite, only the
   first page needs a total. Immediately removes the full-set materialisation from every page beyond
   the first.
7. **Add `[profile.dev]` / `[profile.dev.package."*"]` to the root `Cargo.toml`** (#17) — pure
   developer-throughput win, no runtime risk.
8. **Bind `&[&str]` instead of the `Vec<String>` copy at `catalog.rs:525`** (#19) — removes 20 000
   allocations per large catalogue page.
9. **Hoist the payload construction out of the notifier watcher loop** (`main.rs:205-211`, #19) — build
   once, borrow; drops 2 JSON allocations per watcher.
10. **Pre-size the fetch body buffer from `Content-Length`** (`crates/fetch/src/base.rs:155`, #19) —
    removes ~log2(n) reallocations per fetch.

---

# Summary by theme

| Theme | Verdict |
|---|---|
| Database — indexes | Materially incomplete. `series(updated_at)` is missing and backs three hot paths; `floor(number)` predicates have no expression index anywhere. |
| Database — pagination | OFFSET everywhere, plus `count(*) OVER()` which defeats `LIMIT` entirely on the busiest public route. |
| Database — batching | Mixed. `scans::create_tasks` is an exemplary `UNNEST` batch; `ingest_series`, `add_series_tags/authors/titles`, and the notifier fan-out are per-row loops, some inside long transactions. |
| Database — pool | Defaults left unchanged; `test_before_acquire` costs a round trip per repo call. |
| Async runtime | No `spawn_blocking` anywhere despite html5ever parsing in every adapter. No unbounded `spawn` storms except `spawn_targeted_push`. Concurrency is generally *too serial* rather than unbounded. |
| Regex/selector caching | Absent — zero `LazyLock`/`OnceLock` in `crates/`; selectors recompile per element. |
| HTTP client | Correctly configured *shape* (emulation, redirect policy, body cap) but constructed per task, defeating both connection reuse and rate limiting. API client has no timeouts. |
| Caching | `FeatureGate` is a well-built snapshot cache and the right model. Nothing else uses it — providers, permissions, and the OpenAPI document are all re-fetched or re-serialized per request. |
| Serialization | `openapi.json` is correctly build-time generated. `/scalar` re-serializes it per request. Static assets are uncompressed. |
| Build time | Release profile deliberate and defensible. Dev profile entirely untuned; two TLS stacks in the `api` binary. |
| WASM size | Already correct and well-documented. Only remaining lever is the duplicate HTTP client crates, and compression on the serving side. |
