# TankoVault / Kanpai — Backend Architecture, Abstraction & Modularization Audit

Scope: `crates/*`, `services/*`, workspace dependency graph. Read-only.
Excluded by assignment: security, performance, testing, frontend.

Baseline: 14 library crates + 8 service binaries + `xtask`; ~36.4k LOC of Rust across
`crates/` and `services/`.

**Overall verdict.** The *infrastructure* layering is unusually good for a project this size:
`crates/domain` is genuinely pure (no I/O, no sqlx unless feature-gated), `crates/fetch` is a
clean decorator stack (`Base → Solving → RateLimited → Backoff → Retrying`), `crates/service`
successfully centralises the cross-cutting runtime, and no service leaks `sqlx` types
(`sqlx` appears zero times under `services/`). The failures are concentrated in three places:
(1) the **persistence↔wire boundary**, which does not exist — `crates/db` row structs *are*
the public HTTP schema; (2) **five god modules** whose growth is driven by copy-paste rather
than complexity; and (3) a **second-class error/proxy layer** in `services/api` and
`services/sync` that routes HTTP status by string matching and collapses upstream failures to
`500`.

---

## 1. `crates/db` repository rows are the public HTTP wire schema

**Severity: Critical**

**Evidence**
- `crates/db/Cargo.toml:22` — the persistence crate depends on `utoipa`.
- 23 `ToSchema` derives on repository row structs, e.g.
  `crates/db/src/repo/sync.rs:613` (`AdminAccountRow`), `crates/db/src/repo/sync.rs:655`
  (`AdminMappingRow`), `crates/db/src/repo/sync.rs:708` (`UnmappedSeriesRow`),
  `crates/db/src/repo/sync.rs:800` (`RemoteEntryRow`), `crates/db/src/repo/stats.rs:13,38`,
  `crates/db/src/repo/scans.rs:354`, `crates/db/src/repo/audit.rs:71`,
  `crates/db/src/repo/user_admin.rs:25,49,138`, `crates/db/src/repo/gdpr.rs:41,87,110,138`,
  `crates/db/src/repo/tracking.rs:825`, `crates/db/src/repo/permissions.rs:75`,
  `crates/db/src/repo/providers.rs:207`, `crates/db/src/repo/matching.rs:102`,
  `crates/db/src/repo/flags.rs:19`.
- 11 handler signatures return a repo row directly:
  `services/api/src/admin/sync.rs:36,60,318,357,396`,
  `services/api/src/admin/system.rs:33,58`, `services/api/src/admin/scans.rs:150`,
  `services/api/src/admin/providers.rs:338`, `services/api/src/me/dashboard.rs:116`,
  `services/api/src/series.rs:471`.
- Schema-type census: `crates/contracts` = 5 `ToSchema` types, `crates/domain` = 19,
  `crates/db` = 23, `services/api` = 75.

**Why it matters**
`crates/contracts` is documented (`crates/contracts/src/lib.rs:1-7`) as "the wire contract
shared between services", but it holds 5 of ~122 schema types. The real single source of
truth is nowhere: it is split three ways, and the largest non-handler share sits in the
*persistence* layer. Concretely, renaming a column in `admin_list_mappings`' SELECT silently
rewrites the public `/v1/admin/sync/mappings` response and the generated
`crates/api-client` — with no compile error anywhere at the API boundary, because the handler
never names a field. That is a class of breaking change that cannot be caught in review of the
API crate. It also inverts the dependency direction: `tankovault-db` now has an opinion about
OpenAPI, and cannot be reused (or compiled) without the presentation-layer dependency.

**Remediation**
1. Delete `utoipa` from `crates/db/Cargo.toml` and strip every `ToSchema` from
   `crates/db/src/repo/*`. Keep `serde::Serialize` only where a row is genuinely serialised
   for an internal purpose (audit detail blobs).
2. Move the 11 leaked shapes into `crates/contracts` as explicit view types in new modules —
   `contracts::admin::{SyncAccountView, SyncMappingView, UnmappedSeriesView, RemoteEntryView,
   FailedTaskView, AuditView, ProviderStatView, SystemStatsView}`,
   `contracts::me::{MeStatsView}`, `contracts::catalogue::{PublicProviderView}`.
3. Add `impl From<repo::X> for contracts::XView` in `services/api` (the only crate that may
   know both), so a column rename becomes a compile error at exactly one call site.
4. Add a CI guard: `cargo tree -p tankovault-db | grep -q utoipa && exit 1`.

**Effort: L** (mechanical but broad; ~11 handlers + 23 structs + regenerated client).

---

## 2. `crates/db/src/repo/catalog.rs` (1408 LOC) — five copy-pasted 50-line SQL statements

**Severity: Critical**

**Evidence**
- `crates/db/src/repo/catalog.rs:934-1220` — `list_series_filtered`, 287 lines, carrying
  `#[allow(clippy::too_many_lines)]` at :933.
- Five near-identical `sqlx::query_as!` blocks at :949 (`title`), :998 (`chapters`),
  :1048 (`sources`), :1097 (`year`), :1147 (default `updated`). `grep -c "count(*) OVER() AS"`
  = 5. Each repeats the *same* 9-predicate `WHERE` clause and 11 binds; only the `ORDER BY`
  differs.
- The comment at :928-932 concedes the design: "each sort order is spelled out as its own
  otherwise-identical `query_as!`".

**Why it matters**
Adding one filter (say `author`) is five identical edits plus five `.sqlx/` cache entries; a
reviewer cannot see that they are identical without diffing ~250 lines. Any of the five
drifting produces a filter that silently applies under four sorts and not the fifth. This is
the single highest-risk duplication in the codebase because the duplicated thing is a
*security-relevant predicate set* (tag exclusion, provider scoping).

**Remediation**
Collapse to one static statement, preserving `query_as!` compile-time checking, by moving the
sort into bound `CASE` expressions:

```sql
ORDER BY
  CASE WHEN $12 = 'title'    THEN s.canonical_title END ASC NULLS LAST,
  CASE WHEN $12 = 'year'     THEN s.release_year    END DESC NULLS LAST,
  CASE WHEN $12 = 'chapters' THEN (SELECT COALESCE(sum(ss.chapter_count),0)::int8 …) END DESC,
  CASE WHEN $12 = 'sources'  THEN (SELECT count(DISTINCT ss.provider_id) …) END DESC,
  s.updated_at DESC
```

with a validated sort token:

```rust
#[derive(Debug, Clone, Copy, Default)]
pub enum SeriesSort { #[default] Updated, Title, Chapters, Sources, Year }
impl SeriesSort { pub fn as_token(self) -> &'static str { … } }
```

Change `SeriesFilter::sort` from `Option<String>` (`catalog.rs:894`) to `SeriesSort`, parsed
in the handler (`services/api/src/series.rs:126`) so an unknown sort is a `400` rather than a
silent fallback. Verify the plan afterwards — the `CASE` form is index-friendly for the
`title`/`year` branches; if `updated` regresses, keep exactly *two* statements (indexed
default + `CASE` for the rest), not five.

**Effort: M**

---

## 3. `crates/db/src/repo/catalog.rs` is four aggregates in one file

**Severity: High**

**Evidence** — the file already documents its own seams with banner comments:
`:17` series, `:396` sources, `:601` chapters, `:778` read models, `:1336` composite ingest.
30 public functions and 14 public structs in one module (`get_symbols_overview`).

**Why it matters**
`catalog.rs` is imported by `services/worker`, `services/sync`, `services/api` and `xtask`.
Every consumer takes the whole module's compile cost and every one of them can reach every
function, so there is no way to say "the worker writes; the API reads".

**Remediation** — split at the existing banners into `crates/db/src/repo/catalog/`:

| module | contents (current lines) |
|---|---|
| `catalog/series.rs` | `SeriesUpsert`, `SeriesRow`, `resolve_canonical_series`, `create_series`, `update_series_meta`, `get_series`, `slugify` (75-190, 292-395) |
| `catalog/enrichment.rs` | `SeriesEnrichmentRow`, `MetadataEnrichment`, `list_series_for_enrichment`, `apply_enrichment`, `add_series_{titles,tags,authors}` (194-395) |
| `catalog/sources.rs` | `SourceRow`, `upsert_source`, `register_source_stub{,s}`, `update_source_scan`, `source_content_hash`, `list_sources_for_series`, `source_provider_base_url` (396-600, 1315) |
| `catalog/chapters.rs` | `ChapterUpsert*`, `ChapterRow`, `upsert_chapter`, `max_chapter_number`, `count_full_chapters*`, `list_chapters*` (601-777) |
| `catalog/browse.rs` | `SeriesListItem`, `SeriesFilter`, `SeriesPage`, `FilteredRow`, `list_series*`, `list_tags` (778-1335) |
| `catalog/ingest.rs` | `ScannedSeries`, `IngestOutcome`, `ingest_series` (1336-1408) |

`catalog/mod.rs` re-exports for source compatibility, then callers migrate incrementally.

**Effort: M**

---

## 4. `crates/db/src/repo/tracking.rs` (983 LOC) — seven unrelated aggregates

**Severity: High**

**Evidence** — one module owns watchlist CRUD (`:19-96`), read progress (`:97-339`),
watch status + sync exclusion (`:340-455`), notifications (`:456-548`), watcher fan-out
(`:549-585`), the activity feed (`:586-656`), notification dedup claims (`:657-682`),
dashboard cards (`:683-825`), stats (`:826-863`) and recommendations (`:864-983`).
Consumers are disjoint: `services/notifier` needs only `watchers_for_series`/
`notification_create`/`dedup_claim`; `services/sync` needs only `progress_*` and
`is_sync_excluded`; `services/api` needs the rest.

**Why it matters**
"Tracking" is not an aggregate — it is a folder name. `recommendations` (a read model over the
catalogue) and `dedup_claim` (a broker-idempotency primitive) have nothing in common beyond
sharing a `user_id` column.

**Remediation** — split into `crates/db/src/repo/tracking/`:
`watchlist.rs` (upsert/remove/list/status/`WatchlistCard`), `progress.rs`
(`ReadProgress`, `progress_*`, `set_sync_{excluded,override}`, `is_sync_excluded`),
`notifications.rs` (`notification_create`, `notifications_*`, `Watcher`,
`watchers_for_series`, `dedup_claim`), `dashboard.rs` (`FeedItem`, `feed`, `ContinueCard`,
`continue_reading`, `MeStats`, `me_stats`, `recommendations`).

**Effort: S**

---

## 5. `crates/db/src/repo/sync.rs` (944 LOC) — three lifecycles, two audiences

**Severity: Medium**

**Evidence** — `:25-292` snapshots/conflicts/history, `:294-612` external accounts + mappings,
`:613-944` the admin console read models (`AdminAccountRow`, `AdminMappingRow`,
`UnmappedSeriesRow`, `RemoteEntryRow`, `SeriesCandidateRow`, `suggest_series_candidates`).
Three `#[allow(clippy::too_many_arguments)]` at :54, :87, :758.

**Why it matters**
The admin read models are consumed only by `services/api/src/admin/sync.rs`; the rest only by
`services/sync`. Two services with disjoint needs compile the same 944-line module. The
`too_many_arguments` suppressions mark exactly the functions that want a parameter struct.

**Remediation** — `repo/sync/{snapshots.rs, conflicts.rs, history.rs, accounts.rs,
mappings.rs, remote_entries.rs, admin_views.rs}`. Replace the three suppressed signatures
with parameter structs (`RecordSnapshot`, `NewConflict`, `RemoteEntryUpsert`) — the
suppression is the smell, not the lint.

**Effort: M**

---

## 6. `services/sync/src/engine.rs` (1267 LOC) — one `impl` block, 22 methods, six responsibilities

**Severity: High**

**Evidence** — `SyncEngine` at `:125`, single `impl` at `:136-1213` containing:
OAuth/link lifecycle (`link` :183, `store_tokens` :380, `access_token` :406, `unlink` :353,
`status` :361), settings/policy (`settings` :209, `update_settings` :235, `effective_policy`
:487), conflict/history read models (`list_conflicts` :260, `history` :268,
`resolve_conflict` :283), reconciliation (`reconcile_account` :505, `reconcile_series`
:625-840 — **216 lines**, with `#[allow(...)]` at :620), targeted push (`push_series` :865,
`push_series_one` :894, `push_series_inner` :933), matching (`resolve_series` :983,
`resolve_media_id` :1041) and metadata enrichment (`enrich_all` :1069, `enrich_series` :1119,
`apply_metadata` :1145).

**Why it matters**
`SyncEngine` holds seven fields serving six unrelated concerns; every method can touch all of
them. Metadata enrichment (`enrich_*`, which needs no user token at all — see
`ExternalProvider::supports_public_metadata`, `services/sync/src/provider.rs:92`) is welded to
token-bearing reconciliation. `reconcile_series` at 216 lines is the only place the three-way
merge is applied, and it is unreachable from a test without a live pool and a provider.

**Remediation** — decompose into collaborators, each owning its slice of state:

```
services/sync/src/
  engine/mod.rs        // SyncEngine facade: holds the four below, no logic
  engine/accounts.rs   // AccountService: link/unlink/status/settings/policy, token sealing
  engine/tokens.rs     // TokenVault { secret, pool }: store_tokens, access_token, refresh
  engine/reconcile.rs  // Reconciler { pool, policy }: reconcile_account/_series/_all
  engine/push.rs       // TargetedPush: push_series{,_one,_inner}, resolve_media_id
  engine/enrich.rs     // Enricher { pool, metadata_priority }: enrich_all/_series/apply_metadata
  engine/resolve.rs    // SeriesResolver { pool, thresholds, candidate_limit }: resolve_series
```

Split `reconcile_series` into `plan_series(local, remote, policy) -> MergeAction` (pure,
delegating to `crate::mapping::three_way`) and `apply_series(action)` (I/O). The pure half
becomes unit-testable, which is the point.

**Effort: L**

---

## 7. `services/sync/src/anilist.rs` (974 LOC) — transport, GraphQL, parsing, pacing and the trait impl in one file

**Severity: Medium**

**Evidence** — client + OAuth (`:99-235`), GraphQL queries (`:235-430`), the
`ExternalProvider` impl (`:438-530`), free-function JSON parsers (`parse_media_list` :531,
`parse_entry` :564, `titles_from_media` :614, `genres_from_media` :638, `staff_from_media`
:653, `parse_media_metadata` :671, `strip_html` :719), a `Pacer` (`:733-761`), a hand-rolled
`urlencode` (`:764`), and ~190 lines of tests.

**Why it matters**
Adding a second provider (the stated design goal — `provider.rs:70` is a generic trait) means
copying this file's structure, because none of the reusable parts are extracted. The parsers
are pure and independently valuable; the pacer and the URL encoder are generic utilities that
happen to live in a provider module.

**Remediation** — `services/sync/src/providers/anilist/{mod.rs, client.rs, graphql.rs,
parse.rs}`; hoist `Pacer` to `services/sync/src/pacing.rs` (or better, see finding 12); delete
`urlencode` and use `url::form_urlencoded` (`url` is already a workspace dependency, just not
declared by `services/sync`). Move `progress_to_int` (`:433`) next to `mapping.rs`, which owns
every other unit conversion.

**Effort: M**

---

## 8. `crates/config/src/lib.rs` (815 LOC) — a flat module holding 15 unrelated config aggregates

**Severity: Medium**

**Evidence** — one file declares `ConfigError` (:21), `load` (:32), `DatabaseConfig` (:42),
`RedisConfig` (:64), `NatsConfig` (:71), `HttpConfig` (:78), `TelemetryConfig` (:100),
`EmailSecurity`/`EmailConfig` (:128,:153), `MetricsConfig` (:241), `FeaturesConfig` (:301),
`AuditConfig` (:336), `RateLimitBackend`/`RateLimitPolicy`/`RateLimitConfig` (:388,:407,:436),
`CorsConfig` (:500), `SecurityConfig` (:527), plus `MetadataPriorityConfig` (:600) and the
domain constants `SOURCE_ANILIST`/`SOURCE_ADAPTER` (:589,:591).

**Why it matters**
Every service depends on `tankovault-config`; every service therefore compiles all 15. Worse,
`MetadataPriorityConfig` + `SOURCE_*` are *domain policy* ("AniList's title beats the
adapter's") living in a crate whose stated job is layered TOML/env loading and which has no
dependency on `tankovault-domain` — so the policy cannot reference domain types and is stringly
typed. `services/sync/src/engine.rs:19` imports `SOURCE_ADAPTER, SOURCE_ANILIST` from `config`.

**Remediation**
1. Split into `crates/config/src/{lib.rs, error.rs, database.rs, redis.rs, nats.rs, http.rs,
   telemetry.rs, email.rs, metrics.rs, features.rs, audit.rs, ratelimit.rs, cors.rs,
   security.rs}` with `pub use` in `lib.rs` (no API break).
2. Move `MetadataPriorityConfig`, `SOURCE_ANILIST`, `SOURCE_ADAPTER` to
   `crates/domain/src/metadata_priority.rs` and replace the `&'static str` source keys with a
   `MetadataSource` enum. `crates/config` should not know what a metadata source is.

**Effort: S**

---

## 9. The internal-service HTTP proxy is open-coded 9+ times, and collapses every upstream failure to `500`

**Severity: High**

**Evidence**
- Helpers already exist — `services/api/src/me/sync.rs:452` (`sync_get`) and `:477`
  (`sync_proxy`) — yet the identical 8-line block is re-inlined at
  `me/sync.rs:34`, `:68`, `:105`, `:458`, `:484`, plus `admin/sync.rs:186`,
  `admin/scans.rs:60`, `admin/providers.rs:301`, `me/progress.rs:268`.
  `grep -c "sync service unreachable" me/sync.rs` = **7**.
- Every one of them does:
  ```rust
  if !resp.status().is_success() { return Err(ApiError::Internal); }
  Ok(Json(resp.json().await.map_err(|_| ApiError::Internal)?))
  ```
  `ApiError::Internal` appears **19 times in `me/sync.rs` alone**.
- `services/api/src/state.rs:41,44,47` — three bare `String` base URLs
  (`control_plane_url`, `sync_url`, `challenge_solver_url`), each `trim_end_matches('/')`-ed
  at every call site.

**Why it matters**
The sync service deliberately returns `404` for an unknown provider and `409` for an unlinked
account (`services/sync/src/main.rs:621-627`) — and `services/api` throws both away, answering
`500 internal server error`. The OpenAPI documents `409 Account not linked`
(`admin/sync.rs:87`) for a response the code cannot produce. The generated client and the
frontend are therefore documented against a contract the edge violates.

**Remediation** — introduce a typed upstream client in `services/api/src/upstream.rs`:

```rust
pub struct Upstream { base: Url, http: reqwest::Client, name: &'static str }

impl Upstream {
    pub async fn get<T: DeserializeOwned>(&self, path: &str) -> ApiResult<T>;
    pub async fn post<B: Serialize, T: DeserializeOwned>(&self, path: &str, body: &B) -> ApiResult<T>;
    pub async fn delete<B: Serialize, T: DeserializeOwned>(&self, path: &str, body: &B) -> ApiResult<T>;
}

fn map_upstream_status(name: &str, status: StatusCode, body: &str) -> ApiError {
    match status {
        StatusCode::NOT_FOUND        => ApiError::NotFound,
        StatusCode::CONFLICT         => ApiError::Conflict(body.to_owned()),
        StatusCode::BAD_REQUEST      => ApiError::BadRequest(body.to_owned()),
        s if s.is_server_error()     => ApiError::Unavailable,
        _                            => ApiError::Internal,
    }
}
```

Replace the three `String` fields in `AppState` with `sync: Upstream`,
`control_plane: Upstream`, `solver: Upstream` (base-URL normalisation happens once, in
`Upstream::new`). Then type the read proxies against `tankovault_contracts::sync::*` instead of
`Json<serde_json::Value>` (see finding 10).

**Effort: M**

---

## 10. Proxy handlers return `Json<serde_json::Value>` while their OpenAPI declares a typed body

**Severity: Medium**

**Evidence** — 20 handlers return `ApiResult<Json<serde_json::Value>>`
(`me/sync.rs:32,63,99,134,183,223,255,285,321,365,400,439`;
`admin/sync.rs:94,134,173,231,276,545`; `admin/scans.rs:45`; `admin/providers.rs:207,288,382`;
`me/progress.rs:39,115,162,199,233`) while their `#[utoipa::path]` `responses(...)` name a
concrete `body =` type (e.g. `me/sync.rs:25` declares
`body = Vec<tankovault_contracts::sync::ProviderInfo>`).

**Why it matters**
The declaration and the implementation are unrelated artefacts. Nothing — not the compiler,
not a test — connects `body = Vec<ProviderInfo>` to what the handler actually forwards. The
generated `crates/api-client` and the frontend trust the declaration. This is precisely the
drift class that `crates/contracts/src/sync.rs:1-12` was created to eliminate, reintroduced one
layer up.

**Remediation** — with `Upstream` (finding 9), change each read proxy to
`ApiResult<Json<contracts::sync::AccountStatus>>` etc. The deserialize step then *enforces* the
declared schema at the edge. Keep `serde_json::Value` only for the four genuinely
schema-less command responses the module docs already identify
(`services/api/src/openapi.rs:16-19`) and mark them `body = serde_json::Value` so the
declaration is honest.

**Effort: M** (blocked on finding 9)

---

## 11. `services/sync` routes HTTP status by substring-matching error messages

**Severity: High**

**Evidence** — `services/sync/src/main.rs:609-628`:
```rust
struct AppError(anyhow::Error);
...
let status = if message.contains("unknown sync provider") {
    StatusCode::NOT_FOUND
} else if message.contains("account linked") {
    StatusCode::CONFLICT
} else {
    StatusCode::BAD_GATEWAY
};
```
The strings originate at `services/sync/src/engine.rs:158` (`anyhow!("unknown sync provider
…")`) and `:414` (`anyhow!("no {} account linked for user", …)`). `anyhow` is used 30× in
`engine.rs`, 28× in `anilist.rs`, 8× in `provider.rs`.

**Why it matters**
Rewording a log message silently changes an HTTP status contract, with no compile error and no
test that would catch it. The `"account linked"` needle also matches the *negated* message it
is derived from — it is matching the substring of `"no anilist account linked for user"`, so any
future message containing "account linked" (e.g. "account linked successfully") maps to `409`.
The entire sync service has no domain error type; every failure — a provider 500, a sealed-token
UTF-8 failure, a DB outage — is the same opaque `anyhow::Error`.

**Remediation** — introduce `services/sync/src/error.rs`:

```rust
#[derive(Debug, thiserror::Error)]
pub enum SyncError {
    #[error("unknown sync provider: {0}")]      UnknownProvider(String),
    #[error("no account linked at {0}")]        NotLinked(String),
    #[error("provider rejected the request: {0}")] Provider(#[source] anyhow::Error),
    #[error(transparent)]                       Db(#[from] tankovault_db::DbError),
    #[error(transparent)]                       Crypto(#[from] tankovault_auth::AuthError),
}
```
Return `SyncError` from every `SyncEngine` method (replacing `anyhow::Result`), and map
variant→status exhaustively in `IntoResponse`. Emit RFC 9457 `problem+json` to match
`services/api/src/error.rs:117`, so `Upstream` (finding 9) can parse one error format from
every internal service.

**Effort: M**

---

## 12. Error handling is inconsistent across the eight services: four distinct shapes

**Severity: Medium**

**Evidence** — the workspace has 15 `thiserror` enums, but only one service has a real API
error type:

| service | error → HTTP mechanism | file:line |
|---|---|---|
| `api` | `ApiError` + RFC 9457 `problem+json` | `services/api/src/error.rs:29,117` |
| `sync` | `AppError(anyhow)` + substring match, plain-text body | `services/sync/src/main.rs:609-628` |
| `control-plane` | ad-hoc `(StatusCode, String)` tuples + `fn internal<E: Display>` | `services/control-plane/src/main.rs:198,368` |
| `render` / `challenge-solver` | inline `(StatusCode::BAD_GATEWAY, format!("… failed: {e}"))` | `services/render/src/main.rs:139,154`; `services/challenge-solver/src/main.rs:123` |

**Why it matters**
`services/api` proxies three of these four. A caller (and the `Upstream` client of finding 9)
must parse four different error encodings — problem+json, bare text, `Display`-formatted
anyhow, and formatted `format!` strings. There is no way to write one correct upstream error
mapper.

**Remediation** — move `ProblemDetails` and the `IntoResponse` impl from
`services/api/src/error.rs` into `crates/service/src/problem.rs`:

```rust
pub struct Problem { pub status: StatusCode, pub kind: &'static str, pub detail: String }
impl IntoResponse for Problem { … }          // RFC 9457, one implementation
pub trait IntoProblem { fn into_problem(self) -> Problem; }
```

Each service keeps its own `thiserror` enum and implements `IntoProblem`. `services/api`'s
`ApiError` becomes one implementor rather than the definition. Every service then emits one
wire error format.

**Effort: M**

---

## 13. `services/notifier` reimplements SMTP instead of using `crates/email`

**Severity: High**

**Evidence**
- `crates/email/src/lib.rs` provides exactly this: `EmailService` trait (`:84`), `EmailMessage`
  (`:46`), `SmtpMailer` with envelope-sender resolution (`:100-290`), `NoopMailer` (`:292`),
  and `build(&EmailConfig) -> Arc<dyn EmailService>` (`:315`) — with 10 unit tests.
- `services/notifier/src/channels.rs:242-303` re-derives it: its own
  `AsyncSmtpTransport<Tokio1Executor>::from_url`, its own `Mailbox` parsing, its own
  `Message::builder()` assembly, its own `is_positive()` response check.
- `services/notifier/Cargo.toml` depends on `lettre` directly and **does not** depend on
  `tankovault-email`.
- `services/api` uses the crate correctly (`services/api/src/state.rs:66`,
  `services/api/src/mailer.rs`).

**Why it matters**
Two SMTP clients, two `From`/envelope policies, two TLS configurations. `crates/email`
deliberately resolves the envelope sender from the SMTP login rather than the `From` header
(`crates/email/src/lib.rs:175`, tests at `:405-448`) because relays reject the mismatch — the
notifier's copy does not, so operator alerts will be rejected by the same relay that accepts
password-reset mail. It also means the notifier's email path has zero tests while
`crates/email` has ten.

**Remediation**
1. Add `tankovault-email` to `services/notifier/Cargo.toml`; drop `lettre`.
2. Reduce `EmailChannel` to a thin adapter:
   ```rust
   pub(crate) struct EmailChannel { mailer: Arc<dyn EmailService>, to: Vec<String> }
   #[async_trait] impl NotificationChannel for EmailChannel {
       async fn deliver(&self, alert: &Alert) -> anyhow::Result<()> {
           self.mailer.send(EmailMessage::plain(&self.to, email_subject(alert), email_body(alert))).await?;
           Ok(())
       }
   }
   ```
3. Build it from `EmailConfig` via `tankovault_email::build`, so an unconfigured deployment
   gets `NoopMailer` and the channel degrades identically to the API's.

**Effort: S**

---

## 14. The JetStream consume loop is hand-rolled three times

**Severity: Medium**

**Evidence** — the same "select on shutdown → pull → log-and-continue on pull error → decode →
handle → log-and-continue on handler error → ack → warn on ack failure" skeleton appears at:
- `services/notifier/src/main.rs:112-160`
- `services/control-plane/src/aggregator.rs:23-50` (no shutdown arm — it consumes forever, so
  the control-plane cannot drain this consumer on `SIGTERM`)
- `services/worker/src/main.rs:237-303` (via `FairQueue`, plus retry/backoff)

Divergences that matter: the aggregator has **no** cancellation handling; the notifier acks a
message whose fan-out failed (`main.rs:154-160` — `fan_out` error is logged, then `ack()` runs
unconditionally), silently dropping a notification; only the worker implements redelivery
(`is_retryable` :322, `retry_delay` :332, `MAX_TASK_DELIVERIES` :312).

**Why it matters**
Three implementations of at-least-once delivery means three different at-least-once
semantics. The notifier's is actually at-most-once for any failing message. This is a
correctness divergence caused purely by the absence of an abstraction.

**Remediation** — add to `crates/bus/src/lib.rs` (which already owns `with_ack_heartbeat` :58,
`retry_later` :97, `delivery_count` :109 — the hard parts):

```rust
pub struct ConsumePolicy { pub max_deliveries: u64, pub backoff: fn(u64) -> Duration }

pub async fn consume<T, F, Fut>(
    consumer: BrokerConsumer,
    shutdown: CancellationToken,
    policy: ConsumePolicy,
    handler: F,
) -> Result<(), BusError>
where
    T: DeserializeOwned,
    F: Fn(T) -> Fut,
    Fut: Future<Output = anyhow::Result<()>>;
```

It owns decode, the shutdown arm, `with_ack_heartbeat`, `retry_later` on a retryable failure,
terminal ack, and the undecodable-message drop. The three call sites shrink to a handler
closure. Keep `FairQueue` as the worker's *consumer source*; it composes with this.

**Effort: M**

---

## 15. `POST /v1/solve` is implemented twice, byte-identically

**Severity: Medium**

**Evidence** — `services/challenge-solver/src/main.rs:114-127` and
`services/render/src/main.rs:144-157` are the same 13 lines: same `metrics::counter!
("solve_attempts_total", "result" => …)`, same `BAD_GATEWAY`, same `format!("solve failed:
{e}")`, over the same `tankovault_solver::ChallengeSolver` trait.

**Why it matters**
Two services expose the same contract with no shared definition, so they can drift (different
status, different metric label, different error body) while `crates/fetch`'s
`HttpChallengeSolver` (`crates/fetch/src/solver_client.rs`) talks to both and assumes they are
interchangeable — which is the explicit design intent stated at
`services/render/src/solver.rs:1-7`.

Adjacent question worth answering: `services/challenge-solver` is a 127-line HTTP front over
`tankovault_solver::FlareSolverrSolver`, and `services/render` already exposes the same route
over `ChromiumSolver`. The separate service earns its place only if it will host solver
selection/pooling; today it is a process boundary around a trait object.

**Remediation**
1. Add `crates/solver/src/http.rs` behind an `axum` feature:
   `pub fn solver_router(solver: Arc<dyn ChallengeSolver>) -> axum::Router` — one handler, one
   metric, one error mapping.
2. Both services `merge(solver_router(state.solver))`.
3. Separately decide whether `challenge-solver` should absorb backend *selection*
   (FlareSolverr | render | fake) behind one endpoint, which would justify the process; if
   not, fold it into `render` and drop a deployment unit.

**Effort: S** (step 1-2)

---

## 16. Series canonicalisation is implemented twice with different thresholds

**Severity: High**

**Evidence**
- `crates/db/src/repo/catalog.rs:75-123` — `resolve_canonical_series`: candidate lookup →
  `matching::CandidateRow` → `matcher::Candidate` conversion (`:82-90`) → `matcher::decide`
  with **`Thresholds::default()` hardcoded** (`:107`).
- `services/sync/src/engine.rs:983-1035` — `resolve_series`: the identical field-for-field
  `CandidateRow → Candidate` conversion (`:1008-1016`, 8 fields, same order) →
  `matcher::best_match` → compared against **`self.thresholds.high`** (`:1034`), which is
  service-configurable.
- `crates/db/Cargo.toml:11` — the persistence crate depends on `tankovault-matcher`.

**Why it matters**
Two code paths decide "is this the same series?" using the same scorer with *different*
threshold sources and *different* decision functions (`decide` vs `best_match`). The worker can
attach a source that the sync service would refuse to map, and vice versa, with no single place
to reason about it. The duplicated 8-field conversion is the tell: the abstraction exists
(`tankovault_matcher`), the plumbing to it does not. Additionally, `crates/db` executing a
*matching policy* — including writing a `merge_candidate` row on the ambiguous band
(`catalog.rs:109-121`) — puts business logic inside the repository layer.

**Remediation**
1. Add `impl From<matching::CandidateRow> for tankovault_matcher::Candidate` in
   `crates/db/src/repo/matching.rs`; delete both hand-written conversions.
2. Thread thresholds in rather than hardcoding:
   `pub async fn resolve_canonical_series(conn, meta, thresholds: Thresholds)`. The worker
   passes its configured value, matching what `services/sync` already does.
3. Longer term, hoist the decision out of the repository: move
   `resolve_canonical_series` to `services/worker/src/canonicalise.rs`, leaving
   `matching::find_candidates` + `catalog::create_series` +
   `matching::record_merge_candidate` as the persistence primitives it composes. That also
   removes `tankovault-matcher` from `crates/db`'s dependency list.

**Effort: S** (steps 1-2) / **M** (step 3)

---

## 17. `crates/test-support` inverts the crate/service layering and creates a two-hop dev cycle

**Severity: Medium**

**Evidence**
- `crates/test-support/Cargo.toml:17` — `tankovault-api = { path = "../../services/api" }`.
- `services/api/Cargo.toml:53` — dev-depends on `tankovault-test-support`.
- `crates/db/Cargo.toml:30` — **also** dev-depends on `tankovault-test-support`.

The `api ↔ test-support` cycle is documented and intentional (`test-support/Cargo.toml:13-14`).
The `db → test-support → api → db` path is not.

**Why it matters**
`cargo test -p tankovault-db` now compiles the entire API service (and transitively `adapters`,
`fetch`, `solver`, `bus`, `auth`, `email`, `axum`, `utoipa`, `fred`, `reqwest`) to run
repository tests. It also means the lowest layer of the workspace has a dev-time dependency on
the highest, which defeats the `crates/` vs `services/` split as a layering statement.

**Remediation** — split the harness in two:
- `crates/test-support` (no `services/*` dependency): ephemeral Postgres via testcontainers,
  migrations, seeding, token minting. `crates/db` dev-depends on this.
- `services/api/tests/support/` (or a new `services/api-test-support`): the in-process router
  harness, which is the only part that needs `tankovault-api`. Only `services/api` uses it.

Move `crates/test-support` out of `crates/` regardless — a crate that depends on a service does
not belong in the layer beneath services.

**Effort: S**

---

## 18. Feature-flag route tables are duplicated across the API and the sync service

**Severity: Low**

**Evidence** — the same features are gated at two hops with different path prefixes:
`services/api/src/lib.rs:129` gates `/v1/me/sync/conflicts` on `SyncConflictReview` and
`:130` `/v1/me/sync/history` on `SyncHistory`; `services/sync/src/main.rs:367-369` gates
`/v1/sync/push-series`, `/v1/sync/conflicts`, `/v1/sync/history` on `SyncAutoPush`,
`SyncConflictReview`, `SyncHistory`. `services/api/src/me/progress.rs:258` checks
`SyncAutoPush` a third time in a handler body.

**Why it matters**
Defence in depth is defensible, but the two tables are maintained independently: a new
`/v1/me/sync/*` route can be gated at the edge and not at the origin, or vice versa. There is
no test asserting the two tables agree.

**Remediation** — declare the sync surface's feature mapping once in
`crates/contracts/src/sync.rs` as
`pub fn sync_route_features() -> &'static [(&'static str, Feature)]` keyed on the *suffix*
(`"/conflicts"`, `"/history"`, `"/push-series"`), and have both services build their
`RouteFeatures` from it with their own prefix. Effort: **S**.

---

## 19. `services/api/src/auth.rs` (676) and `admin/users.rs` (643) are approaching god-module size

**Severity: Low**

**Evidence** — `auth.rs` holds five independent credential flows: registration (`:96`), login
(`:170`), refresh rotation (`:287`), logout (`:372`), password reset (`:414`, `:456`), email
verification (`:521`, `:567`), plus shared session issuance (`:609`, `:622`) and validation
(`:659`). `admin/users.rs` holds directory listing (`:71`), detail (`:111`), profile update
(`:154`), status (`:234`), permissions (`:310`), session revocation (`:371`), email
verification (`:408`), deletion (`:464`), the permission catalogue (`:559`) and two
safety guards (`:581`, `:607`).

`crates/db/src/repo/users.rs` (657) has the same shape and already marks its own seams at
`:175` (refresh tokens), `:262` (password reset), `:382` (email verification), `:509` (profile
& sessions).

**Why it matters**
Not yet broken, but on the same trajectory as findings 2-6. `auth.rs` is the highest-risk file
in the service and the one most in need of being readable in one sitting.

**Remediation** — `services/api/src/auth/{mod.rs, register.rs, login.rs, refresh.rs,
password_reset.rs, email_verify.rs, session.rs}`; `services/api/src/admin/users/{mod.rs,
directory.rs, profile.rs, status.rs, permissions.rs, deletion.rs, guards.rs}`;
`crates/db/src/repo/users/{credentials.rs, refresh_tokens.rs, password_reset.rs,
email_verification.rs, profile.rs, sessions.rs}` following the existing banners.
Effort: **S** each.

---

## 20. Outbound pacing is implemented three times

**Severity: Low**

**Evidence** — `crates/fetch/src/ratelimit.rs:32-160` (`ThrottlePolicy` + `Throttle` +
`RateLimitedFetcher`, governor-backed, with 429 penalty decay), `crates/fetch/src/backoff.rs`
(`BackoffFetcher`, `Retry-After` aware), and `services/sync/src/anilist.rs:733-761` (`Pacer`,
a minimum-gap mutex). `services/sync` does not depend on `tankovault-fetch`.

**Why it matters**
The AniList client has no `Retry-After` handling and no 429 penalty, which the fetch stack
implements carefully (`backoff.rs:84`). Provider politeness policy — the thing most likely to
get the deployment blocked — lives in two crates with different capability levels.

**Remediation** — extract the transport-agnostic core of `Throttle` into
`crates/fetch/src/pacing.rs` as a `pub struct Pacer { min_interval, penalty }` with
`async fn wait(&self)` and `fn penalise(&self, retry_after: Option<Duration>)`, independent of
`Fetcher`. `RateLimitedFetcher` composes it; `services/sync` depends on `tankovault-fetch` and
uses it directly. Effort: **S**.

---

## 21. Non-findings worth recording (verified clean)

These were checked and are in good shape; noted so a future audit does not re-litigate them.

- **`crates/domain` is genuinely pure.** No infrastructure imports
  (`grep` over `crates/domain/src/*.rs`); `sqlx` is optional and feature-gated
  (`crates/domain/Cargo.toml:19-22`), which is what lets the WASM frontend link it.
- **No sqlx leakage into services.** `grep -rn "sqlx" services/` returns nothing; no service
  manifest declares `sqlx` except via `tankovault-service`'s `db` feature.
- **`crates/api-client` is not dead.** It is a 762 KB progenitor-generated file emitted on one
  line (hence "1 LOC"), consumed by `web/frontend` (`web/frontend/Cargo.toml:27`,
  `src/wire.rs:3`). Regenerated by `xtask openapi` with a `--check` mode.
- **`crates/fetch` is the best-factored crate in the workspace** — a clean decorator chain
  (`lib.rs:29-38`) with each layer independently testable.
- **`crates/service`** achieves what it claims: fixed middleware ordering
  (`http.rs:31-45`), toggles as wiring rather than branches (`lib.rs:15-27`), one `ops_router`.
- **`crates/adapters`** shares a real template — `KunMangaAdapter` delegates
  latest/series parsing to an embedded `GenericConfigAdapter` (`kunmanga.rs:15-17`) and
  overrides only what differs. The one soft spot is the hardcoded slug `match` in
  `factory.rs:48-53`; a `HashMap<&str, fn(AdapterConfig) -> Box<dyn SourceAdapter>>` registry
  would decouple registration from the factory, but at 2 custom adapters this is premature.
- **Audit call sites are well-abstracted** (`services/api/src/audit.rs:17,32,50` — three
  intent-named helpers over one sink).
- **No dead feature flags.** All 37 `Feature` variants (`crates/domain/src/features.rs:51-176`)
  are referenced by a gate, a handler check, or the frontend capability probe.
- **No `#[allow(dead_code)]` anywhere**; the 19 `#[allow(...)]` sites are all narrow clippy
  suppressions, and the three interesting ones (`too_many_lines` at `catalog.rs:933`,
  `matching.rs:187`; `too_many_arguments` at `sync.rs:54,87,758`) are exactly the findings
  above.

---

## Recommended sequencing

| # | Finding | Sev | Effort | Unblocks |
|---|---|---|---|---|
| 1 | `catalog.rs` 5× SQL duplication (#2) | Critical | M | — |
| 2 | Repo rows as wire schema (#1) | Critical | L | #10 |
| 3 | Notifier SMTP duplication (#13) | High | S | — |
| 4 | Matcher conversion + thresholds (#16) | High | S | — |
| 5 | `Upstream` client + status mapping (#9) | High | M | #10 |
| 6 | `SyncError` enum, drop substring routing (#11) | High | M | #12 |
| 7 | `db/repo/{catalog,tracking}` splits (#3, #4) | High | M+S | — |
| 8 | `sync/engine.rs` decomposition (#6) | High | L | — |
| 9 | `bus::consume` helper (#14) | Medium | M | fixes notifier at-most-once |
| 10 | `crates/service::problem` (#12) | Medium | M | — |
| 11 | `test-support` split (#17) | Medium | S | faster `-p tankovault-db` tests |
| 12 | Remaining splits (#5, #7, #8, #19) + #15, #18, #20 | Med/Low | S–M | — |
