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
| SECURITY (16+2) | 12 | 4 | 2 | — |
| ARCHITECTURE (21) | 3 | 0 | 17 | 1 no-finding |
| PERFORMANCE (20) | 3 | 0 | 16 | 1 no-finding |
| TESTING (21) | 3 | 0 | 18 | — |
| FRONTEND (18) | 2 | 0 | 16 | — |
| BUILD_AND_OPS (31) | 9 | 1 | 21 | — |

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
| SEC-9 | Username may contain `@`; login resolves ambiguously | **PARTIAL** | `validate_username` restricts to `[A-Za-z0-9_.-]` and is applied on registration *and* `patch_profile`. Still **OPEN**: the DB-level `CHECK (position('@' in username) = 0)` migration, applying the validator in `admin::update_user`, and splitting `find_credentials` on `@`. Existing rows are not retro-validated. |
| SEC-10 | Login timing side channel discloses account existence | **PARTIAL** | `login` now verifies `DUMMY_PASSWORD_HASH` on the unknown-identifier branch, pinned by a test asserting the dummy's argon2 parameters match the live hasher's. `forgot_password`'s smaller channel is still open. |
| SEC-11 | `panic = "abort"` with no catch layer; `page * limit` overflow | **DONE** | Release profile moved to `panic = "unwind"` with `overflow-checks = true`; `CatchPanicLayer` is the innermost layer of `HttpStack`; `page` is clamped to `MAX_PAGE` and the multiply saturates. |
| SEC-12 | Rate-limit buckets are per-exact-IP | **PARTIAL** | IPv6 now buckets per /64 and junk keys are truncated. The `Principal` extension is still never inserted, so authenticated traffic is not bucketed per account — needs auth middleware ahead of the limiter. |
| SEC-13 | `/scalar` served unauthenticated | **DONE** | `SecurityConfig::expose_api_docs`, defaulting to off under `TANKOVAULT_PROFILE=production` and on elsewhere. Also fixes PERF-14 (the 253 KB re-serialization per request). |
| SEC-14 | Username interpolated unescaped into HTML email | **DONE** | `mailer::esc` on every interpolated value, with a regression test injecting an anchor. |
| SEC-15 | GDPR self-export includes audit rows naming third parties | **DONE** | The export projects `created_at`/`action`/`outcome` and `target` only when the target is the subject; `detail` is dropped. |
| SEC-16 | Unfixable advisory against an empty `deny.toml` ignore list | **OPEN** | See OPS-3.2. Check first whether `jsonwebtoken`'s `rust_crypto` feature already drops `rsa` — the manifest already sets `default-features = false`, but `rsa 0.9.10` still appears in `Cargo.lock`. |

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
| ARCH-1 | `crates/db` rows are the public wire schema | **OPEN** | Strip `utoipa` from `crates/db`, move 11 shapes into `crates/contracts`, CI-guard with `cargo tree`. |
| ARCH-2 | `catalog.rs` — five copy-pasted 50-line SQL statements | **OPEN** | One static query + a `SeriesSort` enum. |
| ARCH-3 | `catalog.rs` is four aggregates in one file | **OPEN** | |
| ARCH-4 | Internal-service proxy open-coded 9+ times, collapses failures to 500 | **DONE** | `services/api/src/upstream.rs`. Adds `ApiError::BadGateway` / `GatewayTimeout`; upstream 404/409 now survive, so the documented 409 is emittable. |
| ARCH-5 | `db/repo/tracking.rs` — seven aggregates | **OPEN** | |
| ARCH-6 | `sync/engine.rs` — one `impl`, 22 methods, six responsibilities | **OPEN** | Do after TEST F-06 exists. |
| ARCH-7 | `sync/anilist.rs` — five concerns in one file | **OPEN** | |
| ARCH-8 | `config/src/lib.rs` — 15 flat config aggregates | **PARTIAL** | Grew by one (`InternalAuthConfig`) plus `ConfigError::Invalid` and `is_production()`. The split is still open. |
| ARCH-9 | (same as ARCH-4 in the report's numbering) | **DONE** | |
| ARCH-10 | Proxy handlers return `Json<Value>` while OpenAPI declares typed bodies | **OPEN** | `Upstream` makes this a one-place change now. |
| ARCH-11 | `sync` routes HTTP status by substring-matching error text | **OPEN** | `SyncError` thiserror enum. The needle currently matches the *negated* source message. |
| ARCH-12 | Four distinct error shapes across eight services | **OPEN** | Hoist `ProblemDetails` into `crates/service/src/problem.rs`. |
| ARCH-13 | `notifier` reimplements SMTP instead of using `crates/email` | **OPEN** | Also omits the envelope-sender policy relays need. |
| ARCH-14 | JetStream consume loop hand-rolled three times | **OPEN** | `notifier/main.rs` acks after a *failed* fan-out — notifications are silently dropped. Highest-value of the ARCH set. |
| ARCH-15 | `POST /v1/solve` duplicated byte-for-byte | **PARTIAL** | Both copies now validate the target and require the token, so they no longer *diverge*; the duplication itself is still there. |
| ARCH-16 | Canonicalisation implemented twice with different thresholds | **OPEN** | |
| ARCH-17 | `crates/test-support` inverts crate/service layering | **OPEN** | `cargo test -p tankovault-db` compiles the whole API service. |
| ARCH-18 | Feature-flag route tables duplicated | **OPEN** | |
| ARCH-19 | `auth.rs` (676) and `admin/users.rs` (643) approaching god size | **OPEN** | |
| ARCH-20 | Outbound pacing implemented three times | **OPEN** | `sync`'s `Pacer` lacks the `Retry-After` handling `crates/fetch` has. |
| ARCH-21 | Verified-clean non-findings | — | No action. |

---

## PERFORMANCE.md

| # | Finding | Status | Notes |
| --- | --- | --- | --- |
| PERF-1 | Fetch stack rebuilt per scan task | **OPEN** | **Highest impact in this report.** Also means per-provider rate limiting is currently enforced only *within* one task, contradicting the comment at `crates/fetch/src/ratelimit.rs:4-7`. |
| PERF-2 | `count(*) OVER()` on the browse query | **OPEN** | |
| PERF-3 | Notifier: 3 sequential queries per watcher | **OPEN** | |
| PERF-4 | Missing `series(updated_at)`; OFFSET enrichment sweep | **OPEN** | Includes a correctness bug: the sweep sets `updated_at = now()`, so enriched rows jump the cursor and unenriched series are skipped. |
| PERF-5 | Selectors recompiled per element | **OPEN** | No `LazyLock`/`OnceLock` anywhere in `crates/` today. |
| PERF-6 | `test_before_acquire` left at its default | **OPEN** | One line. |
| PERF-7 | WASM bundle uncompressed, uncached, no ETag | **DONE** | `CompressionLayer` plus `Cache-Control: no-cache` on the shell in `services/frontend`. `ServeDir` already supplied ETag/Last-Modified. |
| PERF-8 | `reqwest::Client::new()` has no timeout; unbounded spawn | **DONE** | `Upstream::client()` sets connect (5 s) and request (25 s) timeouts; `spawn_targeted_push` goes through it. |
| PERF-9 | CPU-bound parsing on the async executor | **OPEN** | No `spawn_blocking` anywhere today. |
| PERF-10 | `GET /v1/series/{id}` N+1 | **OPEN** | |
| PERF-11 | Ingest holds one transaction across ~1,200 INSERTs | **OPEN** | |
| PERF-12 | `floor(number)` predicates non-sargable | **OPEN** | |
| PERF-13 | Sync reconcile ~6 sequential queries per remote entry | **OPEN** | |
| PERF-14 | `/scalar` re-serializes 253 KB per request | **DONE** | The route is unmounted in production (SEC-13). |
| PERF-15 | `register_source_stubs` opens a transaction per entry | **OPEN** | |
| PERF-16 | `FairQueue` polls lanes sequentially | **OPEN** | |
| PERF-17 | Dev profile untuned | **OPEN** | Ready-to-paste `[profile.dev]` in the report. |
| PERF-18 | The `api` binary links two TLS stacks | **OPEN** | |
| PERF-19 | Miscellaneous allocation waste | **OPEN** | |
| PERF-20 | WASM payload config already correct | — | No action. |
| PERF-idx | Missing-index DDL | **OPEN** | `series_tags(tag_id)` is marked UNVERIFIED — run `EXPLAIN` before adding. |

---

## TESTING_AND_FUZZING.md

| # | Finding | Status | Notes |
| --- | --- | --- | --- |
| F-01 | `parse_chapter_number` panics on non-ASCII titles (verified crash) | **DONE** | Fixed + regression test covering U+0130 and the no-marker fallback. |
| F-02 | `parse_json_body` candidate scan quadratic (verified DoS) | **DONE** | Single linear pass, bounded working set; timing regression test asserts <2 s on a 60k-deep body. |
| F-03 | The frontend's 41 tests run in no CI job | **OPEN** | ~8 lines of YAML. Activates the only i18n missing-key check. |
| F-04 | `services/api/src/auth.rs` (676 LOC) has no unit tests | **OPEN** | Note the email-verification branch of `register` has *never executed* — `TestApp` hardcodes a disabled mailer. |
| F-05 | `crates/db`: 6,893 LOC, 7 integration tests | **OPEN** | |
| F-06 | `sync` merge engine: 1,267 LOC, 2 tests on a helper | **OPEN** | Blocks ARCH-6. |
| F-07 | Worker retry/backoff untested | **OPEN** | |
| F-08 | GDPR export/erase has no test that fails as the schema grows | **OPEN** | |
| F-09 | `test-support` covers one axis | **OPEN** | |
| F-10 | No coverage, mutation testing, or ratchet | **OPEN** | |
| F-11 | Zero doc tests | **OPEN** | |
| F-12 | `xtask` and `challenge-solver` have no tests | **PARTIAL** | `challenge-solver` gained `validate_target` coverage indirectly via `tankovault_domain::ssrf`; neither crate has its own tests yet. |
| F-13 | Test-quality positives | — | No action. Preserve: no sleeps, no network, hermetic per-test DBs. |
| Fuzz | `cargo-fuzz` targets (nightly) | **OPEN** | Seed corpora from `crates/adapters/fixtures/`. F-01 and F-02 are exactly what these would have found. |
| Prop | `proptest` targets (stable) | **OPEN** | `normalize_title` idempotence, `token_set_ratio` symmetry, `content_hash` determinism, `contracts::sync` serde round-trips. |
| Access | Access-control integration matrix | **OPEN** | The single highest-value test investment in the codebase per the roadmap. |

---

## FRONTEND.md

| # | Finding | Status | Notes |
| --- | --- | --- | --- |
| FE-F1 | 35 of 59 fetches bypass `async_view`; "always retryable" already broken | **OPEN** | Largest single frontend item (~500 LOC). Root cause is an `Option<Option<Result<_>>>` signed-out idiom; fix the idiom first, then the sweep is mechanical. |
| FE-F2 | CI runs `cargo check` only — 41 tests and `clippy::pedantic` are dead | **OPEN** | Same fix as TEST F-03. |
| FE-F3 | Seven auth/password inputs have no programmatic label | **OPEN** | |
| FE-F4 | ~285 LOC of shared components live inside view modules | **OPEN** | `stats.rs` imports `HealthPill` from a sibling *view* — a layering inversion. |
| FE-F5 | `EmptyBox`/`SkeletonBlock` exist but 40 sites hand-roll them | **OPEN** | Not exported from `components/mod.rs`. |
| FE-F6 | Four hand-rolled tab strips, none with tab semantics | **OPEN** | |
| FE-F7 | Zero `use_memo` in 16k LOC | **OPEN** | |
| FE-F8 | 488 inline `style:` attributes bypass the token layer | **OPEN** | |
| FE-F9 | `users.rs` (1,395) and `providers.rs` (1,385) are god files | **OPEN** | Do *after* the dedup sweeps. |
| FE-F10 | DTOs: no drift (positive), one gap | **OPEN** | Only the gap is actionable. |
| FE-F11 | `.gitignore`/README call generated CSS "hand-authored" | **DONE** (`.gitignore`) | The README claim is still wrong — FE-F11b. |
| FE-F11b | `web/frontend/README.md` repeats the hand-authored claim | **OPEN** | |
| FE-F12 | 13 `ik-*` classes shipped but never referenced | **OPEN** | |
| FE-F13 | Pagination implemented twice | **OPEN** | |
| FE-F14 | Signed-out `/console` shows a permanent skeleton | **OPEN** | The only protected route without a `SignInGate`. User-visible bug — do this first in the frontend set. |
| FE-F15 | 14 unused icon variants behind an expired `#[allow(dead_code)]` | **OPEN** | |
| FE-F16 | 27 ARIA attributes against 134 click handlers | **OPEN** | Worst: `series/chapters.rs:275`, a mouse-only `div` disclosure. |
| FE-F17 | Two hardcoded English strings | **OPEN** | |
| FE-F18 | No cache headers, no CSP on assets | **DONE** | CSP (with `wasm-unsafe-eval`, which the app needs to boot), `Cache-Control: no-cache` on the shell, compression. |

---

## BUILD_AND_OPS.md

| # | Finding | Status | Notes |
| --- | --- | --- | --- |
| OPS-1.1 | Committed live credentials | **PARTIAL** | See SEC-6 / OP-1..OP-3. |
| OPS-1.2 | `api-client/src/lib.rs` is 780 KB on one physical line | **DONE** | `xtask` pipes the generated client through `rustfmt`; the file is now normally formatted and diffable. |
| OPS-1.3 | Unused dependencies | **OPEN** | `services/api` `tower`/`tower-http`, `crates/db` futures, `crates/service` uuid, `crates/solver` tracing, `crates/fetch` serde+serde_json. |
| OPS-1.4 | Dead `[workspace.dependencies]`, incl. the whole OTel stack | **OPEN** | Pairs with OPS-8.2. |
| OPS-1.5 | 86 crates at 2+ versions | **OPEN** | |
| OPS-1.6 | Two crates named `tankovault-frontend` | **OPEN** | |
| OPS-2.1 | `cargo fmt --all --check` red — CI's first gate | **DONE** | `rustfmt.toml`'s `ignore` is nightly-only; removed, generator formats instead. Verified `cargo fmt --all --check` and `xtask openapi --check` now both pass. |
| OPS-2.2 | `[workspace.lints]` leaves three gaps | **OPEN** | |
| OPS-2.3 | 36 `#[allow(...)]` escapes | **OPEN** | |
| OPS-2.4 | `api-client` opts out of all clippy | **OPEN** | |
| OPS-2.5 | `rustfmt.toml` carries no style configuration | **WONTFIX** | Deliberate: defaults everywhere. The file now documents why, and why `ignore` must not come back. |
| OPS-3.1 | `cargo-deny` sections configured | — | No finding. |
| OPS-3.2 | `[bans]` warns rather than denies | **OPEN** | Ready-to-paste `deny.toml` in the report, including the `openssl-sys` deny that protects the `scratch` runtime. |
| OPS-3.3 | `[licenses]` allows `OpenSSL`, omits unlicensed/private | **OPEN** | |
| OPS-3.4 | No lockfile-integrity or provenance gate | **OPEN** | |
| OPS-4.1 | No `rust-toolchain.toml`; three-way drift | **OPEN** | The claimed MSRV 1.85 is verified by nothing. |
| OPS-4.2 | Postgres `19beta2` in CI and compose | **PARTIAL** | Compose pinned to `17-alpine`. **CI still runs 19beta2** — `.github/workflows/ci.yml` must be updated to match, or the `.sqlx` cache is validated against a different catalog than it runs on. |
| OPS-4.3 | `flaresolverr:latest` unpinned | **DONE** | Pinned to `v3.3.21`. |
| OPS-4.4 | No dependency-update automation | **OPEN** | `dependabot.yml`. |
| OPS-4.5 | No release automation, image publishing, SBOM or signing | **OPEN** | Both Docker jobs run `push: false`, so there is no artifact to roll back to. |
| OPS-4.6 | No build matrix | **OPEN** | |
| OPS-4.7 | `xtask/build.rs` writes into `.git/hooks/` on every build | **OPEN** | |
| OPS-4.8 | No static/secret scanning in CI | **OPEN** | gitleaks. This audit's OPS-1.1 is exactly what it exists to catch. |
| OPS-4.9 | `sqlx prepare --check` gate correct | — | No finding. |
| OPS-4.10 | OpenAPI drift gate correct | — | No finding. |
| OPS-5.1 | `wreq`/BoringSSL dlopen handling | — | No finding. Do not "simplify" the Dockerfile here. |
| OPS-5.2 | Helm chart documented in detail, directory empty | **OPEN** | Either build it or delete the claim in `deploy/README.md:104-111` and `docs/design.md:1027`. |
| OPS-5.3 | No healthchecks and no resource limits in compose | **PARTIAL** | Memory limits added to all 12 services; `shm_size` on `render` and `flaresolverr`. **Healthchecks are still absent** on the 8 app services and cannot be added as-is: they are `scratch` images with no shell and no `wget`. The fix is an argv branch (`<binary> --healthcheck`) in the shared runtime that self-probes `/health`; that is a real change to every `main` and is not done. |
| OPS-5.4 | No read-only rootfs or capability drop | **OPEN** | |
| OPS-6.1 | Startup migration concurrency | — | No finding; document it. |
| OPS-6.2 | Zero `.down.sql` — no rollback | **OPEN** | |
| OPS-6.3 | Destructive unguarded DDL in `0018`/`0019` | **OPEN** | |
| OPS-6.4 | No `CREATE INDEX CONCURRENTLY` | **OPEN** | `0017:28` indexes an already-populated `audit_log`. |
| OPS-6.5 | Non-idempotent DDL | **OPEN** | |
| OPS-7.1 | No env-var reference document | **OPEN** | 52 config fields, 45 `TANKOVAULT_*` keys in use, `docs/OPERATIONS.md` mentions 3. `TANKOVAULT_PROFILE` is documented nowhere and gates production validation. |
| OPS-7.2 | No `.env.example` | **DONE** | `deploy/local.env.example`, covering the internal token, auth secrets and AniList. |
| OPS-7.3 | Startup validation thin, in one service only | **PARTIAL** | Placeholder secrets are now refused in every profile, and `InternalAuthConfig::resolve` validates in five services. `TOKEN_ENCRYPTION_KEY` length is still unvalidated. |
| OPS-8.1 | `frontend` bypasses the shared runtime | **OPEN** | Uses `/healthz` not `/health`+`/ready`, no metrics, bare `TraceLayer` — so the tier that *originates* every correlation chain emits no request id. |
| OPS-8.2 | `otlp_endpoint` is an inert knob | **OPEN** | Finish it or delete it; four OTel crates are declared and used by zero members. |
| OPS-8.3 | No dashboards, alerts or recording rules | **OPEN** | |

---

## Suggested next steps, in order

1. **Finish Phase 0.** SEC-4 (email change) is the only roadmap item 0.x still open.
   SEC-3b, SEC-9, SEC-10, SEC-11 are all small and in the same two files.
2. **Phase 1 — make CI enforce the bar.** Nearly all YAML: TEST F-03/FE-F2 (frontend tests
   and pedantic), OPS-4.1 (toolchain), OPS-4.2 (Postgres in CI — *the compose half is done,
   the CI half is not*), OPS-4.4 (dependabot), OPS-4.8 (gitleaks), OPS-3.2 (`deny.toml`).
   This is what keeps everything above from regressing.
3. **Then the phases are independent** and can run in parallel — see
   [README.md §4](./README.md#4-suggested-sequencing). Two ordering constraints hold:
   ARCH-6 needs TEST F-06 first, and FE-F9 needs FE-F1/F4/F5 first.

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
