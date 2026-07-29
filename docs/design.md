# TankoVault — Manga Source Aggregator & Tracker

**Implementation & Handoff Specification (v1.0)**

A fully-Rust, multi-microservice system that indexes manga/manhwa/manhua metadata across many
provider sites, links (never mirrors) their sources, and layers a user tracking, notification, and
external-sync experience on top. This document is the authoritative build brief for the implementing
agent. It is decisive on library choices and data shapes so the implementer can proceed without
re-deciding architecture at every step.

> **Scope boundary (read first).** This system stores **links and metadata only**. It never
> downloads, stores, or serves chapter images or page content. It
> performs **fast bot-management detection** (Cloudflare managed/JS challenges, Turnstile, and similar
> interstitials) and, when one is detected, delegates the crawl to a dedicated, pluggable **challenge
> solver microservice** (FlareSolverr by default) to obtain a valid session and proceed. The solver is
> a *modular, extensible* component behind a stable trait, so alternative back-ends can be swapped in
> without touching the crawl pipeline. Where a provider exposes an official API, that path is still
> preferred. See §9 for the exact detection/solve posture. Operators remain responsible for the
> legality of crawling any given source in their jurisdiction and under each site's terms.

---

## 0. Table of contents

1. Product goals & non-goals
2. System topology (the microservices)
3. Workspace & crate layout
4. Technology decisions (locked)
5. Domain model & the "links, not content" principle
6. PostgreSQL schema (optimised)
7. The provider adapter framework (the core abstraction)
8. Scan orchestration: full vs. fast modes
9. The fetch layer & crawl posture
10. Series canonicalisation (matching one manga across providers)
11. API service (contract)
12. Control plane
13. Worker service
14. Notification service
15. External sync service (AniList)
16. Authentication, authorisation & security
17. Frontend: Dioxus + Tailwind (design system + screens)
18. Observability & operations
19. Deployment
20. Phased delivery roadmap
21. Definition of done / acceptance criteria

---

## 1. Product goals & non-goals

### Goals
- Aggregate metadata (title, alt titles, description, tags/genres, cover URL, status, author) and
  **chapter link lists** from many independent provider sites.
- Treat a single manga as one **canonical series** with many **provider sources**, so a reader sees
  "this title is on provider A, B, and C" and can jump to any of them.
- Two scan cadences: a rare **full scan** (rebuild the whole archive) and a frequent **fast scan**
  (detect only new chapters from each provider's "latest" feed).
- A user system with watchlists, read progress, and per-title notification opt-in.
- **Domain-migration resilience**: when a provider moves to a new domain, one field changes and every
  stored link resolves correctly. Stored links are relative paths, resolved against the provider base
  URL at read time.
- Provider onboarding **without code** for the common case (config-driven adapters), because most of
  these sites share the same CMS.
- Back-sync watchlist/progress to **AniList**.
- An operator web console showing live scan progress, provider health, and adapter config editing.
- Horizontal scale on the API and worker tiers.

### Goals (crawl resilience)
- **Detect bot-management challenges fast** (a cheap classifier on status codes, headers, and body
  markers) so a challenged fetch is recognised in milliseconds rather than after a timeout.
- **Bypass challenges through a dedicated solver microservice** (FlareSolverr by default) exposed
  behind a `ChallengeSolver` trait, so the mechanism is modular and new solver back-ends can be added
  without changing the crawl pipeline or the adapters.

### Non-goals
- No hosting, caching, proxying, or downloading of chapter images or page content.
- No account-farming or credential use against provider sites.
- No manual, per-CAPTCHA human-solving loop baked into the crawler; challenge solving is delegated to
  the pluggable solver service, not hand-rolled in the worker.

---

## 2. System topology (the microservices)

Eight deployable services plus shared libraries. Each is a separate binary in one Cargo workspace.
The **challenge-solver** service is the new bot-management bypass tier: workers call it over HTTP when
the fetch stack detects a challenge, and it fronts a pluggable solver back-end (FlareSolverr by
default) plus an optional headless-render fallback.

```
                         ┌──────────────────────────┐
                         │        Web Frontend        │  Dioxus (WASM SPA) + Tailwind
                         │      (webfront service)    │  static assets served by API/CDN
                         └─────────────┬──────────────┘
                                       │ HTTPS (REST + SSE/WS)
                         ┌─────────────▼──────────────┐
                         │        API service          │  Axum. Public edge. AuthN/Z,
                         │  (public read/write + admin) │  read models, SSE scan feed.
                         └───┬─────────────────────┬───┘
                             │                     │
            reads/writes     │                     │  enqueue / control
                             ▼                     ▼
                     ┌───────────────┐    ┌────────────────────┐
                     │  PostgreSQL    │    │   Control plane     │  scheduler, run planner,
                     │  (system of    │◄──►│   (orchestrator)    │  task fan-out, health.
                     │   record)      │    └─────────┬──────────┘
                     └───────┬────────┘              │ tasks via NATS JetStream
                             │                       ▼
                     ┌───────┴────────┐    ┌────────────────────┐
                     │     Redis       │    │   Worker pool       │  N replicas. Fetch + parse
                     │ (cache, rate-   │◄──►│   (scan workers)    │  via adapters, upsert deltas.
                     │  limit, locks)  │    └─────────┬──────────┘
                     └────────────────┘              │ domain events
                             ▲                        ▼
                  ┌──────────┴─────────┐   ┌────────────────────┐   ┌────────────────────┐
                  │ Notification svc    │   │  Sync service       │   │  (optional) Render │
                  │ (new-chapter →      │   │  (AniList push/pull)│   │  service: headless │
                  │  user notifications)│   └────────────────────┘   │  browser for JS SPA │
                  └─────────────────────┘                            │  provider pages     │
                                                                     └────────────────────┘
            solve request ▲ (HTTP) when the fetch stack flags a challenge
                          │
                  ┌───────┴────────────────────┐
                  │    Challenge-solver service    │  Modular bypass tier. Detects &
                  │  (FlareSolverr-backed default; │  solves Cloudflare/JS/Turnstile
                  │   pluggable ChallengeSolver)   │  challenges; returns cookies +
                  │   + optional headless fallback │  UA + solved HTML to the worker.
                  └──────────────────────────────┘
```

**Message bus:** NATS JetStream for task distribution and domain events (durable, at-least-once,
backpressure-aware, tiny footprint, first-class Rust client via `async-nats`). PostgreSQL remains the
durable system of record; the `scan_tasks` table mirrors task lifecycle so progress survives a broker
restart and can be queried directly by the API for the console.

**Why a broker and not just a Postgres queue.** A Postgres `SKIP LOCKED` queue is included as the
durability/audit layer, but JetStream gives per-provider work streams, consumer groups for worker
autoscaling, and natural backpressure without hammering the DB with polling under high fan-out. The
task row is the truth for *progress and audit*; the stream is the truth for *dispatch*.

---

## 3. Workspace & crate layout

Single Cargo workspace. Binaries are thin; logic lives in libraries so it is testable and reusable.

```
tankovault/
├── Cargo.toml                      # [workspace] members, shared deps, lints
├── crates/
│   ├── domain/                     # pure types: Series, Chapter, Provider, enums. No I/O.
│   ├── db/                         # sqlx repositories, migrations, query modules
│   ├── adapters/                   # SourceAdapter trait + Madara/config-driven + custom adapters
│   ├── fetch/                      # Fetcher trait, browser emulation, rate limiting, caching, solver client
│   ├── solver/                     # ChallengeSolver trait + detection + FlareSolverr/render back-ends
│   ├── contracts/                  # message/event schemas shared over NATS (serde)
│   ├── auth/                       # password hashing, JWT, RBAC guards
│   ├── config/                     # layered config loading (env + file), typed
│   ├── observability/             # tracing, OTel, metrics init helpers
│   └── matcher/                    # canonicalisation / fuzzy series matching
├── services/
│   ├── api/                        # Axum HTTP edge
│   ├── control-plane/              # scheduler + planner + orchestrator
│   ├── worker/                     # scan worker
│   ├── notifier/                   # notification fan-out
│   ├── sync/                       # AniList sync
│   ├── challenge-solver/           # bot-management bypass microservice (FlareSolverr-backed, pluggable)
│   └── render/                     # optional headless render microservice
├── web/
│   └── frontend/                   # Dioxus app + Tailwind + assets
├── migrations/                     # sqlx migration SQL (versioned)
├── deploy/                         # Dockerfiles, Helm chart, k8s manifests
└── xtask/                          # dev tasks: db reset, seed, codegen
```

**Workspace lints (enforce in CI):** `#![deny(warnings)]` in CI profile, `clippy::pedantic` selectively,
`unsafe_code = "forbid"` in every crate except where a justified `#[allow]` with comment exists,
`cargo deny` (licenses + advisories), `cargo audit`, `rustfmt` check.

---

## 4. Technology decisions (locked)

| Concern | Choice | Rationale |
|---|---|---|
| Async runtime | **Tokio** (multi-thread) | Ecosystem default; required by chosen libs. |
| HTTP server | **Axum** + tower/tower-http | Composable middleware, first-class extractors, ecosystem fit. |
| DB access | **SQLx** (Postgres, `runtime-tokio`, `tls-rustls`) | Compile-time-checked SQL, no heavy ORM, full control over queries and indexes. |
| Migrations | **sqlx-cli** migrations (SQL files) | Deterministic, reviewable, no macro magic. |
| DB | **PostgreSQL 19** | Rich indexing (GIN, trigram, FTS), `ON CONFLICT` upserts, `SKIP LOCKED`. |
| Cache / locks / rate state | **Redis 7** (`fred` client) | Hot read cache, distributed rate-limit counters, advisory locks. |
| Message bus | **NATS JetStream** (`async-nats`) | Durable streams, consumer groups, backpressure. |
| HTTP client (crawl) | **wreq** (BoringSSL, browser TLS/HTTP2 emulation) + `governor` (rate limit) | Providers are WAF-fronted; a rustls handshake is fingerprintable regardless of headers. `wreq-util` supplies the matching profiles. Internal service-to-service HTTP stays on **reqwest** (rustls). |
| HTML parsing | **scraper** (`html5ever`) + `selectors` | CSS-selector driven; pairs with config-driven adapters. |
| Headless render (optional) | **chromiumoxide** in the `render` service | For JS-rendered listing pages only; isolated service. |
| Challenge detection | Cheap classifier in `fetch` (status/headers/body markers) | Recognises Cloudflare/JS/Turnstile interstitials in ms, before a solve is attempted. |
| Challenge solving (bypass) | **FlareSolverr** (default) behind a `ChallengeSolver` trait in the `challenge-solver` service | Modular, extensible bypass tier; back-end swappable (FlareSolverr, headless fallback, or custom) without touching the crawl pipeline. |
| Frontend | **Dioxus 0.6+** (fullstack/WASM) + `dioxus-router` | Rust end-to-end, component model, signals for state. |
| Styling | **TailwindCSS** (CLI build step) | Utility-first, tokenised design system. |
| Auth | Argon2id (`argon2`) + JWT (`jsonwebtoken`) access + rotating refresh | Strong hashing, stateless access, revocable refresh. |
| Validation | `validator` + typed newtypes | Reject malformed input at the edge. |
| Serialization | `serde` / `serde_json` | Standard. |
| Errors | `thiserror` (libs), `anyhow` (bins), typed API error enum | Clear separation; no `unwrap` in service paths. |
| Observability | `tracing` + `tracing-opentelemetry` + `metrics`/Prometheus | Structured logs, traces, metrics. |
| Time | `time` or `chrono` (`time` preferred) | tz-aware timestamps. |
| IDs | UUID v7 (`uuid` w/ `v7`) | Time-sortable primary keys, index-friendly. |

---

## 5. Domain model & the "links, not content" principle

Core aggregate boundaries:

- **Provider** — a source site (e.g. `kunmanga`, `demonicscans`, `manhuaus`). Owns a `base_url`, an
  adapter kind, and an adapter config. The single place a domain is defined.
- **Series** — the canonical work. Provider-independent. Carries merged metadata and a search vector.
- **SeriesSource** — the join between a `Series` and a `Provider`: "this canonical series exists at
  this provider under this path." Holds the provider-specific `source_path` and a `content_hash` used
  for cheap change detection.
- **Chapter** — belongs to a `SeriesSource`. Holds `number`, optional `title`, and a **`path`** — the
  relative link to the chapter page. Never image data.

**The migration-safe link rule.** Every persisted location is stored as a **relative path** on the
provider (`/manga/solo-leveling/chapter-1/`), *not* an absolute URL. The absolute URL is computed at
read time: `resolve(provider.base_url, path)`. Consequences:

- Domain migration = `UPDATE providers SET base_url = $new WHERE id = $id;` — one row, zero data
  rewrite, every link now resolves correctly.
- The DB never stores stale absolute URLs.
- The frontend and API always ask the domain layer to resolve a link; there is exactly one resolver
  function (`domain::resolve_link`) and it is unit-tested for trailing-slash and scheme edge cases.

**No content, ever.** The chapter entity stores a link and metadata. There is no column, blob store,
or code path that fetches or persists page images. This is a hard invariant enforced by review and by
the absence of any image-download capability in the `fetch` crate's public API.

---

## 6. PostgreSQL schema (optimised)

Design principles: UUIDv7 PKs (sortable, good for B-tree locality), `timestamptz` everywhere,
generated `tsvector` for search, GIN indexes for search + trigram fuzzy + tag arrays, idempotent
upserts keyed on natural uniqueness, `content_hash` to short-circuit unchanged work.

```sql
-- Enums
CREATE TYPE content_type   AS ENUM ('manga','manhwa','manhua','webtoon','unknown');
CREATE TYPE series_status  AS ENUM ('ongoing','completed','hiatus','cancelled','unknown');
CREATE TYPE adapter_kind   AS ENUM ('madara','generic_config','custom');
CREATE TYPE provider_state AS ENUM ('active','degraded','challenged','solving','blocked','disabled');
CREATE TYPE scan_mode      AS ENUM ('full','fast');
CREATE TYPE run_state      AS ENUM ('queued','running','completed','failed','cancelled');
CREATE TYPE task_state     AS ENUM ('queued','claimed','running','done','failed','skipped');
CREATE TYPE watch_status   AS ENUM ('reading','planned','completed','dropped','paused');
CREATE TYPE user_role      AS ENUM ('user','operator','admin');

-- Providers: the single source of truth for a site's domain + parsing config.
CREATE TABLE providers (
  id           uuid PRIMARY KEY DEFAULT uuidv7(),
  slug         text NOT NULL UNIQUE,
  name         text NOT NULL,
  base_url     text NOT NULL,               -- change here on domain migration
  adapter      adapter_kind NOT NULL,
  config       jsonb NOT NULL DEFAULT '{}', -- selectors, pagination, latest-feed path
  state        provider_state NOT NULL DEFAULT 'active',
  politeness   jsonb NOT NULL DEFAULT '{}', -- rps, concurrency, crawl_delay, user_agent, emulation
  last_full_scan_at timestamptz,
  created_at   timestamptz NOT NULL DEFAULT now(),
  updated_at   timestamptz NOT NULL DEFAULT now()
);

-- Canonical works.
CREATE TABLE series (
  id            uuid PRIMARY KEY DEFAULT uuidv7(),
  canonical_title text NOT NULL,
  normalized_title text NOT NULL,           -- lowercased, punctuation-stripped (matching key)
  description   text,
  cover_url     text,                        -- link only (may point at a provider's cover)
  content_type  content_type NOT NULL DEFAULT 'unknown',
  status        series_status NOT NULL DEFAULT 'unknown',
  release_year  int,
  search_vec    tsvector GENERATED ALWAYS AS (
                   to_tsvector('simple', coalesce(canonical_title,'') || ' ' ||
                                         coalesce(description,''))
                 ) STORED,
  created_at    timestamptz NOT NULL DEFAULT now(),
  updated_at    timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX series_search_gin  ON series USING gin (search_vec);
CREATE INDEX series_title_trgm  ON series USING gin (normalized_title gin_trgm_ops);
CREATE INDEX series_status_idx  ON series (status);

CREATE TABLE series_titles (       -- alternative titles aid cross-provider matching
  series_id uuid NOT NULL REFERENCES series(id) ON DELETE CASCADE,
  title     text NOT NULL,
  normalized text NOT NULL,
  PRIMARY KEY (series_id, normalized)
);
CREATE INDEX series_titles_trgm ON series_titles USING gin (normalized gin_trgm_ops);

CREATE TABLE tags (
  id   uuid PRIMARY KEY DEFAULT uuidv7(),
  slug text NOT NULL UNIQUE,
  name text NOT NULL
);
CREATE TABLE series_tags (
  series_id uuid NOT NULL REFERENCES series(id) ON DELETE CASCADE,
  tag_id    uuid NOT NULL REFERENCES tags(id)   ON DELETE CASCADE,
  PRIMARY KEY (series_id, tag_id)
);

-- The join: one canonical series can exist on many providers.
CREATE TABLE series_sources (
  id              uuid PRIMARY KEY DEFAULT uuidv7(),
  series_id       uuid NOT NULL REFERENCES series(id) ON DELETE CASCADE,
  provider_id     uuid NOT NULL REFERENCES providers(id) ON DELETE CASCADE,
  source_path     text NOT NULL,             -- RELATIVE path; resolve against base_url
  provider_title  text,                       -- as seen on the provider
  content_hash    bytea,                      -- hash of last-seen metadata+chapter list
  chapter_count   int NOT NULL DEFAULT 0,
  last_scanned_at timestamptz,
  state           provider_state NOT NULL DEFAULT 'active',
  UNIQUE (provider_id, source_path)
);
CREATE INDEX series_sources_series_idx ON series_sources (series_id);

CREATE TABLE chapters (
  id               uuid PRIMARY KEY DEFAULT uuidv7(),
  series_source_id uuid NOT NULL REFERENCES series_sources(id) ON DELETE CASCADE,
  number           numeric(10,4) NOT NULL,    -- supports 10.5, 10.1 etc.
  volume           int,
  title            text,
  path             text NOT NULL,             -- RELATIVE link to the chapter page
  published_at     timestamptz,
  discovered_at    timestamptz NOT NULL DEFAULT now(),
  UNIQUE (series_source_id, number)
);
CREATE INDEX chapters_source_idx  ON chapters (series_source_id, number DESC);
CREATE INDEX chapters_discovered  ON chapters (discovered_at DESC);

-- Users & tracking
CREATE TABLE users (
  id            uuid PRIMARY KEY DEFAULT uuidv7(),
  email         citext NOT NULL UNIQUE,
  username      citext NOT NULL UNIQUE,
  password_hash text NOT NULL,                -- argon2id
  role          user_role NOT NULL DEFAULT 'user',
  created_at    timestamptz NOT NULL DEFAULT now()
);
CREATE TABLE refresh_tokens (
  id         uuid PRIMARY KEY DEFAULT uuidv7(),
  user_id    uuid NOT NULL REFERENCES users(id) ON DELETE CASCADE,
  token_hash text NOT NULL,                    -- store hash, never the token
  expires_at timestamptz NOT NULL,
  revoked_at timestamptz,
  created_at timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX refresh_user_idx ON refresh_tokens (user_id);

CREATE TABLE watchlist_entries (
  user_id   uuid NOT NULL REFERENCES users(id)   ON DELETE CASCADE,
  series_id uuid NOT NULL REFERENCES series(id)  ON DELETE CASCADE,
  status    watch_status NOT NULL DEFAULT 'reading',
  notify    boolean NOT NULL DEFAULT true,
  added_at  timestamptz NOT NULL DEFAULT now(),
  PRIMARY KEY (user_id, series_id)
);
CREATE INDEX watchlist_series_notify ON watchlist_entries (series_id) WHERE notify;

-- Superseded by a two-scalar split (last_read_whole_number / last_read_part_number, still
-- scalar, no ledger) — see docs/READING_PROGRESS_AND_SYNC.md Part A. Kept here as the
-- as-built v1 shape (single last_read_number column).
CREATE TABLE read_progress (
  user_id           uuid NOT NULL REFERENCES users(id)  ON DELETE CASCADE,
  series_id         uuid NOT NULL REFERENCES series(id) ON DELETE CASCADE,
  last_read_number  numeric(10,4) NOT NULL,
  updated_at        timestamptz NOT NULL DEFAULT now(),
  PRIMARY KEY (user_id, series_id)
);

CREATE TABLE notifications (
  id         uuid PRIMARY KEY DEFAULT uuidv7(),
  user_id    uuid NOT NULL REFERENCES users(id) ON DELETE CASCADE,
  kind       text NOT NULL,                     -- 'new_chapter', 'series_completed', ...
  payload    jsonb NOT NULL,
  read_at    timestamptz,
  created_at timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX notifications_user_unread ON notifications (user_id, created_at DESC)
  WHERE read_at IS NULL;

-- External sync
CREATE TABLE external_accounts (
  user_id       uuid NOT NULL REFERENCES users(id) ON DELETE CASCADE,
  provider      text NOT NULL,                   -- 'anilist'
  access_token  text NOT NULL,                   -- encrypted at rest (see §16)
  refresh_token text,
  expires_at    timestamptz,
  PRIMARY KEY (user_id, provider)
);
CREATE TABLE sync_mappings (
  series_id uuid NOT NULL REFERENCES series(id) ON DELETE CASCADE,
  provider  text NOT NULL,
  external_id text NOT NULL,
  PRIMARY KEY (series_id, provider)
);

-- Scan orchestration (progress + audit; mirrors the JetStream dispatch)
CREATE TABLE scan_runs (
  id          uuid PRIMARY KEY DEFAULT uuidv7(),
  provider_id uuid REFERENCES providers(id) ON DELETE SET NULL,
  mode        scan_mode NOT NULL,
  state       run_state NOT NULL DEFAULT 'queued',
  total_tasks int NOT NULL DEFAULT 0,
  done_tasks  int NOT NULL DEFAULT 0,
  failed_tasks int NOT NULL DEFAULT 0,
  started_at  timestamptz,
  finished_at timestamptz,
  created_at  timestamptz NOT NULL DEFAULT now()
);
CREATE TABLE scan_tasks (
  id          uuid PRIMARY KEY DEFAULT uuidv7(),
  run_id      uuid NOT NULL REFERENCES scan_runs(id) ON DELETE CASCADE,
  kind        text NOT NULL,                    -- 'catalog_page','series','latest_feed'
  target      jsonb NOT NULL,                   -- e.g. {"path":"/manga/x","page":3}
  state       task_state NOT NULL DEFAULT 'queued',
  attempts    smallint NOT NULL DEFAULT 0,
  worker_id   text,
  error       text,
  claimed_at  timestamptz,
  finished_at timestamptz
);
CREATE INDEX scan_tasks_run_state ON scan_tasks (run_id, state);
-- Durable claim path (fallback / audit): SELECT ... FOR UPDATE SKIP LOCKED
CREATE INDEX scan_tasks_queue ON scan_tasks (state) WHERE state = 'queued';
```

Notes:
- `citext` for case-insensitive email/username uniqueness; enable `citext`, `pg_trgm`, and a `uuidv7()`
  function (Postgres 19: provide via `pg_uuidv7` extension, or generate in-app and pass explicitly).
- Partitioning `chapters` is **not** needed at MVP. Scale path: range-partition `chapters` and
  `notifications` by month if row counts exceed ~10^8; documented, not built.
- All writes from workers are idempotent `INSERT ... ON CONFLICT DO UPDATE`, so re-running a task is
  safe.

---

## 7. The provider adapter framework (the core abstraction)

Most target sites (kunmanga, manhuaus, and many others) run the **Madara** WordPress theme; their
markup structure is near-identical, differing only in selectors and paths. demonicscans is a custom
layout. The abstraction must make the common case config-only and the rare case a code plugin.

Two layers:

1. **`SourceAdapter` trait** — the behavioural contract every provider satisfies.
2. **Config-driven adapters** — a `GenericConfigAdapter` / `MadaraAdapter` that reads CSS selectors and
   pagination rules from `providers.config` (JSONB). Adding a Madara-like site = insert one row, no
   deploy. A custom site = a small Rust struct implementing the trait, registered by `adapter` enum.

```rust
// crates/adapters/src/lib.rs
use async_trait::async_trait;

#[async_trait]
pub trait SourceAdapter: Send + Sync {
    /// Enumerate the provider catalogue one page at a time (full scan).
    async fn list_catalog(&self, ctx: &Ctx, page: u32) -> Result<CatalogPage>;

    /// The provider's "latest updates" feed (fast scan) — recently updated series + newest chapter.
    async fn list_latest(&self, ctx: &Ctx) -> Result<Vec<LatestUpdate>>;

    /// Full metadata for one series, given its RELATIVE path.
    async fn fetch_series(&self, ctx: &Ctx, path: &str) -> Result<SeriesMeta>;

    /// The chapter list (numbers + relative links) for one series.
    async fn fetch_chapters(&self, ctx: &Ctx, path: &str) -> Result<Vec<ChapterMeta>>;
}

pub struct CatalogPage { pub items: Vec<CatalogItem>, pub has_next: bool }
pub struct CatalogItem { pub path: String, pub title: String }
pub struct LatestUpdate { pub path: String, pub title: String, pub latest_chapter: f64 }
pub struct SeriesMeta {
    pub title: String,
    pub alt_titles: Vec<String>,
    pub description: Option<String>,
    pub cover_url: Option<String>,
    pub tags: Vec<String>,
    pub status: SeriesStatus,
    pub content_type: ContentType,
}
pub struct ChapterMeta { pub number: f64, pub title: Option<String>, pub path: String,
                         pub published_at: Option<time::OffsetDateTime> }
```

`Ctx` bundles the `provider` row (base_url, politeness) and a `&dyn Fetcher`, so adapters never own
their transport — the transport is injected (testable, swappable).

**Config schema (`providers.config`) for the generic adapter:**

```json
{
  "catalog": { "path": "/manga/?page={page}", "item": "div.page-item-detail",
               "link": "a", "title": "h3 a", "next": "a.nextpostslink" },
  "latest":  { "path": "/", "item": "div.page-item-detail",
               "chapter": "span.chapter a" },
  "series":  { "title": "div.post-title h1", "desc": "div.description-summary",
               "cover": "div.summary_image img@src", "tags": "div.genres-content a",
               "status": "div.post-status .summary-content", "alt": "div.summary-heading" },
  "chapters":{ "container": "li.wp-manga-chapter", "link": "a",
               "number_from": "text", "date": "span.chapter-release-date" }
}
```

The `@attr` suffix selects an attribute; otherwise inner text is taken. A number-extraction helper
parses `"Chapter 10.5"` → `10.5`. **All adapter parsing is fuzz-tested against saved HTML fixtures**
(one fixture folder per provider), so a site markup change is caught by a failing test, not by silent
data loss. Fixtures are checked into `crates/adapters/fixtures/<provider>/`.

**Registering a custom adapter** (demonicscans example): implement `SourceAdapter`, add a variant, and
a factory maps `providers.adapter = 'custom'` + `providers.slug` → the struct. Keep custom code minimal
by reusing the generic HTML helpers.

---

## 8. Scan orchestration: full vs. fast modes

The **control plane** schedules and plans; **workers** execute; the **API** exposes progress.

### Full scan (rare, e.g. weekly, or on demand)
Purpose: rebuild/refresh the entire archive for a provider.
Plan:
1. Control plane creates a `scan_run(mode='full', provider)`.
2. It walks the catalogue (via `list_catalog`) enough to know page count, or streams pages, emitting a
   `catalog_page` task per page.
3. Each `catalog_page` task (worker) yields series paths → emits a `series` task per series.
4. Each `series` task (worker): `fetch_series` + `fetch_chapters`, compute `content_hash`, upsert
   series/source/chapters idempotently, mark task done, increment `scan_runs.done_tasks`.
5. Run completes when `done + failed == total` (with a settle window).

### Fast scan (frequent, e.g. every 3–5 min)
Purpose: cheap detection of *new* chapters only.
Plan:
1. Control plane creates `scan_run(mode='fast', provider)` and emits **one** `latest_feed` task per
   provider.
2. Worker calls `list_latest`, compares each item's newest chapter number against the stored
   `series_sources.chapter_count` / max chapter. Only changed series get a follow-up `series` task.
3. New chapters upserted → emit `chapter.discovered` domain events consumed by the notifier.

### Change detection & politeness
- `content_hash` on `series_sources` lets the worker skip an unchanged series without writing.
- Per-provider **concurrency and rate limits** from `providers.politeness`, enforced by `governor`
  keyed on provider, plus a Redis token bucket shared across worker replicas so the *aggregate* rate
  across the whole pool respects the limit.
- **Challenge handling**: when the fetch stack detects a bot-management challenge the provider moves to
  `challenged`, the fetch is routed through the `challenge-solver` service (`solving`), and on a valid
  solved session it returns to `active` with the solver-issued cookies/UA cached for reuse. Only when
  the solver **repeatedly fails** to bypass the challenge does the provider fall through to `blocked`.
- **Circuit breaker**: consecutive hard failures (non-challenge) on a provider flip `providers.state`
  to `degraded` then `blocked`, pausing its tasks and surfacing in the console.
- Retries: exponential backoff with jitter; `attempts` capped (e.g. 4), then task → `failed`.

### Scheduling
`control-plane` runs a scheduler (`tokio-cron-scheduler`) with per-provider cron for fast/full; also
accepts on-demand triggers from the API (operator "Scan now" button). A distributed lock (Redis)
guarantees a single active run of a given `(provider, mode)`.

---

## 9. The fetch layer & crawl posture

The `fetch` crate is the only place network egress to providers happens. It is a composition of
decorators over **`wreq`** — a BoringSSL-backed client that reproduces a real browser's TLS
`ClientHello` and HTTP/2 SETTINGS fingerprint. A generic rustls client is identifiable to
Cloudflare/DDoS-Guard no matter which headers it sends, and a browser `User-Agent` over a
non-browser handshake is a *stronger* bot signal than no disguise at all, so the handshake and the
header set are selected together as one emulation profile (`Politeness::emulation`).

```rust
#[async_trait]
pub trait Fetcher: Send + Sync {
    async fn get(&self, req: FetchRequest) -> Result<FetchResponse, FetchError>;
}
// Composed stack (outer → inner):
//   BackoffFetcher     -> honours provider-directed 429/503 + Retry-After
//   RateLimitedFetcher -> per-provider governor + shared Redis token bucket; narrows its own
//                         spacing when the provider answers 429/503, recovering on quiet
//   CachedFetcher      -> ETag/Last-Modified conditional GETs, short-TTL body cache in Redis
//   SolvingFetcher     -> detects bot-management challenges; on a hit, delegates to the
//                         challenge-solver service and replays the request with the solved session
//   RetryingFetcher    -> exponential backoff + jitter on transient errors
//   BaseHttpFetcher    -> wreq with a browser emulation profile, SSRF-validating DNS, timeouts
```

**Challenge detection & solving (the modular bypass tier):**

```rust
#[async_trait]
pub trait ChallengeSolver: Send + Sync {
    /// Solve a bot-management challenge for `url`, returning a reusable session
    /// (cookies + user-agent) and the solved HTML if the solver fetched it.
    async fn solve(&self, req: SolveRequest) -> Result<SolveOutcome, SolveError>;
}

/// Cheap, allocation-light classifier run on every response by SolvingFetcher.
pub fn detect_challenge(resp: &FetchResponse) -> Option<ChallengeKind> {
    // Fast checks, cheapest first — status, then headers, then a bounded body scan:
    //   * HTTP 403/429/503 with `server: cloudflare` + `cf-mitigated: challenge`
    //   * `Set-Cookie: cf_clearance` / `__cf_bm` absence + interstitial markers
    //   * body markers: "Just a moment...", "cf-challenge", Turnstile widget, `/cdn-cgi/challenge`
    //   * generic JS-interstitial heuristics for non-Cloudflare WAFs
    // Returns None on a normal page so the hot path stays a single branch.
}

pub enum ChallengeKind { CloudflareJs, CloudflareManaged, Turnstile, GenericJsInterstitial }
```

- **Detection is fast and in-band.** `detect_challenge` runs on every `FetchResponse`; on a normal
  page it is a couple of cheap comparisons, so the happy path pays almost nothing. Only a positive
  hit triggers a solve.
- **Solving is delegated, not hand-rolled.** `SolvingFetcher` calls the `challenge-solver` service
  over HTTP. The default `ChallengeSolver` implementation is a **FlareSolverr** client
  (`POST /v1/ (request.get)` with the target URL); the trait keeps the mechanism swappable for a
  headless-render solver or a custom back-end without changing the crawl pipeline or adapters.
- **Sessions are cached and reused.** A successful solve yields `cf_clearance`/UA that are stored in
  Redis keyed by provider and replayed on subsequent requests, so one solve amortises across many
  fetches until it expires; expiry re-triggers a solve, not a block.
- **Bounded and observed.** Solves are per-provider rate-limited (they are expensive), capped in
  concurrency, time-limited, and counted in metrics; repeated solve failures are what finally move a
  provider to `blocked`.

**Crawl posture (explicit and enforced by the code's shape):**
- **Rate-limited and backed off** by default; conservative defaults, operator-tunable *downward* in
  politeness but bounded by hard ceilings in config. This — not a `robots.txt` gate — is what bounds
  the load a provider actually sees, and it is the part the code enforces.
- **Conditional requests** (ETag/Last-Modified) to minimise load and bandwidth.
- **Presents a browser identity by default** (`Politeness::emulation`, Chrome unless changed): the
  TLS/HTTP2 fingerprint and the `User-Agent` come from one profile so they cannot contradict each
  other. Setting `emulation` to `null` reverts a provider to the identifiable `TankoVaultBot`
  user-agent with no fingerprint spoofing. When a provider is behind a challenge, the solver-issued
  session User-Agent overrides the profile's, because the `cf_clearance` cookie is bound to it.
- **No `robots.txt` enforcement.** It was removed with the move to emulation rather than left as
  dead code: a gate keyed on a user-agent the crawler no longer sends can only ever match the `*`
  group, which would misrepresent what the provider agreed to. Politeness is the budget above.
- **No image/content fetching path exists** in the public API — the `FetchResponse` body is treated as
  HTML/JSON for parsing only.

**On JS-rendered pages:** some providers render listings client-side. The optional `render` service
runs a real headless browser (`chromiumoxide`) to obtain the rendered DOM for such pages. This is a
normal rendering step, isolated in its own service and rate-limited like any other fetch, and it also
serves as an alternate `ChallengeSolver` back-end when FlareSolverr is unavailable.

**The `challenge-solver` service.** The bypass tier is its own microservice so the browser/solver
runtime is isolated from the workers and scaled independently:
- It exposes a small HTTP contract (`POST /v1/solve { url, provider, kind }` → `{ cookies, user_agent,
  html?, ttl }`) consumed by `SolvingFetcher`.
- The default back-end is **FlareSolverr** (run as a sidecar/companion container); the service selects
  a back-end via config, and the `ChallengeSolver` trait makes adding a new one (headless render, a
  commercial solver, a self-hosted alternative) a matter of implementing one method and registering it
  — **modular and extensible by construction**.
- It is stateless per request but writes solved sessions to Redis so every worker replica benefits
  from one solve.
- It enforces its own per-provider concurrency/rate limits and timeouts, and emits metrics
  (`solve_attempts`, `solve_success_ratio`, `solve_latency`) for the console.

**Posture:** the system prefers an official API where one exists, crawls politely by default, detects a
bot-management challenge quickly, and **bypasses it through the pluggable solver** rather than giving
up. `blocked` is now the *last resort* — reached only when the solver cannot obtain a valid session
after its retry budget — and is still surfaced as a legible provider health state in the console.
Operators remain responsible for the legality of crawling any given source under its terms.

> Implementer note: keep all challenge logic behind the `ChallengeSolver` trait and inside the
> `challenge-solver` service. Adapters and the rest of the fetch stack must stay solver-agnostic, so a
> new bypass back-end can be dropped in without touching them.

---

## 10. Series canonicalisation (matching one manga across providers)

The hard problem: "Solo Leveling" on three sites must map to one `series` row.

Pipeline (in `crates/matcher`), run when a worker upserts a `series_source`:
1. **Normalise** the title: lowercase, strip punctuation/diacritics, collapse whitespace, drop common
   noise ("manga", "webtoon", scanlation suffixes).
2. **Candidate lookup**: trigram similarity (`pg_trgm`, `%` operator) against `series.normalized_title`
   and `series_titles.normalized`, above a threshold.
3. **Score** candidates on normalised-title similarity + alt-title overlap + release year proximity +
   content-type agreement.
4. **Decision:**
   - High confidence (≥ threshold): attach the new source to the existing series.
   - Low/no confidence: create a new canonical series.
   - **Ambiguous band**: create the source but flag a `merge_candidate` for **operator review** in the
     console (a "Possible duplicates" queue with a one-click merge/split).
5. **Merge operation** (operator-driven) is a transactional re-parent of `series_sources` + title/tag
   union, with an audit record. Splits are the inverse.

This keeps automation aggressive where it's safe and human-in-the-loop where it isn't, which is the
correct trade-off for messy real-world titles.

---

## 11. API service (contract)

Axum. REST + JSON. SSE for live scan progress. Tower middleware: tracing, CORS, compression, request
timeout, rate limit, auth.

**Public / user:**
```
POST   /v1/auth/register
POST   /v1/auth/login                  -> access (JWT, ~15m) + refresh (httpOnly cookie)
POST   /v1/auth/refresh
POST   /v1/auth/logout

GET    /v1/series?query=&tag=&status=&content_type=&provider=&page=&sort=
GET    /v1/series/:id                   -> canonical meta + sources + resolved links
GET    /v1/series/:id/chapters?source=  -> chapters (resolved links) for a source or merged
GET    /v1/tags

GET    /v1/me/watchlist
PUT    /v1/me/watchlist/:series_id       { status, notify }
DELETE /v1/me/watchlist/:series_id
PUT    /v1/me/progress/:series_id        { last_read_number }
GET    /v1/me/feed                        -> new chapters across watchlist (the "reading" dashboard)
GET    /v1/me/notifications
POST   /v1/me/notifications/read          { ids }

GET    /v1/me/sync/anilist/authorize
GET    /v1/me/sync/anilist/callback
POST   /v1/me/sync/anilist/push
POST   /v1/me/sync/anilist/pull
```

**Operator / admin (RBAC-gated):**
```
GET    /v1/admin/providers
POST   /v1/admin/providers               { slug,name,base_url,adapter,config,politeness }
PATCH  /v1/admin/providers/:id            (incl. base_url change = domain migration)
POST   /v1/admin/providers/:id/test       -> dry-run adapter against live/fixture, returns parsed sample
POST   /v1/admin/scans                    { provider_id?, mode }  -> triggers a run
GET    /v1/admin/scans/:run_id
GET    /v1/admin/scans/stream             -> SSE: run/task progress events (console live view)
GET    /v1/admin/merge-candidates
POST   /v1/admin/series/merge             { keep, merge }
```

- **Read models**: series list/detail served from denormalised query joins; hot lists cached in Redis
  with short TTL and event-based invalidation on chapter discovery.
- **Link resolution** happens in the API using `domain::resolve_link(provider.base_url, path)`; clients
  receive ready-to-open absolute URLs but the DB stays relative.
- **Error model**: single typed `ApiError` → RFC 9457 problem+json. No leaking internal errors.
- **Pagination**: keyset (cursor) pagination on UUIDv7/`discovered_at` for large lists.

---

## 12. Control plane

- **Scheduler**: per-provider cron for `fast` and `full`; on-demand triggers via internal endpoint
  called by the API.
- **Planner**: expands a run into tasks, writes `scan_tasks`, publishes to the provider's JetStream
  subject (`scan.tasks.<provider_slug>.<scan_mode>`).
- **Progress aggregator**: consumes task lifecycle events, updates `scan_runs` counters, republishes a
  compact progress event on `scan.progress` (API relays to the console via SSE).
- **Health manager**: watches failure/block ratios, drives the provider circuit breaker and
  `providers.state`.
- **Distributed locks** (Redis) to keep exactly one active run per `(provider, mode)`.
- Stateless and horizontally scalable except for the singleton scheduler, which uses a leader-election
  lock so only one replica schedules.

---

## 13. Worker service

- Subscribes with **one durable consumer per provider per scan mode** — a *lane*,
  `tankovault-workers-<mode>-<slug>` filtered on `scan.tasks.<slug>.<mode>`. Horizontal scaling is
  unchanged (replicas share every lane); what changes is which task a worker takes next. A single
  wildcard consumer served the stream in publish order, so a full catalogue scan (one `series` task
  per catalogue entry, hundreds of thousands for a large site) starved everything else until it
  drained. Two rules replace that:
  1. **Fast before full.** Every fast lane is offered a turn before any full lane is looked at, so a
     chapter release is never queued behind a catalogue walk. Strict priority is safe because the fast
     tier is bounded by construction — a fast run enqueues one `latest_feed` task per provider and
     processes the feed inline — so it cannot starve the full tier. If a fast scan ever fans out, this
     needs to become a weighted split.
  2. **Round-robin between providers**, within a mode, one task per lane per turn. A provider's share
     is set by how many providers have work, not by how many tasks it enqueued.
  - The lane set is refreshed from the provider table on an interval (`worker.provider_refresh_secs`),
    unioned with the consumers already on the stream so a renamed or deleted provider's queued tasks
    are still drained rather than stranded.
  - Lanes are polled with `no_wait` pulls; a round in which every lane is empty backs off (200 ms →
    5 s) so an idle pool does not busy-poll the broker.
  - `scan_tasks_served_total{provider,scan}` counts tasks handed out per lane, which is how fairness
    and priority are observed rather than assumed.
  - Upgrade notes. The pre-fairness `tankovault-workers` wildcard consumer is deleted on worker start:
    a work-queue stream refuses overlapping consumer filters, so the lanes cannot exist beside it.
    Nothing is lost — work-queue retention drops a message on *ack*, not on consumer deletion. The
    tasks stream's subject binding widens from `scan.tasks.*` to `scan.tasks.>` (applied via
    `create_or_update_stream`, since `get_or_create_stream` leaves an existing stream untouched), and
    each full-scan lane also binds the untiered `scan.tasks.<slug>` subject so tasks published before
    the split are executed rather than stranded.
- For each task: claim (`scan_tasks` → `claimed`), run the adapter via the injected fetch stack, upsert
  results idempotently, emit `chapter.discovered` events, mark done/failed, publish progress.
- **Robust environment handling** = resilience, not evasion: timeouts, retries with backoff, circuit
  breaking, honouring provider politeness, graceful handling of malformed markup (fixture-tested
  parsers fail a single series, never the whole run), and clean surfacing of blocks as health state.
- Optional call-out to the `render` service for JS-rendered pages.
- Memory-bounded: streams catalogue pages; never loads a full provider into memory.
- Graceful shutdown: finish in-flight task or requeue it (at-least-once + idempotent upserts make this
  safe).

---

## 14. Notification service

- Consumes `chapter.discovered` events.
- For each event, finds users watching that `series_id` with `notify = true` (indexed partial index),
  respecting each user's read progress (don't notify for chapters below their progress on a rescan).
- Writes `notifications` rows and pushes to connected clients (WS/SSE via the API) and optional
  channels (email/webhook/Discord) behind a `NotificationChannel` trait so channels are pluggable.
- Deduplicates per (user, series, chapter) to avoid double-fire across overlapping providers.

---

## 15. External sync service (AniList)

- OAuth2 with AniList; tokens stored **encrypted at rest** (§16).
- AniList GraphQL client (`reqwest` + typed queries).
- **Pull**: import a user's AniList list → map to canonical series via title matching (reuse
  `matcher`), create watchlist entries.
- **Push**: reflect local watch status/progress to AniList (`SaveMediaListEntry`).
- Mapping cache in `sync_mappings`. Rate-limited to AniList's published limits; backoff on 429.
- Conflict policy is explicit and user-selectable (local-wins / remote-wins / newest-wins).

> **Superseded by `docs/READING_PROGRESS_AND_SYNC.md` Part B**: persisted (not env-config)
> per-account `auto_sync_enabled` + `conflict_policy`, a three-way merge (not two-way) with an
> `ask_me` policy and a conflict-review queue, a scheduled reconciliation loop in addition to
> the reactive targeted push, and a per-series (opt-out) sync-exclusion flag. As-built v1
> (env-only policy, two-way `reconcile_progress`, reactive push only) is described in
> `IMPLEMENTATION_STATUS.md` §7.

---

## 16. Authentication, authorisation & security

**AuthN.** Argon2id password hashing (tuned params) with a **server-side pepper** — a secret mixed
into every hash as its keyed input, held in configuration rather than the database so a database
leak alone cannot be brute-forced offline. Short-lived JWT access tokens carry **identity only,
never privileges**; rotating refresh tokens are stored **as hashes** in `refresh_tokens` and
delivered as `httpOnly`, `Secure`, `SameSite=Strict` cookies scoped to `/v1/auth`. Refresh rotation
with reuse-detection: a reused (already-rotated) refresh token revokes the whole family and emits an
audited `token_reuse_detected` event.

**AuthZ — per-capability, resolved per request.** The earlier ordered role tier
(`user < operator < admin`) has been **removed**. Authorization is now a set of fine-grained,
**non-implying** capabilities (`tankovault_domain::Permission`, e.g. `providers.delete`,
`users.permissions`, `audit.read`): holding one capability never implies another, so a grant cannot
silently widen. Each privileged handler asks for the exact capability it exercises
(`user.require(Permission::X)` / `require_all(&[…])`), and the caller's capability set is **read
fresh from `user_permissions` on every request** rather than embedded in the access token — so a
revocation takes effect immediately instead of outliving the token's lifetime. A refusal is recorded
as an `authz.denied` audit event naming *every* missing capability. The extractor rejects a
**suspended** account outright, before any capability is consulted (suspension ≠ "no permissions").
Ownership is enforced on all `/me/*` resources — the id alone is never authority over another user's
data. The "Reader / Operator / Administrator" presets are a UI convenience expanded at grant time and
never persisted; they are starting points, not stored roles.

> Supersedes the `user_role` enum and `users.role` column sketched in §12: authorization is stored
> per user in `user_permissions`, not as a role column. Authoritative implementation:
> `services/api/src/state.rs` (the `AuthUser` extractor), `crates/domain/src/permissions.rs`
> (the capability set) and `crates/db/src/repo/permissions.rs` (per-request resolution).

**SSRF — critical for this system.** Workers and the "test adapter" endpoint fetch operator-supplied
URLs. Guard rails in the `fetch` crate:
- Allow only `http`/`https` schemes.
- Resolve host and **reject private, loopback, link-local, and metadata IP ranges** (block
  `169.254.169.254`, RFC1918, `::1`, etc.) — re-checked after DNS resolution and on redirects
  (prevent DNS-rebinding / redirect-to-internal).
- Cap redirects; validate each hop.
- Never place secrets or user data in query strings.

**Startup fail-fast.** Under a production profile (`TANKOVAULT_PROFILE=production`) the API refuses to
boot when `jwt_secret` is missing or shorter than 32 bytes — a broken trust root must never serve a
single request — and warns when the pepper is empty. Development, tests and the integration harness
are unaffected, so short/generated secrets keep working locally.

**Secret & pepper rotation.**
- `jwt_secret`: rotating it invalidates every outstanding **access** token immediately (they fail
  signature verification); **refresh** sessions survive only for their remaining lifetime because
  refresh state lives in the database, not the token. Roll it whenever a leak is suspected and keep
  it at ≥ 32 bytes of CSPRNG output.
- `password_pepper`: it is a keyed input to every argon2id hash, so **changing it invalidates every
  stored password hash**. Treat it as set-once and stable, held in the secret manager (never the
  database or the repo); a genuine rotation needs a re-hash-on-next-login migration, not a config
  edit.
- AniList / provider tokens: encrypted at rest (envelope encryption, `aes-gcm`); rotate the KMS/data
  key per the secret manager's policy.

**Other.** Strict input validation at the edge (`validator` + newtypes). Rate limiting on the auth
endpoints. CSRF defence for the cookie refresh flow is `SameSite=Strict` plus a cookie path scoped to
`/v1/auth`. `cargo audit` + `cargo deny` in CI. Secrets from env/secret store, never in the repo.
Baseline hardening headers on the web edge (the axum `frontend` server; see
`services/frontend/`) — `X-Content-Type-Options: nosniff`, `X-Frame-Options: DENY`,
`Referrer-Policy: no-referrer` on the app shell; the JSON API additionally sets `X-Content-Type-Options: nosniff`,
`X-Frame-Options: DENY`, `Referrer-Policy: no-referrer`, `Cross-Origin-Resource-Policy: same-origin`
and HSTS. Structured audit log for privileged actions (provider edits, merges, scan triggers, grant
changes, suspensions).

---

## 17. Frontend: Dioxus + Tailwind (design system + screens)

The reader-facing experience is the product. It must feel purpose-built for manga readers, not like a
generic dashboard. Dioxus (WASM SPA) + `dioxus-router` + signals for state; Tailwind for styling built
via the Tailwind CLI against a tokenised config.

### 17.1 Design direction — "Inkstone"

A calm, high-contrast reading environment that borrows from print manga and the sumi-e ink tradition
rather than the usual neon-on-black tracker look. Dark-first (readers browse at night), with a warm
paper light mode. The signature is an **ink-brush motif**: section dividers and the active-nav
indicator are a single expressive brush stroke, and cover cards use a subtle paper grain — restrained,
one memorable device, everything else quiet.

**Color tokens** (Tailwind theme extension; dark-first):
```
--ink-900  #0E1116   app background (near-black, faintly blue, not pure #000)
--ink-800  #161B22   raised surfaces / cards
--ink-700  #232A33   borders / hairlines
--paper    #EDE6D6   light-mode background (warm paper)
--sumi     #F2EFE9   primary text on dark
--vermilion #E4572E  the single accent (hanko-seal red) — used sparingly: unread, CTAs, active nav
--jade     #2E8B78   success / "up to date" states
--muted    #8A94A0   secondary text
```
Deliberately **not** the default AI palette (no cream+terracotta, no acid-green-on-black). Vermilion is
the one bold move — used only for unread counts, the primary CTA, and the active-nav brush stroke.

**Typography (2 roles + data):**
- Display: **Bricolage Grotesque** (variable, 400–800) for the wordmark and screen titles, used
  with restraint. (Supersedes the earlier *Zodiak / Clash Display* placeholder; the TankoVault
  redesign standardises on Bricolage Grotesque — see `docs/frontend/DESIGN_SPEC.md` §3.)
- Body/UI: a clean humanist sans (*IBM Plex Sans*).
- Data/labels: a mono (*IBM Plex Mono*) for chapter numbers and counts — makes numeric scanning fast.
- Type scale: 12 / 14 / 16 / 20 / 28 / 40, generous line-height on descriptions.

**Layout:** persistent left rail (Discover, Reading, Watchlist, Notifications, + Console for
operators), a top command bar with instant search, content area on a 12-col grid. Cover grid is a
responsive masonry of 2:3 cards.

### 17.2 Key screens

1. **Discover** — masonry cover grid with filter chips (tags, status, content type, provider) and a
   sort control (recently updated / A–Z / most sources). Virtualised grid, lazy-loaded covers with
   blur-up placeholders. Empty state is an invitation, not an apology ("Nothing here yet — add a
   provider in the console to start indexing.").
2. **Series detail** — hero with cover + title + alt titles + tags; a **Sources** tab strip ("Read on:
   Provider A · B · C") where each opens the resolved link in a new tab; a chapter list (mono numbers,
   published dates, unread markers) with "mark read to here"; watchlist toggle + notify toggle;
   progress bar.
3. **Reading dashboard** — the reader's home. "Continue reading" row (last-read + next-unread jump),
   "New chapters" feed of unread across the watchlist with vermilion unread badges, grouped by day.
4. **Watchlist** — status columns (Reading / Planned / Completed / Paused / Dropped) with drag between
   columns; per-title notify toggle; AniList sync status pill.
5. **Notifications** — chronological, unread emphasised, one-click mark-all-read, deep-links to the
   series.
6. **Search** — instant, fuzzy (trigram-backed), grouped results (Series / Tags), keyboard-navigable.
7. **Operator Console** (RBAC):
   - **Scan dashboard**: live per-run progress bars (SSE), throughput, ETA, failed-task drill-down,
     provider health tiles (active/degraded/challenged/solving/blocked with reason) plus per-provider
     solve success-ratio and last-solve age.
   - **Challenge & solver panel**: which providers are currently challenged, the active solver back-end
     (FlareSolverr/render/custom), solve latency/success charts, and a **"Re-solve now"** action that
     forces a fresh session for a provider.
   - **Providers**: table with `base_url` inline-editable (the **domain-migration** action — a single
     field, with a confirm dialog explaining every link will re-resolve).
   - **Adapter editor**: JSON/selector editor for `providers.config` with a **"Test adapter"** button
     that dry-runs against the live site or a saved fixture and shows the parsed sample side-by-side —
     so operators can fix selectors without a deploy.
   - **Merge queue**: the canonicalisation review list with side-by-side compare and merge/split.

### 17.3 Quality floor (non-negotiable)
- Responsive to mobile; the cover grid reflows to 2-up.
- Visible keyboard focus rings; full keyboard navigation of lists and the command bar.
- `prefers-reduced-motion` respected (the brush-stroke animation degrades to a static stroke).
- Loading = skeletons (never spinners on content); error states name what failed and how to retry;
  optimistic updates for watchlist/progress toggles with rollback on failure.
- WCAG AA contrast for all text/background pairs (vermilion is used on dark surfaces where it passes).
- Copy is plain and action-named: a button says "Add to watchlist" and the resulting toast says "Added
  to watchlist."

### 17.4 Frontend architecture
- Components in a `components/` module; screens in `views/`; a typed API client crate
  (`crates/api-client`) generated by `xtask openapi` from the API's OpenAPI spec (derived from the
  handlers' `utoipa` schemas via `progenitor`) so the frontend and backend share DTOs.
- Client state via Dioxus signals; server state via a small fetch-cache layer (stale-while-revalidate).
- Auth: access token in memory, refresh via httpOnly cookie; a route guard redirects unauthenticated
  users. The shell runs a background silent-refresh loop that adopts a token from the cookie on boot
  and renews it shortly before expiry, so an active tab never lapses. A failed refresh is **not** an
  automatic sign-out: only a genuine `401` (refresh session expired past its window, rotated away, or
  reuse-revoked) clears the session, while transient failures (offline, timeout, 5xx, server restart,
  waking from sleep) keep the session and retry with exponential backoff — the app stays logged in
  across reloads and brief outages like a normal site.
- Live updates: subscribe to the notification WS/SSE for real-time unread badges and (for operators)
  the scan progress stream.

---

## 18. Observability & operations

- **Tracing**: `tracing` spans across service boundaries; propagate trace context over NATS headers and
  HTTP; export to OTel collector.
- **Metrics** (Prometheus): request latency/error rates (API), tasks/sec, task success ratio, per-
  provider fetch latency and block rate, queue depth, DB pool saturation, notification fan-out latency,
  and challenge/solver metrics (`challenge_detected_total`, `solve_attempts`, `solve_success_ratio`,
  `solve_latency`, `solved_session_reuse_ratio`).
- **Dashboards & alerts**: provider block-rate spike, run failure ratio, queue backlog growth, DB
  connection exhaustion, and a **solve success-ratio drop** or **solver unavailable** alert.
- **Logs**: structured JSON, no PII in logs, correlation IDs.
- **Health/readiness** endpoints on every service for k8s probes.

---

## 19. Deployment

- Each service is a small (`scratch`) container — a musl binary plus its loader on an empty
  base (multi-stage build; `cargo chef` for cached dependency layers). The `render` tier is the
  exception (Debian + Chromium). The frontend builds to static WASM+assets served by a CDN or the API.
- **Kubernetes** via a Helm chart in `deploy/helm/tankovault` (consuming the shared
  `deploy/helm/common` library chart; see its README):
   - `api` and `worker` are `HorizontalPodAutoscaler`-scaled (worker on queue depth, api on CPU/RPS).
   - `challenge-solver` is its own Deployment with a **FlareSolverr** companion container; scaled on
     solve queue depth/latency and given a modest CPU/memory floor (a headless browser is heavy).
   - `control-plane` scheduler uses leader election (single active scheduler).
   - A migrations `Job` runs `sqlx migrate run` on deploy (gated before app rollout).
   - Config via env + mounted secrets; Postgres/Redis/NATS as managed or in-cluster statefulsets;
     solver back-end (endpoint, timeouts, max concurrency) is config-driven so it can be swapped.
- **Environments**: local (docker-compose with Postgres/Redis/NATS + FlareSolverr + seed data),
  staging, prod.
- **CI**: fmt + clippy + `cargo deny` + `cargo audit` + unit/integration tests (including adapter
  fixture tests and a Postgres-backed repo test via `sqlx`'s test harness) + build all images.

---

## 20. Phased delivery roadmap

**Phase 0 — Foundations (workspace, DB, one provider end-to-end).**
Workspace + crates skeleton; migrations; `domain` + `db`; the `fetch` stack with emulation + rate limit +
SSRF guard; the `SourceAdapter` trait + generic/Madara adapter + fixtures for one Madara provider;
`resolve_link` + tests. Deliverable: a `worker` binary that full-scans one provider into Postgres,
links-only, idempotently.

**Phase 1 — Orchestration & API.**
Control plane (scheduler, planner, JetStream), `scan_runs`/`scan_tasks`, progress aggregation; API with
auth (register/login/refresh), series browse/detail/chapters with resolved links, admin provider CRUD +
`base_url` migration + trigger-scan + SSE progress.

**Phase 2 — Frontend MVP.**
Dioxus app: Discover, Series detail, auth, and the operator scan dashboard + provider editor with
"Test adapter". Tailwind design system ("Inkstone" tokens).

**Phase 3 — Users & tracking + challenge bypass.**
Watchlist, read progress, reading dashboard, notifications service + in-app notifications, fast-scan
mode wired to notifications. Ship the `challenge-solver` service: `detect_challenge` + `SolvingFetcher`
in the `fetch` stack, the `ChallengeSolver` trait with the FlareSolverr back-end, Redis session
caching, and the console challenge/solver panel.

**Phase 4 — Canonicalisation & multi-provider.**
`matcher`, merge-candidate queue + operator merge/split UI; onboard demonicscans (custom adapter) and a
second Madara provider; cross-provider "read on A/B/C".

**Phase 5 — External sync & hardening.**
AniList OAuth + push/pull; observability dashboards; autoscaling; load and resilience testing;
security review (SSRF, authz, secret handling).

Each phase ships a demoable increment; no phase depends on a later one.

---

## 21. Definition of done / acceptance criteria

- All services build with `deny(warnings)`, pass clippy (pedantic where configured), `cargo deny`, and
  `cargo audit` clean.
- Every adapter has fixture tests; a markup change breaks a test, not production data.
- `resolve_link` and the domain-migration path have tests proving one `base_url` edit re-resolves all
  links, with no absolute URLs persisted anywhere.
- No code path fetches or stores chapter images/page content (reviewed invariant).
- SSRF guard rejects private/loopback/metadata targets, including after redirects and DNS resolution
  (tested).
- Full scan and fast scan both run for ≥2 providers end-to-end; fast scan produces notifications only
  for genuinely new chapters above user progress.
- A provider behind a bot-management challenge is **detected quickly and bypassed via the pluggable
  solver** (FlareSolverr by default): the challenged fetch is routed through the `challenge-solver`
  service, a solved session is cached and reused, and the crawl proceeds. `blocked` is reached only
  after the solver's retry budget is exhausted, and is surfaced with a clear console reason.
- The solver is modular: swapping the `ChallengeSolver` back-end (via config) requires no change to
  the fetch pipeline, adapters, or workers (covered by a trait-level test with a fake solver).
- Auth: argon2id, rotating hashed refresh tokens with reuse detection, RBAC enforced on admin routes,
  ownership enforced on `/me/*`.
- Frontend meets the quality floor in §17.3 (responsive, keyboard, reduced-motion, AA contrast,
  skeleton/error/empty states, optimistic updates) verified on Discover, Series detail, and the
  console.
- Observability: traces span service hops; the specified metrics are emitted; health/readiness probes
  respond.
- Horizontal scale demonstrated: adding worker replicas increases throughput without duplicate writes
  (idempotency verified).

---

### Appendix A — key invariants for the implementer
1. Store relative paths, resolve at read time. One resolver, tested.
2. Links and metadata only. No content, no images, no mirroring.
3. Polite crawl by default; detect challenges fast and bypass them via the pluggable `ChallengeSolver`
   (FlareSolverr default). `blocked` is the last resort, only after the solver's retries are exhausted.
4. Every worker write is idempotent (`ON CONFLICT`), so at-least-once delivery is safe.
5. Adapters own no transport; the fetch stack is injected and mockable.
6. Config-driven adapters make the common provider a one-row insert; custom code is the exception.
7. Canonicalisation is aggressive where safe, human-reviewed where ambiguous.