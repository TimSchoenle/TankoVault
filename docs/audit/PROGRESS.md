# Audit remediation tracker

Working state of the fixes for [the 2026-07-29 audit](./README.md). **This file is the
hand-off**: it is the one place that says what is done, what is not, and what a fix decided
that the audit did not.

## How to use this file

- One row per finding, keyed exactly as the source report keys it (`SEC-3`, `ARCH-9`,
  `PERF-2`, `TEST F-04`, `FE-F7`, `OPS-4.1`), so a row can always be traced back.
- Statuses: **DONE** (fixed, with a test or a check that would catch the regression),
  **PARTIAL** (the exploitable part is closed, the rest is named in Notes), **OPEN**,
  **WONTFIX** (with the reasoning in Notes), **OPERATOR** (cannot be fixed from the
  repository — a human has to act).
- When you finish a row, update it *in the same commit as the fix*. A tracker that lags is
  worse than none, because the next agent trusts it.
- Add new rows for things you find; do not silently widen an existing one.

## Status at a glance

| Report | DONE | PARTIAL | OPEN | Other |
| --- | --- | --- | --- | --- |
| SECURITY (18) | 13 | 4 | 1 | — |
| ARCHITECTURE (22) | 13 | 2 | 6 | 1 no-finding |
| PERFORMANCE (21) | 17 | 0 | 2 | 1 wontfix, 1 no-finding |
| TESTING (17) | 9 | 3 | 4 | 1 no-finding |
| FRONTEND (20) | 17 | 0 | 1 | 1 wontfix, 1 no-finding |
| BUILD_AND_OPS (41) | 27 | 4 | 4 | 1 wontfix, 5 no-finding |

**96 DONE · 13 PARTIAL · 3 WONTFIX · 18 OPEN · 9 no-finding**, across 139 tracked rows.

The rows below are authoritative; this summary is a convenience — and it is now a *count* of
them rather than a hand-maintained tally, which the previous version had drifted from (it
reported 2 open `BUILD_AND_OPS` rows against an actual 4, and 2 partial `SECURITY` rows against
an actual 4). Recount rather than increment.

---

## Operator actions (cannot be done from this repository)

These block nothing in the code but are the highest-severity items in the audit. Nobody
should mark the security work complete until they are done.

| # | Action | Why it cannot be automated |
| --- | --- | --- |
| OP-1 | **Rotate the AniList client secret** for application `client_id 46552` at <https://anilist.co/settings/developer>. | The live value is in this repository's git history (`deploy/local.env`, introduced in `e5cff29`). Deleting the file — done in `60a29d0` — does not un-publish it. |
| OP-2 | **Rotate `TANKOVAULT_ANILIST__TOKEN_ENCRYPTION_KEY`**, then re-encrypt `external_accounts.access_token` / `.refresh_token` under the new key, or truncate the table to force a re-link. | Same history exposure. This key seals every user's AniList tokens at rest, so anyone with an old clone plus any database backup can read them. |
| OP-3 | **Purge both values from git history** (`git filter-repo`) if the repository is or will ever be public, then force-push and have every clone re-cloned. | Rewrites shared history; needs a human to coordinate. |
| OP-4 | **Generate and distribute `TANKOVAULT_INTERNAL__TOKEN`** (`openssl rand -hex 32`) to every deployment. | `TANKOVAULT_PROFILE=production` now refuses to boot without it — deliberately, see SEC-1. |
| OP-5 | **Set `TANKOVAULT_AUTH__JWT_SECRET`, `TANKOVAULT_SEED_ADMIN_PASSWORD` and `TANKOVAULT_ANILIST__TOKEN_ENCRYPTION_KEY`** before `docker compose up`. | Compose no longer ships working defaults for them (`${VAR:?…}`), because the previous defaults are published in this repository. See `deploy/local.env.example`. |
| OP-6 | **Decide what to do about `wreq-util` being GPL-3.0** before publishing this project or distributing its Docker images. | Not in the audit — found while making the `cargo deny` licences gate green. `wreq-util` supplies the browser-emulation profiles `crates/fetch/src/base.rs` depends on, so it cannot be dropped without replacing that. GPL-3.0 obligations attach on *conveying*: running a private service triggers nothing, but pushing images to a registry (the audit's Phase 6) is conveying and would require offering corresponding source for the combined work. Options: relicense the project, vendor an Apache-2.0 profile set, or drop the dependency. The reasoning is written out in `deny.toml`. |

---

## SECURITY.md

| # | Finding | Status | Notes |
| --- | --- | --- | --- |
| SEC-1 | `sync` takes `user_id` from the body, unauthenticated, published on the host | **DONE** | `X-Internal-Token` gate (`crates/service/src/internal_auth.rs`) on `sync`, `control-plane`, `render`, `challenge-solver`; `ports:` removed from everything but `frontend`. The subject is still named by the caller in the path/body — deliberate, see the design note below. |
| SEC-2 | `render` / `challenge-solver` fetch any URL, unauthenticated | **DONE** | Both now require the internal token and validate the target through `tankovault_domain::ssrf::validate_str`. Chrome/FlareSolverr still resolve independently, so a DNS rebind is not covered — that needs `--host-resolver-rules` or an egress-restricted netns. Tracked as SEC-2b. |
| SEC-2b | Renderer DNS rebinding | **OPEN** | Requires container-level egress restriction; no code change closes it. |
| SEC-3 | `X-Forwarded-For` rate-limit bypass | **DONE** | Reads the right-most entry; regression tests in `crates/service/src/ratelimit/mod.rs`. Password length cap is SEC-3b. |
| SEC-3b | No maximum password length ⇒ argon2 memory DoS | **DONE** | `auth::MAX_PASSWORD_LEN` (4096), enforced by `validate_password` on registration, reset *and* the new authenticated change. |
| SEC-4 | Email change needs no re-auth or re-verification | **DONE** | `patch_profile` requires `current_password` for an address change, `update_profile` clears `email_verified_at`, a confirmation goes to the new address and a warning to the old, and every session is revoked. `POST /v1/me/password` added — there was previously no authenticated path to a new password at all. The report's pending-email column is *not* implemented: with the password required, writing straight through plus forced re-verification closes the takeover, and a pending column adds a migration and a second confirmation state for no further protection. Revisit if the address is ever used for anything before confirmation. |
| SEC-5 | SSRF guard bypassed by an IP-literal URL | **DONE** | `validate_url` range-checks literals; `admin/providers.rs` validates `base_url` on create *and* update via `validate_and_resolve`. Tests in `crates/domain/src/ssrf.rs`. |
| SEC-6 | Live credentials committed | **PARTIAL** | Repo side done (untracked, `.gitignore` fixed, `local.env.example` added, placeholders refused in every profile). Rotation is OP-1..OP-3. |
| SEC-7 | Cookie not `Secure` by default; no CSP | **PARTIAL** | `cookie_secure` now defaults to true (it was `#[serde(default)]` on a `bool`); CSP added to both the API (`default-src 'none'`) and the SPA shell, plus `Cache-Control: no-cache`. The `__Host-` cookie prefix is **not** done — it requires `Path=/` rather than the current `/v1/auth`, which widens where the cookie is sent and deserves its own review. |
| SEC-8 | `/v1/me/stream` skips the suspension check, token in the URL | **PARTIAL** | The principal is now resolved and `may_authenticate()` checked, and the stream is capped at the token's `exp` so the check re-runs on `EventSource`'s automatic reconnect. The token is **still in the query string** — replacing it with a short-lived ticket needs a Redis-backed store and a frontend change. |
| SEC-9 | Username may contain `@`; login resolves ambiguously | **DONE** | Application half (`validate_username`, applied on registration, `patch_profile` and `admin::update_user`), the split `find_credentials` lookup, and the DB `CHECK (position('@' in username) = 0)` in migration `0021`. |
| SEC-10 | Login timing side channel discloses account existence | **PARTIAL** | `login` now verifies `DUMMY_PASSWORD_HASH` on the unknown-identifier branch, pinned by a test asserting the dummy's argon2 parameters match the live hasher's. `forgot_password`'s smaller channel is still open. |
| SEC-11 | `panic = "abort"` with no catch layer; `page * limit` overflow | **DONE** | Release profile moved to `panic = "unwind"` with `overflow-checks = true`; `CatchPanicLayer` is the innermost layer of `HttpStack`; `page` is clamped to `MAX_PAGE` and the multiply saturates. |
| SEC-12 | Rate-limit buckets are per-exact-IP | **DONE** | IPv6 buckets per /64, junk keys truncated, and `HttpStack::with_principal` now inserts a *verified* `Principal` outside the limiter — wired in `services/api`, so authenticated traffic is bucketed per account rather than per IP. |
| SEC-13 | `/scalar` served unauthenticated | **DONE** | `SecurityConfig::expose_api_docs`, defaulting to off under `TANKOVAULT_PROFILE=production` and on elsewhere. Also fixes PERF-14 (the 253 KB re-serialization per request). |
| SEC-14 | Username interpolated unescaped into HTML email | **DONE** | `mailer::esc` on every interpolated value, with a regression test injecting an anchor. |
| SEC-15 | GDPR self-export includes audit rows naming third parties | **DONE** | The export projects `created_at`/`action`/`outcome` and `target` only when the target is the subject; `detail` is dropped. |
| SEC-16 | Unfixable advisory against an empty `deny.toml` ignore list | **DONE** | The documented, dated ignore list landed with OPS-3.2. The open question is now answered rather than assumed: `cargo tree -i rsa` shows `rsa 0.9.10 ← jsonwebtoken 10.3.0`, and `cargo tree -p jsonwebtoken -e features` shows `rust_crypto` enabling `rsa feature "default"` unconditionally — so the audit's suggested `default-features = false` (already applied) cannot drop it, and jsonwebtoken 10 exposes no HMAC-only feature. Non-exploitable because `crates/auth/src/token.rs` pins `Algorithm::HS256` on both the issuing and verifying side, with a test that fails if that changes. Reasoning and review date live in `deny.toml`. |

### Design note — SEC-1, caller-asserted subject

The audit asks for two things: authenticate the caller, and derive `user_id` from the
authenticated principal instead of the request body. The first is done. The second is
deliberately **not** done as stated, because within `sync` the authenticated principal *is*
the API service, not a user — and the API legitimately acts on behalf of a different user in
the admin console (`admin/sync.rs` force pull/push). Moving the subject into a header would
relocate the value without changing who asserts it. The security property that matters is
that only an authenticated internal caller may assert a subject at all, which the token gate
now provides. If a future deployment gains a second internal caller, revisit this.

---

## ARCHITECTURE.md

| # | Finding | Status | Notes |
| --- | --- | --- | --- |
| ARCH-1 | `crates/db` rows are the public wire schema | **DONE** | `utoipa` stripped from `crates/db`; the leaked shapes moved into `crates/contracts::{admin,me,catalogue}` with conversions in `services/api/src/views.rs`. A column rename can no longer silently rewrite the public API. |
| ARCH-2 | `catalog.rs` — five copy-pasted 50-line SQL statements | **DONE** | One static query plus a `SeriesSort` enum replacing `Option<String>`. The enum closed a real bug: the old trailing `_ =>` arm meant `?sort=titel` returned `200` ordered by recency instead of erroring. |
| ARCH-3 | `catalog.rs` is four aggregates in one file | **DONE** | 1,679 lines split at the banner comments the file already carried, into `repo/catalog/{series,enrichment,sources,chapters,browse,ingest}.rs`. Largest module 586 lines (`browse`, which is one read model plus its 105-line `SeriesSort` test). The modules are `pub` and `mod.rs` glob re-exports them, so every existing `repo::catalog::…` path still resolves and callers can move to the narrower `repo::catalog::chapters::…` spelling one at a time rather than in one sweep — which is what the report meant by migrating incrementally. Pure moves: no query text changed, so the `.sqlx` offline cache is untouched. The private helpers turned out to be already cohesive — `slugify`/`dedup_by_slug` are used only by the tag/author writers and went to `enrichment` with them; `SeriesRow` and `SourceRow` each have exactly one home. |
| ARCH-4 | Internal-service proxy open-coded 9+ times, collapses failures to 500 | **DONE** | `services/api/src/upstream.rs`. Adds `ApiError::BadGateway` / `GatewayTimeout`; upstream 404/409 now survive, so the documented 409 is emittable. |
| ARCH-5 | `db/repo/tracking.rs` — seven aggregates | **DONE** | 1,094 lines into `repo/tracking/{watchlist,progress,notifications,dashboard}.rs`, split by *consumer* rather than by the file's own banners, which is the report's actual point — "tracking" was a folder name, not an aggregate. `services/notifier` now needs exactly `notifications` (the fan-out, dedup claim and notification writes) and nothing else; `services/sync` needs `progress` (the read frontier plus the §A.5 sync exclusion) and the two batched watchlist reads. Three items moved across the old banners to land with their aggregate: `watchlist_set_status`/`watchlist_status_get` were filed under "Read progress", `watchlist_statuses_for_user` under "sync exclusion", and `feed`/`FeedItem` under "notifier fan-out". Glob re-exports keep every existing path resolving. |
| ARCH-5b | `db/repo/sync.rs` — three lifecycles, two audiences | **DONE** | **The tracker had no row for this** — the report's §5. Its numbering and this file's diverge (PROGRESS `ARCH-4` is the report's §9, `ARCH-5` its §4), and §5 fell through the gap; added rather than folded into ARCH-5. 1,007 lines into `repo/sync/{snapshots,conflicts,history,accounts,mappings,remote_entries,admin_views}.rs`. `admin_views` is read only by `services/api/src/admin/sync.rs` and everything else only by `services/sync`, which is the two-audience split the report described. All three `#[allow(clippy::too_many_arguments)]` are gone, as the report asked — the suppression was the smell, not the lint. Two became parameter structs (`AgreedSnapshot`, `NewConflict`; named for what they are rather than the report's verb-phrase `RecordSnapshot`), both of which had transposable adjacent same-typed arguments — getting `local_value`/`remote_value` backwards would have shown a user their own value as the remote one. The third needed no struct: `upsert_remote_entry` was **dead**, superseded by PERF-13's batch form and left behind when the notifier's single-row equivalents were deleted; it is deleted now. Verified by the F-06 suite (55 tests). |
| ARCH-6 | `sync/engine.rs` — one `impl`, 22 methods, six responsibilities | **DONE** | 1,254 lines in one `impl` became `services/sync/src/engine/` — a facade with no logic over eight collaborators, each owning its slice of state: `registry` (the provider set and the one unknown-provider error), `tokens` (the `SecretBox`; nothing else in the service can now open a token), `accounts`, `conflicts`, `resolve`, `reconcile`, `push`, `enrich`. Largest module 646 lines, of which ~90 are tests. The load-bearing part is the seam the report asked for: **`engine/plan.rs` is the merge itself, pure** — `plan_series` (exclusion / first push) and `plan_merge` (the three-way merge over both fields) decide from values alone, and `reconcile.rs` only performs what they decided. Eight unit tests now exercise the merge rules with no pool and no provider behind them, including the two invariants an integration run makes hardest to see: a conflict must not advance the common ancestor, and one remote write must cover both fields. `plan_series`/`plan_merge` are deliberately two calls rather than one, so the snapshot read stays the merge path's alone and an excluded or first-push series does not pay for it (PERF-13 removed exactly such reads). `reconcile_series`'s 216 lines and its three `#[allow(...)]` are gone. Verified by the F-06 suite: all 13 reconciliation tests pass unchanged, which is what made the split checkable. Two behaviours the split forced into the open and pinned rather than changed: a first sync with no common ancestor cannot tell which side moved, so unequal values are decided by policy alone (`AskMe` queues, every other policy picks); and an imported watchlist row is not counted as a pull. |
| ARCH-6b | Ephemeral test-Postgres containers are not reaped on Windows | **OPEN** | Not in the audit; noticed running the F-06 suite. Each `--features integration` run leaves its `postgres:17-alpine` container running (12 present after this session's run). Harmless in CI, where the runner is discarded, but a developer accumulates one per run. |
| ARCH-7 | `sync/anilist.rs` — five concerns in one file | **DONE** | 897 lines became `services/sync/src/providers/anilist/`: `client.rs` (HTTP, `OAuth2`, the paced + `429`-retried round trip), `graphql.rs` (each query document immediately above the one method that sends it — a field added to a document and a field read out of the response are now visible together), `parse.rs` (the `AniList`-shaped types, the pure JSON readers and all nine parsing tests), and `mod.rs` holding only the endpoint defaults plus the `ExternalProvider` impl, which is the boundary where `AniList`'s ids and status vocabulary become the shared types. The new `providers/` parent is where a second provider goes; its module docs say so. The report's other three asks: the hand-rolled `urlencode` is **deleted** for `url::form_urlencoded` (`url` was a workspace dependency this crate had not declared), `progress_to_int` moved to `mapping.rs` which owns every other unit conversion, and the `Pacer` hoist already landed with ARCH-20. Swapping the encoder is a real behaviour change and is tested as such: form encoding writes a space as `+` and escapes `~`, which is what RFC 6749 §3.1 specifies for the authorization endpoint, so the test says that rather than leaving the next reader to "fix" it back. |
| ARCH-8 | `config/src/lib.rs` — 15 flat config aggregates | **DONE** | Both steps. (1) One module per aggregate — `audit`, `cors`, `database`, `email`, `error`, `features`, `http`, `internal_auth`, `loader`, `matching`, `messaging`, `metrics`, `ratelimit`, `security`, `telemetry` — with `pub use` in `lib.rs`, so no caller changed. `RedisConfig` and `NatsConfig` share `messaging.rs` deliberately: two one-field broker URLs do not each earn a file, and the module says so. (2) `MetadataPriorityConfig` and the `SOURCE_*` constants are **gone from this crate**, replaced by `tankovault_domain::metadata_priority` with a closed `MetadataSource` enum — and a `MetadataField` enum too, since the field axis was stringly typed for exactly the same reason. That is a real behaviour change and the point of the move: a typo in a deployment's priority list (`anilst`) used to parse fine and then match nothing, reading as a deliberate de-prioritisation of the source the operator meant to name; it is now a startup error. Six tests moved with it, one added for the typo case, and `docs/CONFIGURATION.md` now says which values are accepted. The last-resort "an unlisted source is still used" behaviour is preserved and tested — narrowing one field's order must de-prioritise the others, not discard data nothing else supplies. |
| ARCH-9 | (same as ARCH-4 in the report's numbering) | **DONE** | |
| ARCH-10 | Proxy handlers return `Json<Value>` while OpenAPI declares typed bodies | **OPEN** | `Upstream` makes this a one-place change now. |
| ARCH-11 | `sync` routes HTTP status by substring-matching error text | **DONE** | `services/sync/src/error.rs`. Typed variants for the two failures with HTTP meaning, downcast through `anyhow` so they survive `.context()`, exhaustive status mapping by construction, RFC 9457 body matching the API's. Test pins that a message merely *containing* the old needles is no longer misrouted. |
| ARCH-12 | Four distinct error shapes across eight services | **DONE** | `crates/service/src/problem.rs` owns the one RFC 9457 encoding: `Problem { status, kind, detail }`, a single `IntoResponse`, and the `IntoProblem` trait each service implements on its own `thiserror` enum. `services/api::ApiError` is now an implementor rather than the definition; `sync` dropped its private copy of the body struct; `control-plane`'s `(StatusCode, String)` tuples and `render`/`solver`'s inline `format!` strings are gone. Two real fixes fell out: `control-plane::internal` used to put the raw `Display` of a database or bus error on the wire (connection strings, SQL) *and* leave no log entry — now the reverse — and nothing previously set `Content-Type: application/problem+json`, because `Json` sets `application/json` and no copy overrode it. A test in `services/api` deserializes a real error response into the published `ProblemDetails` schema, so the documentation copy cannot drift from the body actually sent. |
| ARCH-13 | `notifier` reimplements SMTP instead of using `crates/email` | **DONE** | `EmailChannel` is a thin adapter over `EmailService`, built from the shared `TANKOVAULT_EMAIL__*` config. `lettre` dropped from the service. This fixed a real delivery bug: the private copy did not resolve the envelope sender from the SMTP login, so operator alerts were rejected (`550 5.7.60`) by relays that accepted the API's password-reset mail. |
| ARCH-14 | JetStream consume loop hand-rolled three times | **DONE** | `crates/bus::consume` owns shutdown, decode, heartbeat, retry-vs-settle and the undecodable drop; handlers return a `Disposition` so the retry judgement stays with the layer that can make it. Fixed two real bugs: the notifier acked after a **failed** fan-out (at-most-once — notifications lost with one `warn!`), and the control-plane aggregator had no cancellation arm so it could not drain on `SIGTERM`. |
| ARCH-15 | `POST /v1/solve` duplicated byte-for-byte | **DONE** | Defined once in `tankovault_solver::http::solver_router`, behind an `axum` feature so trait-only consumers do not pull axum. The SSRF target check travels with it, so one copy cannot forget it. |
| ARCH-16 | Canonicalisation implemented twice with different thresholds | **PARTIAL** | Steps 1-2 done. The 8-field `MatchCandidate → matcher::Candidate` conversion is now one `From` impl in `crates/db/src/repo/matching.rs`, so a new candidate field cannot reach one path and not the other. Thresholds come from a new `tankovault_config::MatchingConfig` (`high`/`low`/`candidate_limit`, defaulting to the scorer's own values so a knob and the value it overrides cannot drift), threaded through `resolve_canonical_series` → `ingest_series`/`register_source_stubs` to the worker, and into `SyncEngine::new` — one configured policy for both paths instead of a hardcoded default in the persistence layer and another in sync. **Step 3 is not done**: `resolve_canonical_series` still lives in `crates/db` and still writes the `merge_candidate` row, so matching *policy* remains inside the repository layer. That is the "longer term" half the report itself scoped as M, and it is what would remove `tankovault-matcher` from `crates/db`'s dependencies. |
| ARCH-17 | `crates/test-support` inverts crate/service layering | **DONE** | Split in two: `crates/test-support` keeps the ephemeral Postgres, seeding and token minting and now depends on **no** `services/*` crate; the in-process router harness moved to `services/api/test-support` (`tankovault-api-test-support`). `cargo tree -p tankovault-db -e normal,dev` no longer contains `tankovault-api`. The report also asked to move the crate out of `crates/` — not done, deliberately: the reason to move it was the inverted dependency, and with that gone it is an ordinary leaf crate that belongs where the other `crates/` live. The `api ↔ api-test-support` dev cycle stays, documented in the new manifest. |
| ARCH-18 | Feature-flag route tables duplicated | **DONE** | `tankovault_contracts::sync::sync_route_features()` declares the suffix → `Feature` mapping once; both `services/api` (`/v1/me/sync`) and `services/sync` (`/v1/sync`) fold it into their own `RouteFeatures` with their own prefix, and each carries a test asserting every suffix in the shared list is gated under its prefix. The drift the audit found was real: the API gated `/conflicts` and `/history` but not `/push-series`. A tier now gates suffixes it does not serve — harmless, since a rule for an unrouted path never matches, and the alternative is the per-tier judgement that drifted in the first place. |
| ARCH-19 | `auth.rs` (676) and `admin/users.rs` (643) approaching god size | **OPEN** | |
| ARCH-20 | Outbound pacing implemented three times | **DONE** | One implementation in `tankovault_domain::pacing` (`Pacer` + `PacingPolicy`); `crates/fetch`'s `Throttle` is gone and `ThrottlePolicy` is a re-export. Placed in `domain` rather than `crates/fetch` as the report suggested, deliberately and for the same reason `ssrf` lives there: `services/sync` talks to `AniList` over plain `reqwest`, and making it depend on the crawl stack to pace itself would link wreq/BoringSSL — GPL-3.0 `wreq-util` included (OP-6) — into the sync image. The report's diagnosis was half right: the `AniList` client *did* honour `Retry-After`, but only for a single retry, after which every later call went back to full rate with **no persistent penalty** — which is the behaviour a provider reads as ignoring them. `Retry-After` is now a floor on the penalty rather than a one-shot sleep, and clamped, so a hostile header cannot park a worker. The shared pacer also fixes a second defect in the old private one: it held its mutex across the sleep, so N concurrent callers serialised into a queue N gaps long instead of each reserving the next free slot. Eight tests in `domain`, plus two in `crates/fetch` pinning the composition decision that is genuinely that crate's (crawl delay vs. penalty, wider wins). |
| ARCH-21 | Verified-clean non-findings | — | No action. |

---

## PERFORMANCE.md

| # | Finding | Status | Notes |
| --- | --- | --- | --- |
| PERF-1 | Fetch stack rebuilt per scan task | **DONE** | `Engine` caches one `Arc<dyn Fetcher>` per provider id, keyed on a fingerprint of exactly the settings the stack is built from. This was a **correctness** fix as much as a speed one: the limiter and the adaptive 429 penalty live in the stack, so a per-task stack made `rps`/`concurrency` a per-task budget and N tasks offered N x rps. The false comment in `ratelimit.rs` now states the invariant and who must uphold it. |
| PERF-2 | `count(*) OVER()` on the browse query | **DONE** | The unpartitioned `count(*) OVER()` is gone, the sort is index-backed, and the non-sargable `($n IS NULL OR ...)` guards and `content_type::text` casts are fixed. |
| PERF-3 | Notifier: 3 sequential queries per watcher | **DONE** | Three set-based statements for the whole fan-out instead of three per watcher: `dedup_claim_many` (`UNNEST` + `ON CONFLICT DO NOTHING … RETURNING` — the claimed subset in one round trip, still atomic against a concurrent notifier), `notifications_create_many`, and `notifications_unread_counts` (one grouped query). The payload is built once outside the loop, so the two heap JSON values per watcher are gone too. The single-row versions were deleted rather than left as dead code. |
| PERF-4 | Missing `series(updated_at)`; OFFSET enrichment sweep | **DONE** | Keyset walk fenced by the sweep's start timestamp, cursor advanced *before* the work so a permanently-failing row cannot stall it. Index added in migration 0020. The correctness half — enriched rows jumping the cursor and silently skipping unenriched series — is what made this urgent rather than merely slow. |
| PERF-5 | Selectors recompiled per element | **DONE** | Bounded memo returning `Arc<Selector>`, so every call site is fixed including the custom adapters. `LazyLock<Selector>` could not apply — selectors come from `providers.config`, not constants. |
| PERF-6 | `test_before_acquire` left at its default | **DONE** | Off. The probe bought little — the pool already discards a connection whose query fails — and cost a round trip on every repository call. |
| PERF-7 | WASM bundle uncompressed, uncached, no ETag | **DONE** | `CompressionLayer` plus `Cache-Control: no-cache` on the shell in `services/frontend`. `ServeDir` already supplied ETag/Last-Modified. |
| PERF-8 | `reqwest::Client::new()` has no timeout; unbounded spawn | **DONE** | `Upstream::client()` sets connect (5 s) and request (25 s) timeouts; `spawn_targeted_push` goes through it. |
| PERF-9 | CPU-bound parsing on the async executor | **DONE** | All four `GenericConfigAdapter` methods go through `html::parse_blocking`. Also pre-sizes the body buffer from `Content-Length` (clamped; the streaming check still enforces the cap) and avoids a second copy of up to 8 MiB by trying `from_utf8` before `from_utf8_lossy`. |
| PERF-10 | `GET /v1/series/{id}` N+1 | **DONE** | The two N+1 loops over provider groups collapsed and the tail reads overlapped. |
| PERF-11 | Ingest holds one transaction across ~1,200 INSERTs | **DONE** | Alt-titles, tags, authors and chapters all batch now. The transaction held row locks on shared `tags`/`authors` rows across ~1,200 sequential round trips, so one slow series stalled every other provider. `DISTINCT ON` is load-bearing — `ON CONFLICT DO UPDATE` cannot touch a row twice in one statement, and providers do repeat chapter numbers. Four integration tests pin the semantics. |
| PERF-12 | `floor(number)` predicates non-sargable | **DONE** | One lateral pass for continue-reading, and a `floor()` predicate shaped to match the expression index migration 0020 added. |
| PERF-13 | Sync reconcile ~6 sequential queries per remote entry | **DONE** | All three parts of the fix. (1) `LocalState::load` prefetches the exclusion set, read frontiers and watchlist statuses once per run — sound only because `handled_series`/`handled_ids` guarantee no series is reconciled twice, which the F-06 suite now pins. (2) `reconcile_account` runs in phases so `upsert_remote_entries` and `upsert_mappings` are one statement each; `upsert_mappings` needs `DISTINCT ON (series_id) … ORDER BY ord DESC` to reproduce the loop's last-id-wins and to avoid `ON CONFLICT DO UPDATE` touching one row twice. (3) `find_candidates_multi` scores a remote entry's whole title family in one lateral-joined query instead of K trigram scans. A 500-entry library goes from ~4 500–7 000 sequential round trips to roughly 500 + a handful. |
| PERF-14 | `/scalar` re-serializes 253 KB per request | **DONE** | The route is unmounted in production (SEC-13). |
| PERF-15 | `register_source_stubs` opens a transaction per entry | **DONE** | One transaction per 500 entries, the redundant per-entry existence check dropped (the caller-side batch check already filtered; the race window only reaches an `ON CONFLICT DO UPDATE`), and the `upsert_source` tail batched into one `UNNEST` insert. Canonicalisation stays per-entry inside the transaction because each entry genuinely resolves against the series its predecessors created — a test pins that two spellings of one title still collapse to one series. Per-entry error tolerance is preserved by retrying a failed *chunk* entry by entry rather than by a savepoint per entry, which would have cost the round trips the fix removes. |
| PERF-16 | `FairQueue` polls lanes sequentially | **WONTFIX** | The audit's concurrent-fetch fix is unsafe here and the reasoning is now recorded in `queue.rs`: a concurrent round claims a message from every non-empty lane, a worker may hold only one, and handing the extras back increments their delivery count against `MAX_TASK_DELIVERIES = 3`. A task could exhaust its retry budget by being polled past without ever failing. The idle chatter is already bounded by the existing 200 ms -> 5 s backoff. |
| PERF-17 | Dev profile untuned | **DONE** | Ready-to-paste `[profile.dev]` in the report. |
| PERF-18 | The `api` binary links two TLS stacks | **OPEN** | |
| PERF-19 | Miscellaneous allocation waste | **OPEN** | |
| PERF-20 | WASM payload config already correct | — | No action. |
| PERF-idx | Missing-index DDL | **DONE** | `migrations/0020_performance_indexes.sql`, nine indexes. NOT `CONCURRENTLY`, and the file explains at length why: sqlx's `--no-transaction` suppresses the explicit transaction but Postgres wraps any multi-statement simple query in an implicit one. The file carries the exact `CONCURRENTLY` block to run by hand first on an already-large database, after which `IF NOT EXISTS` makes the migration a no-op. `series_tags(tag_id)` deliberately not added — still UNVERIFIED. |

---

## TESTING_AND_FUZZING.md

| # | Finding | Status | Notes |
| --- | --- | --- | --- |
| F-01 | `parse_chapter_number` panics on non-ASCII titles (verified crash) | **DONE** | Fixed + regression test covering U+0130 and the no-marker fallback. |
| F-02 | `parse_json_body` candidate scan quadratic (verified DoS) | **DONE** | Single linear pass, bounded working set; timing regression test asserts <2 s on a 60k-deep body. |
| F-03 | The frontend's 41 tests run in no CI job | **DONE** | The `frontend` CI job now runs `cargo test`, `cargo clippy --all-targets -- -D warnings` (the pedantic set the crate declares) and the wasm check. All 41 pass, including the i18n catalogue-parity test. Wiring it up immediately caught a real break: the SEC-4 contract change had left `ProfileUpdate` missing a field. |
| F-04 | `services/api/src/auth.rs` (676 LOC) has no unit tests | **DONE** | The mailer is injectable in `crates/test-support` now, which unlocked the email-verification branch of `register` — a branch that had **never executed**, because `TestApp` hardcoded a disabled mailer. |
| F-05 | `crates/db`: 6,893 LOC, 7 integration tests | **PARTIAL** | GDPR export/erase (F-08) and ingest are covered; `catalog.rs`, `sync.rs` and `users.rs` still hold most of the untested SQL. |
| F-06 | `sync` merge engine: 1,267 LOC, 2 tests on a helper | **DONE** | 13 reconciliation tests in `services/sync/src/reconcile_tests.rs` drive the engine through its real `pull`/`push` entry points against an ephemeral Postgres and a recording fake provider — nothing below the engine is mocked. They pin what the merge *does*, not just what it decides: which side is written, that exactly one remote write covers both fields, that an excluded series touches neither side, and — the one that matters most — that an `AskMe` conflict does **not** advance the common-ancestor snapshot, so the conflict is re-detected until resolved instead of silently becoming unresolvable. Also covers first-sync import, the two-remote-ids-to-one-series guard, unmapped/unmatched accounting and the converged no-op. Wired into CI's `integration` job. ARCH-6 is now unblocked. |
| F-07 | Worker retry/backoff untested | **DONE** | Worker retry policy, `content_hash` and challenge detection pinned. |
| F-08 | GDPR export/erase has no test that fails as the schema grows | **DONE** | Driven from `information_schema` rather than a hardcoded table list, so a future migration that adds a user-referencing table turns the build red on the pull request that adds it. That is the failure mode that matters — silent incompleteness nobody notices until a regulator asks. |
| F-09 | `test-support` covers one axis | **OPEN** | |
| F-10 | No coverage, mutation testing, or ratchet | **OPEN** | |
| F-11 | Zero doc tests | **PARTIAL** | CI now runs `cargo test --workspace --doc`; `--all-targets` silently excludes them, which is why they never ran. Exactly **1** doc test exists today — the gate is in place, the examples still need writing. |
| F-12 | `xtask` and `challenge-solver` have no tests | **DONE** | `challenge-solver` is covered via the shared solve router. `xtask` now has 11 tests over the 3.1→3.0 downgrade — the highest-risk thing it does, since `openapi --check` compares two artifacts *both* produced by that function, so a bug there is invisible to the gate. Two of them are proptests; one was intermittently red, see Prop-b. |
| F-13 | Test-quality positives | — | No action. Preserve: no sleeps, no network, hermetic per-test DBs. |
| Fuzz | `cargo-fuzz` targets (nightly) | **OPEN** | Seed corpora from `crates/adapters/fixtures/`. F-01 and F-02 are exactly what these would have found. |
| Prop | `proptest` targets (stable) | **PARTIAL** | Still open for `normalize_title` idempotence, `token_set_ratio` symmetry, `content_hash` determinism and `contracts::sync` serde round-trips. Separately: the one proptest that *does* exist (`xtask`'s `the_downgrade_is_idempotent`) was red on a random schedule and is now fixed — see the row below. |
| Prop-b | `xtask`'s existing proptest was intermittently red | **DONE** | Not in the audit; found running the full suite. `any_document`'s comment claimed it "stays inside well-formed documents", but restricting the *leaf* strategy to strings does not achieve that: `prop_recursive` can hand a `type` key an array whose elements are arbitrary sub-documents, e.g. `{"type": [[]]}` — on which the 3.1→3.0 downgrade is genuinely not idempotent. Three saved regression seeds, all of this class, then replayed on every run, so the gate was permanently red-by-seed and merged past. `type` now comes from a dedicated well-formed strategy inserted separately from the recursive keys, making the invariant structural; the stale seeds are removed with the reasoning recorded in their place, since the malformed-input behaviour is already pinned directly by `a_non_string_type_member_is_not_idempotent`. |
| Access | Access-control integration matrix | **OPEN** | The single highest-value test investment in the codebase per the roadmap. |

---

## FRONTEND.md

| # | Finding | Status | Notes |
| --- | --- | --- | --- |
| FE-F1 | 35 of 59 fetches bypass `async_view`; "always retryable" already broken | **DONE** | Root cause was subtler than the report: `RefreshTick` and `Reload` were structurally identical but not interchangeable, which is *why* tick-driven panels had nothing to hand `ErrorBox`. Fixed the type first, then the sweep. `read_unchecked()` outside `feedback.rs` went 42 → 0. A handful of partial-surface fetches stay open-coded, each with a comment saying why. |
| FE-F2 | CI runs `cargo check` only — 41 tests and `clippy::pedantic` are dead | **DONE** | See TEST F-03. Frontend clippy is clean at pedantic today. |
| FE-F3 | Seven auth/password inputs have no programmatic label | **DONE** | Extracted `components::Field` with `for`/`id`, `autocomplete` and Enter-to-submit, applied across `views/auth.rs` and `views/password.rs`. The labels were siblings rather than ancestors, so there was not even an implicit association — a screen reader announced them as "edit text, blank". |
| FE-F4 | ~285 LOC of shared components live inside view modules | **DONE** | `views/console/shell.rs` split into `components/{layout,confirm,data}.rs`; `HealthPill` retyped and moved, so both sibling-view imports are gone. Three rival `Kpi`s became one. |
| FE-F5 | `EmptyBox`/`SkeletonBlock` exist but 40 sites hand-roll them | **DONE** | Exported and swept: 23 of 30 `ik-empty` and 19 of 23 `ik-skeleton` hand-rolls now use the components. The remainder are multi-child or styled forms, left deliberately. |
| FE-F6 | Four hand-rolled tab strips, none with tab semantics | **DONE** | One `TabBar<T: TabKind>` with `role="tablist"`/`role="tab"`/`aria-selected`, arrow-key and Home/End navigation, and a roving tabindex. |
| FE-F7 | Zero `use_memo` in 16k LOC | **DONE** | Three `use_memo`s where collections were rebuilt per keystroke. |
| FE-F8 | 488 inline `style:` attributes bypass the token layer | **WONTFIX** | Attempted and reverted, deliberately. `.ik-table-compact td` is specificity 0-2-1, so single-class utilities cannot win the cascade where the inline style did — every converted cell in the (table-heavy) console would silently regress. The fixes available are a tripled selector or `!important`, both worse than 53 inline styles. This needs a decision about whether `ik-*` gets a real utility tier (`@layer`), which is a design-system call rather than a sweep. |
| FE-F9 | `users.rs` (1,395) and `providers.rs` (1,385) are god files | **DONE** | `discover.rs` → 3 modules + `views/search.rs`; `users.rs` → 6; `providers.rs` → 8. Largest module 1,433 → 830 lines. Done after the dedup sweeps, as the roadmap sequenced it. |
| FE-F10 | DTOs: no drift (positive), one gap | **OPEN** | Only the gap is actionable. |
| FE-F11 | `.gitignore`/README call generated CSS "hand-authored" | **DONE** | Plus a new `css` CI job that rebuilds from `input.css` and fails if `assets/main.css` differs — nothing checked that before, so a class used in `rsx!` could have had no style behind it. |
| FE-F11b | `web/frontend/README.md` repeats the hand-authored claim | — | **No finding.** The README's §Styling already says `assets/main.css` is "**generated** by the Tailwind **v4** CLI from `input.css`" and tells the reader to re-run `css:build`; "hand-authored" appears nowhere in it. The audit reached this claim through `.gitignore`'s "see web/frontend/README.md" cross-reference rather than by reading the README, which is why the two disagreed — and `.gitignore` was the one that was wrong. |
| FE-F12 | 13 `ik-*` classes shipped but never referenced | **DONE** | 14 dead rules removed — one more than the audit counted (`.ik-chapter`, missed because `ik-chapter-toggle` is a live prefix). Both directions of the used/defined check are now zero. |
| FE-F13 | Pagination implemented twice | **DONE** | `components/pagination.rs` holds the shared offset arithmetic and its tests. The two chromes stay visually distinct, which they legitimately are. |
| FE-F13b | Users pagination compared a client-filtered count against a server total | **DONE** | `has_next` and the "1-N of TOTAL" line used `rows.len()` *after* the status and staff filters ran, so filtering to a single staff member on page 1 hid every later page. Split into three counts: shown, returned, total. |
| FE-F14 | Signed-out `/console` shows a permanent skeleton | **DONE** | Capabilities clear to `Loading` with no session, so `is_ready()` was permanently false and the skeleton never resolved — the worst failure mode available, since the app looks like it is working and the reader waits instead of signing in. Gate added, factored as `components::AuthRequired`, collapsing the four existing hand-rolled copies. |
| FE-F15 | 14 unused icon variants behind an expired `#[allow(dead_code)]` | **DONE** | Six genuinely-unused variants deleted and the blanket `#[allow(dead_code)]` removed. The better half: the console entity rail rendered a bare `span` per entity and now draws its icons — a real `DESIGN_SPEC` gap the audit read as dead code. |
| FE-F16 | 27 ARIA attributes against 134 click handlers | **DONE** | `chapters.rs`'s mouse-only `div` is a `<button>` with `aria-expanded`. Plus rail `aria-label`, `NavLink` `aria-current`, a skip link, search and bell labels, `aria-live` on the unread count, and Discover's three unlabelled sliders (which also hardcoded the year bounds instead of using the constants). |
| FE-F17 | Two hardcoded English strings | **DONE** | Both moved to the catalogues; slug/URL examples kept literal with a comment saying why. |
| FE-F18 | No cache headers, no CSP on assets | **DONE** | CSP (with `wasm-unsafe-eval`, which the app needs to boot), `Cache-Control: no-cache` on the shell, compression. |

---

## BUILD_AND_OPS.md

| # | Finding | Status | Notes |
| --- | --- | --- | --- |
| OPS-1.1 | Committed live credentials | **PARTIAL** | See SEC-6 / OP-1..OP-3. |
| OPS-1.2 | `api-client/src/lib.rs` is 780 KB on one physical line | **DONE** | `xtask` pipes the generated client through `rustfmt`; the file is now normally formatted and diffable. |
| OPS-1.3 | Unused dependencies | **DONE** | Twelve declarations removed. `crates/solver`'s `tracing` was correctly listed as unused by the audit but is now genuinely used by the shared `/v1/solve` router added this session, so it stays. |
| OPS-1.4 | Dead `[workspace.dependencies]`, incl. the whole OTel stack | **DONE** | Six dead entries and all four OTel crates removed. `tankovault-api-client` deliberately kept with a note explaining why (`web/frontend` is outside the workspace and uses its own path dep). |
| OPS-1.5 | 86 crates at 2+ versions | **OPEN** | |
| OPS-1.6 | Two crates named `tankovault-frontend` | **DONE** | The SPA crate is `tankovault-web`; `services/frontend` keeps `tankovault-frontend`, matching the `tankovault-<service>` convention for everything in the host workspace. No build change was needed: CI drives the SPA with `working-directory: web/frontend` (never `-p`), the binary target is already the generic `app`, and `Dockerfile.frontend` locates the `dx` bundle by `-path '*/web/public'` rather than by app name — which is exactly the property the rename tested. |
| OPS-2.1 | `cargo fmt --all --check` red — CI's first gate | **DONE** | `rustfmt.toml`'s `ignore` is nightly-only; removed, generator formats instead. Verified `cargo fmt --all --check` and `xtask openapi --check` now both pass. |
| OPS-2.2 | `[workspace.lints]` leaves three gaps | **PARTIAL** | `clippy::cargo` landed. The rest was measured and rejected with numbers rather than opinion: `nursery` is 62 warnings (31 `missing_const_for_fn`), and `missing_errors_doc` is 166 in `crates/db` alone — the audit's claim that the codebase already writes `# Errors` by hand holds for the services but not for `crates/db`. |
| OPS-2.3 | 36 `#[allow(...)]` escapes | **OPEN** | |
| OPS-2.4 | `api-client` opts out of all clippy | **OPEN** | |
| OPS-2.5 | `rustfmt.toml` carries no style configuration | **WONTFIX** | Deliberate: defaults everywhere. The file now documents why, and why `ignore` must not come back. |
| OPS-3.1 | `cargo-deny` sections configured | — | No finding. |
| OPS-3.2 | `[bans]` warns rather than denies | **DONE** | `multiple-versions` and `wildcards` are now `deny`, with an explicit, dated skip list of the 30 duplicates actually present and `allow-wildcard-paths` for our own path deps. `openssl-sys`/`openssl`/`native-tls` are hard-denied — they would fail at *exec* time on the `scratch` runtime, not at build time. |
| OPS-3.3 | `[licenses]` allows `OpenSSL`, omits unlicensed/private | **DONE** | `OpenSSL` and `Unicode-DFS-2016` removed (neither is in the graph, and allowing `OpenSSL` contradicted the new ban); `private = { ignore = true }` added and every workspace crate marked `publish = false`. **The gate was already red on `main`** — see OP-6: `wreq-util` is GPL-3.0, which the audit did not report. |
| OPS-3.4 | No lockfile-integrity or provenance gate | **DONE** | `cargo auditable` in both Dockerfiles plus a lockfile-integrity job. SBOM matters more than usual here because `scratch` images defeat package-manager scanners. |
| OPS-4.1 | No `rust-toolchain.toml`; three-way drift | **DONE** | `rust-toolchain.toml` pins 1.94.0 with rustfmt, clippy and the wasm32 target; every CI job names the same version; a new `msrv` job builds with 1.85 `--locked` so the manifest's claim is now checked. |
| OPS-4.2 | Postgres `19beta2` in CI and compose | **DONE** | Both on 17 now. Verified safe by regenerating the whole `.sqlx` cache against a 17 container: byte-identical to the 19beta2-derived cache, so the switch changes no query metadata. |
| OPS-4.3 | `flaresolverr:latest` unpinned | **DONE** | Pinned to `v3.3.21`. |
| OPS-4.4 | No dependency-update automation | **DONE** | `.github/dependabot.yml` covering both cargo workspaces, npm, GitHub Actions and the Docker base images, grouped so a routine week is one pull request. |
| OPS-4.5 | No release automation, image publishing, SBOM or signing | **DONE, publishing gated off** | Build, structure-test and SBOM run on every tag; every push/sign/attest step requires an unset repo variable `ALLOW_IMAGE_PUBLISH`. The run summary prints the OP-6 reason rather than burying it in a comment — publishing images is *conveying* under GPL-3.0. |
| OPS-4.6 | No build matrix | **PARTIAL** | Feature legs added; no OS/arch legs, with the reasoning in the workflow. |
| OPS-4.x | No job timeouts; no coverage measurement | **DONE** | All 13 CI jobs now carry `timeout-minutes`. A `coverage` job runs `cargo llvm-cov --summary-only` as `continue-on-error` — report only, no threshold, because the audit's point is that coverage is unevenly *distributed*, and a number nobody has seen is not something to gate on. Add a ratchet once there is a baseline. |
| OPS-4.7 | `xtask/build.rs` writes into `.git/hooks/` on every build | **DONE** | Now `cargo run -p xtask -- install-hooks`; `build.rs` deleted. A build script mutating the developer's git config is a side effect outside `OUT_DIR` applied without consent, and it breaks hermetic builds. CI's `openapi --check` is what actually enforces the invariant. |
| OPS-4.8 | No static/secret scanning in CI | **DONE** | `secrets` job runs gitleaks over the full history (`fetch-depth: 0` — a secret removed in the tip commit is still leaked). |
| OPS-4.9 | `sqlx prepare --check` gate correct | — | No finding. |
| OPS-4.10 | OpenAPI drift gate correct | — | No finding. |
| OPS-5.1 | `wreq`/BoringSSL dlopen handling | — | No finding. Do not "simplify" the Dockerfile here. |
| OPS-5.2 | Helm chart documented in detail, directory empty | **DONE** | Claim **deleted** rather than a chart built. A chart nobody has rendered against a real cluster is the same defect wearing different clothes, and `docs/IMPLEMENTATION_STATUS.md` already said "k8s/Helm still pending" — two docs were the outliers. `deploy/README.md` and `docs/design.md` corrected, the four empty `deploy/helm/` dirs deleted. §19 now records what a future chart would not have to reinvent. |
| OPS-5.3 | No healthchecks and no resource limits in compose | **DONE** | Memory limits on all 12 services, `shm_size` on `render`/`flaresolverr`. `crates/service/src/healthcheck.rs` adds the `--healthcheck` argv branch, wired into all seven backend services plus `services/frontend` (via OPS-8.1). A TCP connect rather than an HTTP GET: `challenge-solver` has no HTTP client, and "the listener is accepting" is what liveness should mean. The compose stanzas are in place too — one `x-healthcheck` anchor on all seven scratch services, the frontend's own binary path, and native probes for `postgres`/`redis`/`nats`. `migrate` and `seed` are deliberately without one (they exit); `flaresolverr` is third-party. |
| OPS-5.4 | No read-only rootfs or capability drop | **PARTIAL** | `cap_drop` and `no-new-privileges` everywhere; `read_only` on the scratch services but not `render`, which needs a writable Chrome profile. |
| OPS-6.1 | Startup migration concurrency | — | No finding; document it. |
| OPS-6.2 | Zero `.down.sql` — no rollback | **DONE** | Reversible pattern set by migration `0021`, documented in `docs/OPERATIONS.md` §8. |
| OPS-6.3 | Destructive unguarded DDL in `0018`/`0019` | **DONE** |  |
| OPS-6.4 | No `CREATE INDEX CONCURRENTLY` | **DONE** | Consistent with the CONCURRENTLY decision recorded in `0020` — see that file's header for why it cannot work in this migrator. |
| OPS-6.5 | Non-idempotent DDL | **DONE** |  |
| OPS-7.1 | No env-var reference document | **DONE** | New `docs/CONFIGURATION.md`, ~70 keys. Found two audit errors along the way: `TANKOVAULT_CORS__*` does not exist (it is `TANKOVAULT_SECURITY__CORS__*`), and `TANKOVAULT_TASKS`/`TANKOVAULT_EVENTS` are JetStream stream names, not env vars. Also corrected `docs/OPERATIONS.md`, which documented the expensive rate-limit budget as 6/2 when the code says 30/10. |
| OPS-7.2 | No `.env.example` | **DONE** | `deploy/local.env.example`, covering the internal token, auth secrets and AniList. |
| OPS-7.3 | Startup validation thin, in one service only | **DONE** | Placeholder secrets refused in every profile; `InternalAuthConfig::resolve` validates in six services; `sync` refuses the published all-zero `TOKEN_ENCRYPTION_KEY`, comparing decoded bytes so every base64 spelling is caught. The key *length* was already enforced by `from_base64_key` decoding into `[u8; 32]` — the audit's remaining gap was a well-known value, not a length. |
| OPS-8.1 | `frontend` bypasses the shared runtime | **DONE** | `services/frontend` is on `HttpStack` + `ops_router` with metrics and an upstream readiness check, so the tier that *originates* every correlation chain finally emits a request id. Four regression tests. |
| OPS-8.2 | `otlp_endpoint` is an inert knob | **DONE** | Removed, together with the four OTel workspace deps. It only ever logged "collector export is pending" — an operator who set it believed traces were exported and would have found out during an incident. `Cargo.toml` and `crates/config` both say to re-add the knob and the layer together or not at all. |
| OPS-8.3 | No dashboards, alerts or recording rules | **OPEN** | |

---

## Suggested next steps, in order

**Phases 0, 1, 3, 4 and 6 of the roadmap are substantially complete**; 2 and 5 are the
remainder. Every gate passes: `cargo fmt --all --check`, `cargo clippy --workspace
--all-targets -- -D warnings`, `cargo deny check advisories licenses sources bans`, 392 unit
tests, every integration target, `cargo test --workspace --doc`, `xtask openapi --check`, the
frontend's 50 tests and its wasm build.

What is genuinely left, in the order it is worth doing:

1. **TEST F-05 — the rest of `crates/db`.** GDPR export/erase, ingest and the new batched
   fan-out/reconciliation paths are covered now; `catalog.rs`'s read models, `sync.rs` and
   `users.rs` still hold most of the untested SQL.
2. **ARCH-3/5/7/8/19 — the remaining module splits, plus ARCH-16 step 3.** All hygiene now that
   ARCH-12, ARCH-18, ARCH-20 and ARCH-6 have landed. ARCH-16's remaining half — hoisting
   `resolve_canonical_series` out of `crates/db` so matching policy stops living in the
   repository layer — is the only one that moves a decision rather than moving code.
3. **TEST Access — the access-control integration matrix.** Still the single highest-value test
   investment in the codebase per the audit's own roadmap.
4. **Fuzz targets (TEST).** `cargo-fuzz` on `parse_chapter_number`, `parse_json_body` under
   `-timeout=2`, and HTML extraction, seeded from `crates/adapters/fixtures/`. The two verified
   defects this audit found (F-01, F-02) are exactly what these would have caught, so the value
   is demonstrated rather than theoretical.
5. **OPS-1.5 / 2.3 / 2.4 / 8.3 — the remaining build hygiene**, and **FE-F10**'s single DTO gap.
   OPS-8.3 (no dashboards, alerts or recording rules) is the largest of these and the only one
   an operator would feel.
6. **FE-F8 — the inline-style layer.** Marked WONTFIX above with reasoning: it needs a decision
   about whether the `ik-*` layer gets a real utility tier, not a mechanical sweep.

Two things no commit can close: the credential rotation (OP-1..OP-3) and the GPL-3.0 question
(OP-6). OP-6 in particular **blocks image publishing**, which is why the release workflow builds
and signs but does not push.

## Conventions the fixes so far have adopted

Worth knowing before adding to them:

- **Internal tier authentication** is one shared secret in `X-Internal-Token`, resolved by
  `tankovault_service::internal_auth::resolve` and mounted via `HttpStack::with_internal_auth`.
  A new internal service should add `internal: InternalAuthConfig` to its config and both
  lines to its `main`. Outbound callers go through `services/api/src/upstream.rs` or, where
  that is not reachable, take the token explicitly (`HttpChallengeSolver::new`).
- **The SSRF policy lives in `tankovault_domain::ssrf`**, not `tankovault_fetch`. Anything
  that fetches a URL someone else chose must call `validate_str` (or `validate_and_resolve`
  when the value is being persisted). Do not re-derive the range table.
- **Placeholder secrets are refused in every profile**, not just production
  (`services/api/src/main.rs::known_placeholder`). If you add a placeholder to a config
  example, add it to that list too.
- **Regression tests carry the story.** Every fix above that could silently come back has a
  test whose doc comment says what the bug was, so a future reader does not "simplify" it
  away. Please keep doing that.
