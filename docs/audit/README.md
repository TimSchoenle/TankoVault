# TankoVault — Full Codebase Audit (2026-07-29)

Six parallel deep-dive audits over the whole workspace: backend architecture, security,
performance, testing and fuzzing, frontend, and build/dependency/operational hygiene.
Scope: 52,934 LOC (36,813 backend across 14 crates + 8 services, 16,121 frontend).

This document is the **index and prioritized roadmap**. The detailed findings — each with
`file:line` evidence, exploit or cost model, and a concrete fix — live in the companion
reports.

> **Remediation is in progress.** [`PROGRESS.md`](./PROGRESS.md) tracks the status of every
> finding below and is the hand-off document — read it before starting work, and update it in
> the same commit as your fix. It also lists the actions only a human can take (credential
> rotation, history purge), which no code change can close.

| Report | Findings | Focus |
| --- | --- | --- |
| [SECURITY.md](./SECURITY.md) | 16 | AuthN/AuthZ, SSRF, secrets, transport, rate limiting, GDPR |
| [ARCHITECTURE.md](./ARCHITECTURE.md) | 21 | Layering, abstraction, modularization, error model, god modules |
| [PERFORMANCE.md](./PERFORMANCE.md) | 20 | DB access, async runtime, allocation, caching, asset delivery |
| [TESTING_AND_FUZZING.md](./TESTING_AND_FUZZING.md) | 21 | Coverage distribution, harness, fuzz/proptest targets, CI gates |
| [FRONTEND.md](./FRONTEND.md) | 16 | Dioxus component abstraction, async state, a11y, styling |
| [BUILD_AND_OPS.md](./BUILD_AND_OPS.md) | 31 | Workspace, deps, CI/CD, Docker, migrations, config, observability |

Related existing documents — this audit **supersedes neither**:
[`docs/PRODUCTION_READINESS.md`](../PRODUCTION_READINESS.md) (test-pyramid roadmap; its central
thesis that verification is the biggest gap is confirmed and sharpened here),
[`docs/design.md`](../design.md), [`docs/IMPLEMENTATION_STATUS.md`](../IMPLEMENTATION_STATUS.md).

---

## 1. Executive summary

The codebase is **substantially above average** for a project of this size, and several
subsystems are genuinely well built. That makes the failures specific rather than systemic,
and therefore cheap to fix relative to their severity.

**What is strong (verified, no action needed):**

- The authorization *design*: capabilities are resolved fresh from the database on every
  request rather than read out of the access token, so revocation is immediate. All 45 admin
  handlers are permission-gated with 100% coverage. Zero dynamic SQL anywhere.
- Credential handling: argon2id with a keyed server-side pepper, refresh-token rotation with
  family revocation on reuse detection, AES-GCM at rest with a redacted `Debug` impl,
  in-memory-only token storage in the SPA.
- `crates/domain` is genuinely pure — no `sqlx` leaks into any service, and `crates/fetch` is a
  clean decorator stack.
- The `wreq`/BoringSSL `dlopen` constraint in the Docker build is handled robustly, with the
  non-obvious reasoning recorded in place.
- Test *craft* is high: no sleeps, no network, no shared mutable state, hermetic per-test
  databases inside a per-binary testcontainer.
- The frontend's DTOs are 100% generated from `tankovault-api-client`, i18n discipline is
  near-perfect (2 hardcoded strings in 16k LOC, with a catalogue-parity test), and there is
  zero Tailwind class duplication.

**What fails an enterprise bar, in order:**

1. **Three internal services are exposed and unauthenticated.** `sync` trusts a caller-supplied
   `user_id`; `render`/`challenge-solver` are arbitrary-URL fetchers that return the body. All
   are published on the host by the shipped compose file.
2. **Live secrets are committed to git**, including the AES key that seals every user's OAuth
   tokens at rest.
3. **Two verified, remotely-triggerable defects in the provider parsers** — one panic, one
   quadratic-time DoS — both reachable from hostile upstream HTML/JSON.
4. **CI's first gate is red**, which likely means more than half the pipeline has not been
   running.
5. **Verification is unevenly distributed**: the 10 largest modules hold 11 tests across
   9,600 LOC, and the frontend's 41 tests execute nowhere.
6. **`crates/db` row structs are the public HTTP schema**, so a column rename silently rewrites
   the public API with no compile error.

**A recurring theme worth naming:** in four separate areas the correct abstraction *already
exists and is bypassed* — `crates/email` (notifier reimplements SMTP), `crates/bus` (three
hand-rolled consume loops), the SSRF guard (never imported by the service that needs it most),
and the frontend's `async_view` (bypassed by 35 of 59 fetch sites). This is a discipline and
CI-enforcement problem more than a design problem, and it is the cheapest class of fix in this
audit.

---

## 2. Cross-cutting findings

Findings that surfaced independently in two or more audits, which raises confidence and
usually means the fix pays off twice.

### 2.1 The SSRF guard exists but is not wired up

Security F5 and Architecture both land on this. `ssrf::validate_url` checks only scheme and
host presence; the actual address check lives in a custom DNS resolver that hyper's connector
skips entirely for IP-literal authorities. `is_forbidden_ip` and `resolve_checked` are called
from *nowhere*. `services/render` never imports the module at all, and
`admin/providers.rs:94-105` stores an operator-supplied `base_url` unvalidated — so
`ProvidersCreate` + `ProvidersTest` reaches cloud instance metadata through the dry-run path.

### 2.2 Existing shared crates are bypassed by their intended consumers

- `services/notifier` reimplements lettre transport, `Mailbox` parsing, and message building
  (`channels.rs:242-303`) instead of using `crates/email`, and omits the
  envelope-sender-from-login policy that relays require — so operator alerts get rejected on
  relays where password-reset mail succeeds.
- The JetStream consume loop is hand-rolled three times with divergent semantics.
  `notifier/main.rs:112-160` acks after a *failed* fan-out (at-most-once: notifications are
  dropped); `control-plane/aggregator.rs:23-50` has no shutdown arm and cannot drain on
  SIGTERM. `crates/bus` already owns `with_ack_heartbeat`, `retry_later`, and `delivery_count`.
- The `POST /v1/solve` handler is duplicated byte-for-byte between `render/main.rs:144` and
  `challenge-solver/main.rs:114`.
- Outbound pacing is implemented three times; `sync`'s `Pacer` (`anilist.rs:733`) lacks the
  `Retry-After`/429-penalty handling `crates/fetch` already has.
- Frontend: `EmptyBox` and `SkeletonBlock` exist but are not exported from
  `components/mod.rs:10-13`, producing 28 and 12 hand-rolled duplicates respectively.

### 2.3 The wire contract has no single source of truth

Architecture F1 (schema census: `contracts`=5, `domain`=19, `db`=23, `api`=75) and Frontend F
converge here from opposite directions. `crates/db` depends on `utoipa`; 23 repository row
structs derive `ToSchema` and 11 handlers return them verbatim. The frontend is *not* the
problem — it consumes a generated client correctly. The problem is upstream: the OpenAPI spec
those types generate is a projection of the database schema.

### 2.4 The fetch/parse layer is the highest-risk surface and the least tested

Security (SSRF), Performance (per-task client rebuild, per-element selector recompilation,
CPU-bound parsing on the async executor), and Testing (two verified crash/DoS defects, ASCII-only
test inputs) all point at `crates/adapters` + `crates/fetch`. It processes fully
attacker-controlled input, and it is where the proposed fuzzing effort should start.

### 2.5 CI does not enforce what the codebase already declares

`cargo fmt --check` exits 1 because `rustfmt.toml:6` uses a nightly-only `ignore` option while
CI runs stable; that step gates three downstream jobs. The frontend crate declares a full
`clippy::pedantic` set and has 41 tests, and CI runs neither. Coverage, mutation testing, and
doc tests are absent. Several findings below would have been caught mechanically.

---

## 3. Prioritized roadmap

Effort estimates are engineer-days for one person familiar with the code. "Verified" means the
agent reproduced the defect by execution rather than by reading.

### Phase 0 — Stop the bleeding (2-3 days)

Everything here is either remotely exploitable, a live credential exposure, or a red gate
hiding other failures. Nothing in this phase is a refactor.

| # | Item | Source | Effort |
| --- | --- | --- | --- |
| 0.1 | Rotate the committed AniList client secret and `TOKEN_ENCRYPTION_KEY`; re-encrypt or invalidate `external_accounts`; purge from git history; fix `.gitignore` to cover `local.env` | SEC-6, OPS-1 | 1d |
| 0.2 | Stop publishing `sync` (8083), `control-plane` (8081), `render`/`challenge-solver` (8084), Postgres, Redis, and NATS on the host in `deploy/docker-compose.yml`; move to an internal network | SEC-1, SEC-2 | S |
| 0.3 | Add service-to-service authentication on `sync`, and derive `user_id` from the authenticated principal rather than the request body | SEC-1 | 1d |
| 0.4 | Validate URLs in `render`/`challenge-solver` through the SSRF guard; reject non-http(s) schemes; require auth | SEC-2 | 0.5d |
| 0.5 | Fix the `html.rs:115` char-boundary panic (byte offset from `to_lowercase()` applied to the original string) — **verified crash**, reachable from provider chapter-anchor text | TEST F-01 | S |
| 0.6 | Fix the quadratic `collect_objects` in `json.rs:120-186` — **verified**: 600 KB input takes 29.5s; the 8 MiB body cap implies ~96 min CPU | TEST F-02 | S |
| 0.7 | Fix `rustfmt.toml:6` (drop the nightly-only `ignore`; fix the generator to emit newlines instead) and confirm the `lint` job — and its three dependent jobs — actually pass | OPS-2, OPS-3 | 0.5d |
| 0.8 | Make the SSRF guard load-bearing: call `resolve_checked`/`is_forbidden_ip` on the connector path, and validate `base_url` in `admin/providers.rs` | SEC-5 | 0.5d |
| 0.9 | Require a current-password check + re-verification + session revocation on `PATCH /v1/me/profile` email change; add an authenticated change-password endpoint | SEC-4 | 0.5d |
| 0.10 | Fix the `X-Forwarded-For` rate-limit bypass: read the right-most entry, not the left-most | SEC-3 | S |

### Phase 1 — Make CI enforce the bar (2-3 days, almost no product code)

This phase is deliberately early: it is cheap, it is nearly all YAML and TOML, and it prevents
regression of everything in Phase 0.

- Run the frontend's 41 tests and its declared `clippy::pedantic` in CI (~8 lines of YAML) —
  this activates the only i18n missing-key check and 8 chapter part-grouping tests.
- Add `rust-toolchain.toml` and an MSRV job; the claimed 1.85 is verified by nothing.
- Add `dependabot.yml`; `cargo-deny` detects vulnerable dependencies but nothing fixes them.
- Add a gitleaks job (this audit's finding 0.1 is exactly what it exists to catch).
- Add `cargo-llvm-cov` reporting (no threshold yet — measure first), `cargo-nextest`, and doc
  tests (currently 0).
- Add job timeouts and a CI check that `assets/main.css` matches `input.css` output.
- Correct `.gitignore:24-27`, which describes generated Tailwind output as "hand-authored".
- Move Postgres off `19beta2` in CI and compose; the 187-entry `.sqlx` cache is derived against
  beta catalogs.
- Ready-to-paste `[workspace.lints]`, `deny.toml`, `dependabot.yml`, and CI job snippets are at
  the end of [BUILD_AND_OPS.md](./BUILD_AND_OPS.md).

### Phase 2 — Close the verification gap (2-3 weeks)

Sequenced by risk-per-day, and consistent with the pyramid already proposed in
`docs/PRODUCTION_READINESS.md`.

1. **Access control integration tests.** Boot the router; assert 401/403/2xx per caller class
   across the admin matrix. This is the single highest-value test investment in the codebase.
2. **`services/api/src/auth.rs`** (676 LOC, 0 unit tests). Note the email-verification branch of
   `register` has *never executed* — `TestApp` hardcodes a disabled mailer.
3. **`crates/db`** (6,893 LOC, 7 integration tests). Start with `catalog.rs` (1,408), `sync.rs`
   (944), `users.rs` (657) — all currently at zero. Include the GDPR export/erase SQL.
4. **`services/sync/src/engine.rs`** (1,267 LOC, 2 tests on a helper) and worker retry/backoff
   + `content_hash`.
5. **Fuzzing and property testing** — zero tooling present today. Highest value first:
   - `cargo-fuzz` (nightly): `parse_chapter_number` non-panic, `parse_json_body` under
     `-timeout=2`, HTML extraction. Seed corpora from the existing
     `crates/adapters/fixtures/`.
   - `proptest` (stable): `normalize_title` idempotence (the result is stored in a DB column
     and has never been proven), `token_set_ratio` symmetry, `content_hash` determinism, and
     `crates/contracts/src/sync.rs` serde round-trips — zero tests today, against a documented
     history of contract-drift bugs.
   - Also check `(a-b).abs()` on `i32` at `matcher/src/lib.rs:92`, overflow-capable from
     `parse_year`-derived input (reachability UNVERIFIED).

### Phase 3 — Frontend abstraction (1.5-2 weeks)

Ordered so each step ships independently. Full sketches in [FRONTEND.md](./FRONTEND.md).

1. **Correctness first.** `/console` shows a permanent skeleton when signed out
   (`console/mod.rs:247` gates on `caps.is_ready()`, which never resolves while signed out) —
   it is the only protected route without a `SignInGate`. Fix the Users pagination bug where
   `has_next` and the "showing 1-N of TOTAL" line compare a *client-filtered* row count against
   a *server* total (`users.rs:158-160`). Add `for`/`id` labels, `<form>`, and `autocomplete`
   to the 7 auth/password inputs.
2. **Restore the `async_view`/`async_list` invariant** — the largest single item (~500 LOC, 3-4
   days). 35 of 59 `use_resource` sites bypass it, so `FRONTEND_AS_BUILT.md` §7's promise that
   "a failed fetch is always visible and always retryable" has already stopped holding
   (`console/overview.rs:37-52`, `stats.rs:41-49`, `audit.rs:39-47` render failures as muted
   grey text with no retry). Root cause is an `Option<Option<Result<_>>>` signed-out idiom that
   no helper can consume — fix the idiom, then the sweep is mechanical.
3. **Consolidate components.** Export `EmptyBox`/`SkeletonBlock` (40 hand-rolls). Promote the
   ~285 LOC of shared components living inside `views/console/shell.rs` — `stats.rs` and
   `solver.rs` currently import `HealthPill` out of a sibling *view*, a layering inversion.
   Reconcile the two rival `Kpi` components plus a third inline hand-roll. Unify 4 identical
   tab strips and the two pagination implementations.
4. **God-file splits** (`console/users.rs` 1,395; `console/providers.rs` 1,385;
   `discover.rs` 913) — deliberately *after* the sweeps, so the splits land on deduplicated code.
5. **Polish.** Add `use_memo` (there are zero in 16k LOC; `users.rs:140-160` clones the whole
   directory page plus every filtered row on each keystroke). Migrate the 488 inline `style:`
   attributes, which form a parallel style system invisible to the density knob and light theme.
   Accessibility: 27 ARIA attributes against 134 click handlers; worst is
   `series/chapters.rs:275`, a mouse-only `div` disclosure with no keyboard path.

> Note on the original premise: the frontend's problem is **not** hand-mirrored DTOs, Tailwind
> soup, or missing i18n — all three are already handled well, and pointing `web/frontend` at
> `crates/contracts` would be a downgrade from the generated client. The problem is that the
> good abstractions are bypassed. See [FRONTEND.md](./FRONTEND.md) §1.

### Phase 4 — Performance (1-1.5 weeks)

Ordered by measured or modeled impact.

1. **Cache `Arc<dyn Fetcher>` per provider id.** `worker/engine.rs:305` rebuilds the entire
   fetch stack per scan task — a fresh `wreq::Client` means zero connection reuse (a full TLS
   handshake per task, ~500k on a full scan) *and* the governor limiter and semaphore reset each
   task, so per-provider rate limiting is currently enforced only *within* one task. The comment
   at `crates/fetch/src/ratelimit.rs:4-7` claiming a per-provider limiter is presently false.
2. **`GET /v1/series`**: drop the unpartitioned `count(*) OVER()` (it materializes every matching
   row before `LIMIT 40`), add the missing `series(updated_at)` index, and move deep pagination
   off `OFFSET`. The `($n IS NULL OR ...)` guards and `content_type::text` casts make every
   filter non-sargable.
3. **Batch the notifier's 3 sequential queries per watcher** (`notifier/main.rs:180-225`) —
   ~15s at 10k watchers. The house `UNNEST` + `ON CONFLICT` + `RETURNING` pattern is already
   demonstrated at `scans.rs:253-264`.
4. **`LazyLock` the selectors** (`html.rs:55-77` re-parses on every extract, inside per-item
   loops — a 20k-entry sitemap page costs 40k parses of two constant strings). There is
   currently no `LazyLock`/`OnceLock` anywhere in `crates/`.
5. **Set `test_before_acquire(false)`** (`pool.rs:19-23`) — sqlx defaults it to `true`, costing an
   extra round trip per repo call; series detail makes ~8.
6. **Add `CompressionLayer` + `Cache-Control` + ETag to the static tier**
   (`services/frontend/src/main.rs:149`) — the WASM bundle ships uncompressed today, ~1-3 MB
   extra per cold load. The API tier already has compression.
7. **Fix the enrichment sweep** (`catalog.rs:216-222`): OFFSET-based and O(n²), plus a
   correctness bug — it sets `updated_at = now()`, so enriched rows jump past the cursor and
   unenriched series are silently skipped.
8. Add timeouts to `reqwest::Client::new()` at `api/src/main.rs:214`, which feeds an unbounded
   `tokio::spawn` in `spawn_targeted_push` — it leaks tasks and sockets if `sync` hangs.
9. Move CPU-bound HTML parsing to `spawn_blocking`; there are no `spawn_blocking` calls today.
10. Split `ingest_series`, which holds one transaction across ~1,200 per-row INSERTs while
    taking global row locks on shared `tags`/`authors`.

The full missing-index table is in [PERFORMANCE.md](./PERFORMANCE.md). `series_tags(tag_id)` is
marked UNVERIFIED — run `EXPLAIN` before adding, as the planner may already be satisfied via the
primary key. The WASM release profile is correctly tuned and needs no change; the root
`Cargo.toml` has no `[profile.dev]` at all.

### Phase 5 — Architecture (2-3 weeks, mostly independent of the above)

1. **Reclaim the wire contract.** Strip `utoipa` from `crates/db`, move the 11 leaked shapes into
   `contracts::{admin,me,catalogue}`, add `From` impls in `services/api`, and CI-guard with
   `cargo tree -p tankovault-db | grep utoipa`.
2. **Collapse the five copy-pasted 50-line SQL statements** in `catalog.rs:934-1220`
   (`:949`, `:998`, `:1048`, `:1097`, `:1147`) that differ only in `ORDER BY` and repeat the same
   nine filter predicates — adding one filter currently means five identical edits. Replace with
   one static query plus a `SeriesSort` enum in place of `Option<String>`.
3. **Introduce a `SyncError` thiserror enum.** `services/sync/main.rs:609-628` routes HTTP status
   by substring-matching error text, and the needle matches the *negated* source message at
   `engine.rs:414`. There are zero domain error types in that service.
4. **Extract `services/api/src/upstream.rs`** with a typed `Upstream` client and
   `map_upstream_status`. The proxy block is open-coded 9+ times, collapses upstream failures to
   500, and discards `sync`'s deliberate 404/409 — OpenAPI currently documents a 409 the code
   cannot emit.
5. **Hoist `ProblemDetails`/`IntoResponse` into `crates/service/src/problem.rs`.** There are four
   different error-to-HTTP shapes across the services, and `services/api` proxies three of them.
6. **Add `bus::consume(consumer, shutdown, policy, handler)`** and delete the three hand-rolled
   loops (see §2.2 — one of them silently drops notifications).
7. **Route `services/notifier` through `crates/email`** and delete its private SMTP stack.
8. **Unify canonicalisation**, implemented twice with different thresholds
   (`catalog.rs:75-123` vs `sync/engine.rs:983-1035`) and an identical 8-field
   `CandidateRow → Candidate` conversion copy-pasted at both sites.
9. **Split the god modules** — `sync/engine.rs` (one `impl`, 22 methods, 6 responsibilities;
   `reconcile_series` alone is 216 lines), `db/repo/tracking.rs` (7 unrelated aggregates),
   `db/repo/catalog.rs` (4 aggregates, with banners already marking the seams), and
   `config/src/lib.rs` (15 flat config types plus domain policy that belongs in
   `crates/domain`). Named module splits are proposed in [ARCHITECTURE.md](./ARCHITECTURE.md).
10. **Fix the test-support layering inversion**: `crates/test-support` depends on `services/api`
    and `crates/db` dev-depends on it, so `cargo test -p tankovault-db` compiles the entire API
    service.

### Phase 6 — Operational maturity (1-2 weeks)

- **Release automation.** Both Docker jobs run `push: false` — images are built and
  structure-tested but never published, so there is no artifact to roll back to. Add a tag
  trigger, a registry, SBOM (which matters more than usual here, since `scratch` images defeat
  package-manager scanners), and signing.
- **Either build the Helm chart or delete the claim.** `deploy/README.md:104-111` documents
  `helm/tankovault` in detail and links a README; `find deploy/helm -type f` returns 0 files.
  `docs/design.md:1027` repeats the claim. There is no working Kubernetes path today.
- **Document the config surface.** 52 config fields, 45 distinct `TANKOVAULT_*` keys in use,
  and `docs/OPERATIONS.md` mentions 3. `TANKOVAULT_PROFILE` gates production validation and is
  documented nowhere. Add validation for `TOKEN_ENCRYPTION_KEY` length, matching the existing
  `MIN_JWT_SECRET_LEN` check.
- **Finish or remove the OTel knob.** `TANKOVAULT_TELEMETRY__OTLP_ENDPOINT` is inert: all four
  OTel crates are declared in `[workspace.dependencies]` and used by zero members;
  `telemetry.rs:47-52` only logs "export is pending". There is no distributed tracing across
  api → NATS → worker.
- **Bring `services/frontend` onto the shared runtime.** It uses `/healthz` instead of
  `/health` + `/ready`, has no readiness check on the API upstream, exports no metrics, and
  uses a bare `TraceLayer` instead of `HttpStack` — so the tier that *originates* every
  correlation chain emits no request id.
- **Migrations.** Zero `.down.sql` files means no production rollback, against destructive
  statements like `DROP COLUMN role; DROP TYPE user_role` (`0018:87-88`). `0017:28` adds an
  index to an already-populated `audit_log` without `CONCURRENTLY`.
- **Compose hardening.** No resource limits anywhere, no healthchecks on any of the 8 app
  services, `flaresolverr:latest` unpinned, `render` missing `shm_size`.
- **Dependency hygiene.** Remove unused declarations (`services/api` still declares
  `tower`/`tower-http` with 0 hits in its `src`; also `crates/db` futures, `crates/service`
  uuid, `crates/solver` tracing, `crates/fetch` serde+serde_json, plus 11 dead
  `[workspace.dependencies]`). Set `deny.toml`'s `multiple-versions`/`wildcards` to `deny` with
  an explicit skip list — the 86 duplicate versions make the current warning pure noise, and
  nothing today blocks a transitive `openssl-sys`, which would break the `scratch` runtime at
  exec time.
- Stop `xtask/build.rs` writing into `.git/hooks/` on every build.

---

## 4. Suggested sequencing

Phases 0 and 1 should be done first and together — Phase 1 is what keeps Phase 0 fixed. After
that the phases are largely independent and can run in parallel across people:

- Phase 2 (testing) and Phase 3 (frontend) share no files.
- Phase 5 (architecture) should land *after* Phase 2's tests exist for the modules it moves,
  particularly the `crates/db` and `sync/engine.rs` splits.
- Phase 4 (performance) items 1-6 are independent quick wins and can be picked up at any time.

## 5. Reading the detailed reports

Each report orders its findings by severity and uses a consistent format: Title, Severity,
Evidence (`file:line` plus the offending snippet), why it matters, a concrete remediation, and
an effort estimate. Anything the agent could not confirm by execution or direct reading is
marked **UNVERIFIED** — treat those as leads, not facts.

[SECURITY.md](./SECURITY.md) and [BUILD_AND_OPS.md](./BUILD_AND_OPS.md) contain the credential
findings with the secret values redacted; the live values are in the git-tracked file named in
finding 0.1 and must be rotated regardless of whether history is purged.
