# Kanpai / TankoVault — Test Quality & Fuzz/Property Audit

Scope: testing, test infrastructure, fuzz/property testing. Security, performance, architecture and frontend
design are explicitly out of scope and are owned by other reviewers. Two defects below were found *while
designing fuzz targets* and are reported here because they are the direct justification for those targets;
they are handed to the security/robustness reviewer for triage.

Read-only analysis. No project file was modified.

---

## 0. Headline

The suite that exists is **unusually good** — assertion-rich, intention-named, comment-justified, no sleeps,
no network, no wall-clock coupling, hermetic DB via testcontainers. The problem is not quality, it is
**distribution**. Testing is concentrated in small pure-helper modules and almost entirely absent from the
largest, most business-critical ones. Aggregate density is 396 test functions over ~53k LOC (≈7.5/kLOC), but
the top ten largest modules in the repo hold **11 tests between them across 9,600 LOC**.

Three structural gaps:

1. **No property or fuzz testing at all.** Zero occurrences of `proptest`, `quickcheck`, `arbitrary`,
   `libfuzzer`, `cargo-fuzz` anywhere in the workspace. Every parser in the crawl path is tested only against
   hand-written happy-path strings. Two 15-minute experiments against these parsers produced a reproducible
   **panic** and a reproducible **quadratic-time DoS**, both reachable from a provider-controlled response
   body (findings F-01, F-02, verified by execution).
2. **The frontend's 41 tests never run.** `web/frontend` is `exclude`d from the workspace
   (`Cargo.toml:31`) and CI's `frontend` job runs only `cargo check --target wasm32-unknown-unknown`
   (`.github/workflows/ci.yml:137-139`). `cargo test --workspace` cannot see them. 41 real assertions —
   including the i18n key-completeness guard — are dead weight.
3. **No coverage, no mutation, no fuzz gate, no nextest.** CI enforces fmt/clippy/test/deny/audit/sqlx/docker
   but has no notion of *how much* is tested, and no way to notice that `services/sync/src/engine.rs` grew to
   1,267 lines with two tests on a helper.

---

## 1. Inventory

Counts are `#[test] | #[tokio::test] | #[sqlx::test]` attribute occurrences, split by whether the file sits
under a `tests/` directory. Doc tests: **0 workspace-wide** (`grep '/// ```'` → 0 hits), so
`cargo test --all-targets` skipping doctests is currently harmless but there is no executable documentation.

### 1.1 Per crate / service

| Crate / service | LOC | Unit | Integration | Doc | Tests/kLOC | Risk |
|---|---:|---:|---:|---:|---:|---|
| `crates/adapters` | 3,158 | 50 | 13 | 0 | 19.9 | Low |
| `crates/auth` | 493 | 17 | 0 | 0 | 34.5 | Low |
| `crates/bus` | 493 | 1 | 0 | 0 | 2.0 | **High** |
| `crates/config` | 815 | 9 | 0 | 0 | 11.0 | Medium |
| `crates/contracts` | 469 | 9 | 0 | 0 | 19.2 | Low |
| `crates/db` | 6,893 | 7 | 7 | 0 | 2.0 | **High** |
| `crates/domain` | 2,262 | 41 | 0 | 0 | 18.1 | Low |
| `crates/email` | 461 | 9 | 0 | 0 | 19.5 | Low |
| `crates/fetch` | 1,708 | 30 | 0 | 0 | 17.6 | Low |
| `crates/matcher` | 375 | 11 | 0 | 0 | 29.3 | Low |
| `crates/service` | 3,063 | 48 | 0 | 0 | 15.7 | Low |
| `crates/solver` | 618 | 11 | 0 | 0 | 17.8 | Low |
| `crates/test-support` | 311 | 0 | — | 0 | n/a | harness |
| `crates/api-client` | 1 | 0 | 0 | 0 | n/a | generated |
| `services/api` | 7,940 | 21 | 8 | 0 | 3.7 | **High** |
| `services/control-plane` | 613 | 5 | 0 | 0 | 8.2 | Medium |
| `services/frontend` (static server) | 465 | 7 | 0 | 0 | 15.1 | Low |
| `services/notifier` | 786 | 13 | 0 | 0 | 16.5 | Low |
| `services/render` | 519 | 4 | 0 | 0 | 7.7 | Medium |
| `services/sync` | 3,436 | 27 | 0 | 0 | 7.9 | **High** |
| `services/worker` | 1,349 | 7 | 0 | 0 | 5.2 | **High** |
| `services/challenge-solver` | 127 | 0 | 0 | 0 | 0 | Medium |
| `xtask` | 458 | 0 | 0 | 0 | 0 | Medium |
| `web/frontend` | 16,121 | 41 | 0 | 0 | 2.5 | **High (never executed)** |
| **Total** | **52,934** | **368** | **28** | **0** | **7.5** | |

Integration-test directories exist only at `crates/adapters/tests`, `crates/db/tests`, `services/api/tests`
— confirmed. `crates/adapters/tests` uses committed golden fixtures under `crates/adapters/fixtures/`
(`demonicscans/`, `madara-sample/`, `manhuaus/`, `kunmanga/` — HTML, JSON and XML), pulled in with
`include_str!`.

### 1.2 Largest completely untested modules (0 test attributes)

| LOC | Module | What it owns | Risk |
|---:|---|---|---|
| 1,408 | `crates/db/src/repo/catalog.rs` | series/chapter upsert, catalogue queries, search | **High** |
| 1,395 | `web/frontend/src/views/console/users.rs` | admin user/permission console | High |
| 944 | `crates/db/src/repo/sync.rs` | external-sync ledger, token storage, cursors | **High** |
| 913 | `web/frontend/src/views/discover.rs` | discovery/search UI | Medium |
| 701 | `web/frontend/src/views/series/tracking.rs` | reading-progress UI | High |
| 676 | `services/api/src/auth.rs` | register / login / refresh rotation / reset / verify | **High** |
| 674 | `web/frontend/src/views/series/chapters.rs` | chapter list + part grouping render | High |
| 657 | `crates/db/src/repo/users.rs` | account creation, lookup, email verification | **High** |
| 643 | `services/api/src/admin/users.rs` | privileged user administration | **High** |
| 630 | `services/sync/src/main.rs` | sync service wiring, schedule loop | High |
| 611 | `web/frontend/src/views/account/sync.rs` | AniList link/unlink UI | Medium |
| 603 | `services/api/src/admin/sync.rs` | admin sync control endpoints | High |
| 559 | `services/worker/src/engine.rs` | scan execution, child-task fan-out, content hashing | **High** |
| 506 | `services/api/src/me/sync.rs` | user-facing sync endpoints | High |
| 478 | `services/api/src/admin/providers.rs` | provider CRUD / config validation | High |
| 437 | `services/api/src/admin/privacy.rs` | GDPR request administration | **High** |
| 406 | `crates/db/src/repo/scans.rs` | scan run/task state machine | High |
| 392 | `xtask/src/main.rs` | OpenAPI downgrade + codegen, destructive `reset` | Medium |
| 326 | `crates/db/src/repo/matching.rs` | trigram candidate retrieval | High |
| 319 | `services/api/src/me/privacy.rs` | user GDPR self-service | High |
| 286 | `crates/db/src/repo/user_admin.rs` | status changes (partially covered via `repo_access_control`) | Medium |
| 284 | `services/api/src/me/progress.rs` | reading progress writes | **High** |
| 278 | `crates/db/src/repo/permissions.rs` | grant/revoke/resolve (covered by `repo_access_control`) | Low |

Near-untested but not zero, and worse in practice:

- `services/sync/src/engine.rs` — **1,267 LOC, 2 tests**, both on the 26-line `dedupe_latest_by_external_id`
  helper. `reconcile_account`, `reconcile_series`, `push_series_inner`, `resolve_media_id`, `enrich_series`,
  `apply_metadata` — the entire two-way merge — are untested.
- `crates/db/src/repo/tracking.rs` — **983 LOC, 4 tests**, all on the pure `ReadProgress::covers` predicate.
  Every SQL statement in the file is untested.
- `crates/db/src/repo/gdpr.rs` — **539 LOC, 3 tests**, all on enum classification. `create`, `export`,
  `erase` SQL is untested except `cancel_own` via `repo_access_control.rs:183`.
- `crates/bus/src/lib.rs` — **493 LOC, 1 test**, which asserts a constant ratio
  (`TASK_ACK_HEARTBEAT * 2 <= TASK_ACK_WAIT`). All JetStream stream/consumer setup, ack/nak/`retry_later`
  semantics are untested.

---

## 2. Findings

Severity: **Critical** = active defect or a gate that lets one ship; **High** = business-critical logic with
no executable specification; **Medium** = meaningful gap; **Low** = hygiene.

---

### F-01 — `parse_chapter_number` panics on non-ASCII chapter titles (verified crash)

**Severity: Critical** · **Effort: S (fix) + S (regression test)**

**Evidence:** `crates/adapters/src/html.rs:115-126`

```rust
pub fn parse_chapter_number(text: &str) -> Option<f64> {
    let lower = text.to_lowercase();
    for marker in ["chapter", "episode", "chap", "ch.", "ch ", "#"] {
        if let Some(idx) = lower.find(marker) {
            let tail = &text[idx + marker.len()..];   // <-- index from `lower`, slice into `text`
```

`idx` is a byte offset into `lower`, but the slice is taken from `text`. `str::to_lowercase` is **not
length-preserving**: `'İ'` (U+0130, 2 bytes) lowercases to `"i\u{0307}"` (3 bytes). The offsets drift and the
slice lands inside a multi-byte character.

Reproduced by execution (standalone extraction of the exact function, input `"İchapteré5"`):

```
input bytes=12 lower bytes=13
thread 'main' panicked at 'byte index 10 is not a char boundary;
it is inside 'é' (bytes 9..11) of `İchapteré5`'
```

**Why it matters:** `parse_chapter_number` is called on *provider-controlled anchor text* from three call
sites — `generic.rs:112`, `generic.rs:250`, `demonicscans.rs:147/235`, plus `kunmanga.rs:145/148` on the JSON
`chapter_name` field. A scanlation site is exactly the kind of source that emits Turkish, Vietnamese or
combining-mark titles. The panic unwinds inside a worker task; at minimum the scan task dies, and depending
on the task boundary it can take the tokio worker with it. The three existing tests
(`html.rs:248-252`) are all ASCII, which is precisely the blind spot a proptest closes in one line.

**Concrete remediation:**
1. Fix: iterate `text.char_indices()` and match markers case-insensitively against the original string, or
   track a parallel offset map. Simplest correct form: search on `text` with
   `text.to_lowercase()` replaced by a `char_indices`-driven ASCII-case-insensitive scan over `text`.
2. Regression test `crates/adapters/src/html.rs::tests::chapter_number_survives_non_ascii_titles` asserting
   `parse_chapter_number("İchapteré5")` returns `Some(5.0)` and does not panic.
3. Property target `proptest_parse_chapter_number_never_panics` (see §3, P-01) — the general guard.

---

### F-02 — `parse_json_body` candidate scan is quadratic on hostile bodies (verified DoS)

**Severity: Critical** · **Effort: M**

**Evidence:** `crates/adapters/src/json.rs:120-186` (`collect_objects` / `opens_object` / `balanced_span`),
reached from `parse_json_body` (`json.rs:36`), whose input is `FetchResponse.body` — capped at
`MAX_BODY_BYTES = 8 * 1024 * 1024` (`crates/fetch/src/base.rs:33`).

`collect_objects` iterates **every** `{` in the document and, for each one that passes `opens_object`, runs
`balanced_span`, which scans to end-of-document when the brace never closes. The `MAX_CANDIDATES = 8`
early-return only fires when a candidate was *successfully pushed*, so a body full of unbalanced objects
never trips it. `wrapped_candidates` (`json.rs:78-86`) calls `collect_objects` up to **three** times over the
whole body plus once per `<pre>` block.

Measured, on an extraction of the exact functions (`"{\"a"` repeated):

```
bytes=300000  candidates=0  elapsed=7.25s
bytes=600000  candidates=0  elapsed=29.47s      # 2x input -> 4x time, confirmed quadratic
```

Extrapolating to the 8 MiB cap: ≈ 29.5 s × (8388608/600000)² ≈ **96 minutes of CPU per response**, ×3 for the
three `collect_objects` passes.

**Why it matters:** a single malicious or merely broken upstream response wedges a worker for hours. This is
reachable from any provider whose JSON endpoint the crawler touches (kunmanga's chapter API today). It is
invisible to every existing test because all 11 tests in `json.rs` use well-formed inputs.

**Concrete remediation:**
1. Bound the work: cap total scanned braces (e.g. first 4,096 `{` positions), cap `balanced_span`'s scan
   window (a wrapped API payload is not megabytes past its opening brace), and count *attempts* against
   `MAX_CANDIDATES`, not just successes.
2. Fuzz target `fuzz_targets/adapters_json_body.rs` with `-timeout=2 -rss_limit_mb=512` (see §3, F-T2) — a
   libFuzzer timeout is exactly the oracle for this bug class.
3. Unit regression `json.rs::tests::a_body_of_unclosed_braces_is_bounded` asserting the call returns within a
   wall-clock budget on a 1 MiB `"{\"a"`-repeat input.

---

### F-03 — The frontend's 41 tests are unreachable from any CI job

**Severity: High** · **Effort: S**

**Evidence:** root `Cargo.toml:31` (`exclude = ["web/frontend"]`); `.github/workflows/ci.yml:126-139` — the
`frontend` job runs `cargo check --target wasm32-unknown-unknown` and nothing else; the `test` job runs
`cargo test --workspace --all-targets`, which cannot reach an excluded member.

41 test functions exist across 9 frontend files, and several are load-bearing:
`web/frontend/src/i18n.rs:47-66` (`locales_define_the_same_keys` — the guard against `i18nrs` rendering the
literal string `Key '…' not found`), `web/frontend/src/views/series/model.rs` (8 tests on chapter part
grouping and source ranking), `web/frontend/src/state/jwt.rs` (4), `web/frontend/src/util.rs` (9).

The same exclusion means `cargo clippy --workspace` (`ci.yml:44`) never lints the frontend either, despite
`web/frontend/Cargo.toml:67-86` carefully mirroring the workspace lint set.

**Why it matters:** an i18n key can be added to `en.json` and forgotten in `de.json` and CI is green. The
chapter part-grouping model — the subject of a whole prior redesign — has an 8-test specification that no
gate enforces.

**Concrete remediation:** add a CI job:

```yaml
frontend-test:
  name: frontend (host tests + clippy)
  runs-on: ubuntu-latest
  steps:
    - uses: actions/checkout@v4
    - uses: dtolnay/rust-toolchain@stable
    - uses: Swatinem/rust-cache@v2
      with: { workspaces: web/frontend }
    - run: cargo test --manifest-path web/frontend/Cargo.toml --lib
    - run: cargo clippy --manifest-path web/frontend/Cargo.toml --all-targets -- -D warnings
```

`--lib` on the host target works for every existing test: none touch `web-sys`/`wasm-bindgen` at runtime.
Verify by running it; if a module gates on `wasm32`, move the pure ones behind
`#[cfg(any(test, target_arch = "wasm32"))]` rather than reaching for `wasm-bindgen-test`.

---

### F-04 — `services/api/src/auth.rs` (676 LOC) has no unit tests; the token lifecycle rests on one integration test

**Severity: High** · **Effort: M**

**Evidence:** `services/api/src/auth.rs` — 0 test attributes. The only coverage is
`services/api/tests/auth_flows.rs`, which has exactly two tests, is `#![cfg(feature = "integration")]`
(line 9) and therefore runs only in the `integration` CI job.

`auth_flows.rs:51` covers the single most important path (rotation + reuse → family revocation + audit) and
covers it *well*. Everything else in the file is unexercised:

- `validate_registration` — password/username/email rules (referenced `auth.rs:98`).
- The `RegisterResponse` fork: mailer-enabled → `verification_required = true`, no session; mailer-absent →
  immediate session. Only the second branch is ever taken, because `TestApp` wires
  `tankovault_email::build(&EmailConfig::default())` (`test-support/src/lib.rs:228`), which is always disabled.
  **The email-verification branch of registration has never been executed by a test.**
- `RESET_TOKEN_TTL` / `VERIFY_TOKEN_TTL` expiry behaviour (`auth.rs:28,32`) — password reset and email
  confirmation link lifecycle entirely untested.
- Logout / cookie clearing; `SameSite`/`Secure`/`Path` cookie attributes (`REFRESH_PATH = "/v1/auth"`,
  `auth.rs:24`) — a regression that widened the cookie path would not fail anything.
- Login with a suspended / unverified account.

**Concrete remediation:** extend `services/api/tests/auth_flows.rs` with:
`register_with_a_mailer_requires_confirmation_and_issues_no_session`,
`an_unverified_account_cannot_log_in`,
`a_reset_token_past_its_ttl_is_refused`,
`a_verification_token_past_its_ttl_is_refused`,
`logout_clears_the_cookie_and_kills_the_family`,
`the_refresh_cookie_is_httponly_samesite_lax_and_scoped_to_v1_auth`.
Requires a `TestApp::spawn_with_mailer(RecordingMailer)` variant — see F-09.
Add pure unit tests for `validate_registration` in-module (they need no DB) so they run in the fast job.

---

### F-05 — `crates/db` has 6,893 LOC of SQL and 7 integration tests

**Severity: High** · **Effort: L**

**Evidence:** `crates/db/tests/repo_access_control.rs` — 7 tests, all on `permissions`, `user_admin` and one
`gdpr::cancel_own`. `catalog.rs` (1,408), `sync.rs` (944), `users.rs` (657), `scans.rs` (406),
`matching.rs` (326) have **zero** tests at any level.

The compile-time `sqlx::query!` macros plus the `sqlx prepare --check` job (`ci.yml:71-104`) guarantee the
SQL *type-checks against the schema*. They guarantee nothing about semantics: an `ON CONFLICT` clause that
updates the wrong column, a `WHERE user_id = $1` that was dropped, a `floor(c.number) >
COALESCE(last_read_whole_number, 0)` comparison inverted — all compile, all pass CI.

**Why it matters:** the four SQL-heaviest areas are exactly the ones where a silent wrong answer is worst:
GDPR export completeness (does the export actually include every table holding the subject's data?), erasure
completeness, reading-progress frontier semantics, and matching/merge candidate selection.

**Concrete remediation:** `crates/db/tests/` grows one file per repo, all under `#![cfg(feature =
"integration")]`, all using `TestDb::spawn()`:

- `repo_catalog.rs` — `series_upsert_is_idempotent_on_repeat_scan`, `chapter_upsert_preserves_read_state`,
  `search_ranks_trigram_matches_above_substring`.
- `repo_tracking.rs` — `progress_mark_read_is_monotonic`,
  `marking_a_part_of_a_read_whole_chapter_is_a_noop` (pairs with the existing pure test at
  `tracking.rs:...covers`), `unread_count_matches_the_covers_predicate` (differential test: SQL vs
  `ReadProgress::covers` over a seeded matrix — this is the single highest-value DB test in the repo).
- `repo_gdpr.rs` — `export_contains_every_table_referencing_the_subject` (drive it from
  `information_schema` so a new table with a `user_id` FK fails the test until it is added to the export),
  `erasure_leaves_no_row_referencing_the_subject`, `an_open_request_blocks_a_second_of_the_same_kind`.
- `repo_users.rs` — `create_rejects_a_duplicate_email_case_insensitively`,
  `mark_email_verified_is_idempotent`.
- `repo_scans.rs` — the run/task state machine: `a_task_cannot_move_backwards_through_states`.

---

### F-06 — `services/sync` two-way merge engine: 1,267 LOC, 2 tests on a helper

**Severity: High** · **Effort: L**

**Evidence:** `services/sync/src/engine.rs` — `#[cfg(test)]` module at line ~1210 tests only
`dedupe_latest_by_external_id`. `reconcile_account` (505), `reconcile_series` (625), `push_series_inner`
(933), `resolve_media_id` (1041), `enrich_series` (1119), `apply_metadata` (1145) are untested.

The good news: `services/sync/src/mapping.rs` has **15 tests** covering exactly the right thing — the
three-way merge decision table (`no_change_since_ancestor_is_noop`, `only_local_changed_pushes`,
`real_conflict_local_wins`, `real_conflict_newest_wins_follows_newer_side`). That decision function is
correctly isolated and well specified. `anilist.rs` has 10 tests on GraphQL response parsing, also good.

The untested part is the **orchestration**: which side's timestamp is taken as the ancestor, what happens
when the remote returns a media id that resolves to two local series, whether a failed push leaves the ledger
consistent, and whether `min_interval` throttling (`anilist.rs:756`) actually spaces requests.

**Concrete remediation:**
1. Make `SyncEngine` generic over `ExternalProvider` at the test boundary (it already goes through
   `fn provider(&self, slug) -> &dyn ExternalProvider`, `engine.rs:155` — so a `FakeProvider` is cheap) and
   add `services/sync/tests/reconcile.rs` (integration-gated, uses `TestDb`):
   `reconcile_is_idempotent_when_run_twice_with_no_remote_change`,
   `a_failed_push_does_not_advance_the_ledger_cursor`,
   `a_remote_media_id_matching_two_local_series_is_flagged_not_guessed`.
2. Rate limiter: `anilist.rs:756` sleeps on `min_interval`. Test it with
   `#[tokio::test(start_paused = true)]` + `tokio::time::advance` — the pattern already used correctly at
   `crates/service/src/health.rs:242` and `crates/service/src/shutdown.rs:97`. Assert *spacing*, not elapsed
   wall time.

---

### F-07 — Worker retry/backoff policy is untested; `services/worker/src/engine.rs` has no tests

**Severity: High** · **Effort: M**

**Evidence:** `services/worker/src/main.rs:252` (`is_retryable(&e) && deliveries < MAX_TASK_DELIVERIES`),
`:322` (`is_retryable`), `:332` (`retry_delay(deliveries)`). `main.rs` has 3 tests; none of them touch these.
`services/worker/src/engine.rs` (559 LOC — `enqueue_child`, `enqueue_children`, `content_hash`) has zero.

`services/worker/src/queue.rs` is the counter-example done right: 4 tests that pin the *fairness policy*
(`the_next_round_starts_after_the_lane_that_was_served`, `the_cursor_survives_lanes_being_added`) with
comments explaining why each property matters. That is the bar; retry does not meet it.

**Concrete remediation:** pure unit tests in `services/worker/src/main.rs`:
`retry_delay_grows_monotonically_and_is_capped`,
`retry_delay_never_returns_below_the_provider_recovery_floor`,
`a_non_retryable_error_is_dead_lettered_on_the_first_delivery`,
`the_delivery_ceiling_is_reached_before_the_ack_wait_expires` (cross-check against
`crates/bus`'s `TASK_ACK_WAIT`, which currently has the only test in that crate).
For `engine.rs`: `content_hash_is_stable_under_chapter_reordering` and
`content_hash_changes_when_a_chapter_title_changes` — this hash gates whether a scan reports "no change",
so a false-stable hash silently stops all updates for a series. Add a proptest (P-04).

---

### F-08 — GDPR export/erase correctness has no test that can fail as the schema grows

**Severity: High** · **Effort: M**

**Evidence:** `crates/db/src/repo/gdpr.rs:539 LOC`, 3 tests — all pure enum classification
(`only_unresolved_statuses_are_open`, `fulfilment_shape_matches_the_right_exercised`,
`wire_tokens_match_the_sql_enum_labels`). The SQL that actually assembles the export and performs the
erasure is untested. `services/api/src/me/privacy.rs` (319) and `services/api/src/admin/privacy.rs` (437)
are untested.

**Why it matters:** this is a legal obligation, and it is the one area where the failure mode is *silent
incompleteness*. There are 19 migrations; every new table with a `user_id` column is a new way for the
export to become incomplete without anything failing.

**Concrete remediation:** the schema-driven test named in F-05
(`export_contains_every_table_referencing_the_subject`). Query `information_schema.columns` for every table
with a `user_id` column, seed one row in each, run the export, assert every table name appears in the
exported document — with an explicit, documented allow-list for the tables that legitimately must not be
exported (e.g. `audit_log` if that is the decision). New table + no export entry + no allow-list entry =
red build. This is the only form of this test that stays correct over time.

---

### F-09 — `crates/test-support` covers one axis (DB + router) and nothing else

**Severity: Medium** · **Effort: M**

**Evidence:** `crates/test-support/src/lib.rs` (311 LOC).

What it provides, and provides well:
- `TestDb::spawn()` — a **per-test database** inside a **per-binary shared container**
  (`OnceCell<PgContainer>`, line 49; `CREATE DATABASE "tv_test_<uuid>"`, line 107; pinned to
  `postgres:17-alpine`, line 54, with a comment explaining why the testcontainers default is too old).
  Genuinely hermetic and parallel-safe. Better than `#[sqlx::test]` transaction-rollback for this codebase
  because migrations include generated columns and trigram indexes.
- `TestApp::spawn()` — the **real** `build_router` with the real middleware stack, driven via
  `tower::oneshot`. No mock HTTP layer, no route duplication.
- `RecordingAuditSink` — lets tests assert the audit trail, which `access_control.rs:74` uses properly.
- `seed_user(name, perms, status)` and `bearer(user)`.

What is missing:

| Gap | Consequence | Proposed API |
|---|---|---|
| **Recording mailer** | The whole email-verification / password-reset half of `auth.rs` is unreachable (F-04). `AppState.mailer` is hardcoded to the disabled default (line 228). | `RecordingMailer` implementing the `Mailer` trait + `TestApp::spawn_with(TestConfig { mailer, features, .. })` |
| **Feature-gate control** | `features: FeatureGate::defaults()` (line 226) is fixed, so no test can exercise a route behind a *disabled* flag — despite `flags.rs` having 13 tests on the resolution logic. | `TestConfig::features(&[(Feature::X, false)])` |
| **Entity builders** | Every DB test hand-rolls series/chapter/provider rows. `crates/db/tests` will not scale past `permissions` without them. | `SeriesBuilder`, `ChapterBuilder`, `ProviderBuilder` with sane defaults + `.build(&pool)` |
| **Deterministic clock** | TTL logic (`RESET_TOKEN_TTL`, `VERIFY_TOKEN_TTL`, refresh expiry, sync `updated_at` comparison) can only be tested by minting already-expired values. Nothing can test "valid at T, expired at T+1". | A `Clock` trait in `crates/service` with `SystemClock` / `TestClock`; `AppState` takes it. **This is the largest single unlock** — it makes token expiry, rate-limit windows, backoff and sync ancestor comparison all testable. |
| **Fake HTTP server** | `services/sync` (AniList), `services/notifier` (Discord/webhook), `services/render`, `crates/fetch` all make outbound HTTP with no way to script responses. `crates/adapters` solved this with a hand-rolled `SiteFetcher` (`madara_presets_fixture.rs:26`) — good, but per-crate and not reusable. | `wiremock` as a workspace dev-dependency + `test_support::MockUpstream` helper |
| **Seeded RNG** | `generate_refresh_token` (`crates/auth/src/token.rs`) uses `rand::thread_rng()` directly; `UserId::new()`/`Uuid::now_v7()` are nondeterministic. Fine today, blocks reproducible fuzz/property replay tomorrow. | Thread an `impl RngCore` through `generate_refresh_token`; `proptest`'s own RNG for property tests |
| **Golden-file helper** | `crates/adapters/fixtures/*` is already a golden corpus, but assertions are hand-written. No snapshot facility for the OpenAPI document, the generated client, or notifier payloads. | `insta` for `openapi.json` shape and notifier/Discord payload snapshots |

---

### F-10 — No coverage measurement, no mutation testing, no ratchet

**Severity: Medium** · **Effort: M**

**Evidence:** `.github/workflows/ci.yml` — no `llvm-cov`, `tarpaulin`, `codecov` or `cargo-mutants` step;
`grep` over all `*.yml`/`*.toml` returns zero hits. No `.config/nextest.toml`, no `rust-toolchain.toml`.

Without a coverage number there is no mechanism that notices `engine.rs` reaching 1,267 lines with 2 tests.
Without mutation testing there is no check that the tests that *do* exist would fail if the code were wrong —
which matters here because several tests assert on constants rather than behaviour
(`crates/service/src/ratelimit/redis.rs:...the_script_returns_the_three_values_the_store_unpacks` asserts
`TOKEN_BUCKET.contains("return {allowed, ...}")`, a string-match on a Lua source literal; `crates/bus`'s only
test asserts an arithmetic relation between two constants). Those are defensible pins, but they are the kind
of test mutation analysis correctly reports as non-discriminating.

---

### F-11 — Zero doc tests; no executable examples on public crate APIs

**Severity: Low** · **Effort: M**

**Evidence:** `grep -rn '/// ```' --include='*.rs' crates services xtask` → 0 matches. Note also that
`cargo test --workspace --all-targets` (`ci.yml:56`) **does not run doc tests** — `--all-targets` excludes
them. So even if doc examples were added, CI would not run them.

Ten crates are public-API-shaped (`domain`, `contracts`, `matcher`, `auth`, `solver`, `service`). Their
doc comments are prose-rich and genuinely good, but none of the invariants they describe are executable.

**Remediation:** add `cargo test --workspace --doc` as a step, and start with the three highest-value
examples: `tankovault_domain::normalize_title`, `tankovault_matcher::decide`,
`tankovault_contracts::subjects::worker_consumer_lane`.

---

### F-12 — `xtask` (458 LOC) and `services/challenge-solver` (127 LOC) have no tests

**Severity: Medium** · **Effort: S**

**Evidence:** `xtask/src/main.rs` — 0 tests. It contains `downgrade_to_3_0` (lines 246-312), a **recursive
JSON tree rewriter** with non-trivial branching over `type: [T, "null"]` arrays, `examples` → `example`, and
a "pick the first non-null type" fallback (line 276-289) that silently discards type information. Its output
feeds `progenitor`, whose output is the frontend's entire wire layer. A bug here corrupts the generated
client, and the only signal is `openapi --check` comparing two artifacts that were *both* produced by the
same buggy function.

`reset()` (line 317) drops the public schema; its `TANKOVAULT_CONFIRM_RESET` guard is untested.

**Concrete remediation:** `xtask/src/main.rs::tests`:
`downgrade_nullable_union_becomes_nullable_true`,
`downgrade_leaves_a_plain_type_untouched`,
`downgrade_of_a_multi_type_union_is_lossy_and_says_so` (pin the current behaviour so a future change is
deliberate), `downgrade_is_idempotent` (proptest P-06), `examples_array_collapses_to_first_example`.
`reset` guard: extract the env check into `fn reset_confirmed(env: Option<&str>) -> bool` and test it.

---

### F-13 — Test-quality positives worth preserving (no action)

Recorded so a future refactor does not regress them:

- **No sleeps in tests.** All 11 `sleep` call sites are production code. The two tests that involve time use
  `#[tokio::test(start_paused = true)]` correctly (`health.rs:242`, `shutdown.rs:97`).
- **No network in tests.** `crates/fetch/src/ssrf.rs` tests only `is_forbidden_ip`/`validate_url` on literal
  addresses; `resolve_checked` (DNS) is deliberately not tested. `crates/email` builds messages without
  sending.
- **No `env::set_var` / `remove_var` anywhere** — `crates/config` uses `figment::Jail` (`config.rs` tests),
  which is the correct hermetic pattern for env-driven config.
- **No shared mutable global state.** No `lazy_static`, no `static … Mutex` in test code, no `serial_test`.
  The only process-global is `test-support`'s `OnceCell<PgContainer>`, which is immutable after init and
  hands out isolated databases.
- **No wall-clock coupling.** `now_millis() > 0` (`ratelimit/redis.rs`) is the only clock assertion, and it
  is trivially stable.
- **Flakiness risk: Low.** The one real exposure is `testcontainers` (Docker availability, image pull, port
  binding) in the `integration` job. Mitigate with a retry-on-container-start and a job-level timeout, not
  with test-level retries.
- **Assertion density is high.** Median assert/unwrap-to-test ratio ≈ 2.5:1; no test in the workspace
  asserts nothing. Test names are behavioural sentences, and most carry a comment explaining *why the
  property matters* — e.g. `crates/auth/src/token.rs::the_token_carries_no_authorization_claim`, which
  decodes the JWT payload and asserts no `role`/`perms`/`scope` key exists. That is an exemplary test.

---

## 3. Fuzz and Property Testing Plan

Current state: **nothing**. No `proptest`, `quickcheck`, `arbitrary`, `libfuzzer-sys` or `cargo-fuzz` in any
manifest; no `fuzz/` directory. Two of the first three targets designed below found real bugs immediately
(F-01, F-02), which is the strongest possible argument for the rest.

### 3.1 Tool selection

| Concern | Tool | Why |
|---|---|---|
| Panic/DoS hunting on byte-oriented parsers over untrusted input (HTML, JSON-in-markup, challenge bodies) | **cargo-fuzz / libFuzzer** (nightly) | Coverage-guided; `-timeout` catches the F-02 bug class, which no property test would find. Corpus seeded from real fixtures. |
| Algebraic invariants over structured domain values (round-trips, idempotence, symmetry, monotonicity) | **proptest** (stable) | Shrinking gives minimal counterexamples; runs in the normal `cargo test` job with zero extra infra. |
| Serde round-trips over generated DTOs | **proptest** + hand-written `Strategy` (not `arbitrary`) | The contract types are few and stable; `Arbitrary` derives would add a dependency for little gain. |

No `rust-toolchain.toml` exists, so `cargo fuzz` must be invoked as `cargo +nightly fuzz`. Add a
`fuzz/rust-toolchain.toml` pinning nightly rather than pinning the whole workspace.

### 3.2 Proposed layout

```
fuzz/                                  # cargo-fuzz workspace member (excluded from the root workspace)
  Cargo.toml
  rust-toolchain.toml                  # channel = "nightly"
  fuzz_targets/
    adapters_html_parsers.rs           # F-T1
    adapters_json_body.rs              # F-T2
    adapters_generic_series_page.rs    # F-T3
    solver_challenge_detection.rs      # F-T4
    auth_jwt_verify.rs                 # F-T5
    config_env_load.rs                 # F-T6
  corpus/
    adapters_html_parsers/             # seeded from crates/adapters/fixtures/**/*.html
    adapters_json_body/                # seeded from crates/adapters/fixtures/kunmanga/chapters.json
    solver_challenge_detection/        # seeded from captured 403/429/503 bodies
  dictionaries/
    html.dict                          # <pre>, </pre>, &amp;, &lt;, cf-turnstile, cdn-cgi, "Just a moment"
    json.dict                          # {, }, [, ], ", \\, :, null, true

crates/adapters/tests/prop_html.rs     # proptest suites live next to the code, in normal cargo test
crates/matcher/tests/prop_scoring.rs
crates/domain/tests/prop_normalize.rs
crates/contracts/tests/prop_roundtrip.rs
```

`fuzz/` must be listed under the root `Cargo.toml`'s `exclude` (alongside `web/frontend`) so the stable
workspace build is unaffected.

### 3.3 Fuzz targets (cargo-fuzz, nightly)

**F-T1 — `adapters_html_parsers`** · *Priority 1 — already found F-01*

```rust
// fuzz/fuzz_targets/adapters_html_parsers.rs
fuzz_target!(|data: &str| {
    let _ = tankovault_adapters::html::parse_chapter_number(data);
    let _ = tankovault_adapters::html::parse_number(data);
    let _ = tankovault_adapters::html::parse_year(data);
    let _ = tankovault_adapters::html::parse_ymd_date(data);
    let _ = tankovault_adapters::html::map_status(data);
    let _ = tankovault_adapters::html::unescape_entities(data);
    let _ = tankovault_adapters::html::split_attr(data);
    let _ = tankovault_adapters::html::relativize("https://p.test/m/x/", data);
    let _ = tankovault_adapters::html::absolutize("https://p.test/m/x/", data);
});
```
- **Invariant:** no panic, no unbounded allocation, on *any* UTF-8 input.
- **Corpus:** anchor text and attribute values extracted from `crates/adapters/fixtures/**/*.html`.
- **Note:** these functions are `pub` but the module may need `pub use` from `lib.rs` for the fuzz crate to
  reach them; if not, add `#[doc(hidden)] pub mod __fuzz` re-exports rather than widening the real API.

**F-T2 — `adapters_json_body`** · *Priority 1 — already found F-02*

```rust
fuzz_target!(|data: &str| {
    let resp = FetchResponse { status: 200, url: "https://p.test/api".into(),
                               headers: vec![], body: data.to_owned(), from_cache: false };
    let _: Result<serde_json::Value, _> = parse_json_body("fuzz", &resp);
});
```
- **Invariants:** no panic; **completes within the libFuzzer `-timeout=2`**; peak RSS bounded
  (`-rss_limit_mb=512`). The timeout oracle is the point of this target.
- **Corpus:** `crates/adapters/fixtures/kunmanga/chapters.json`, plus that same JSON wrapped in
  `<pre>`, entity-escaped, and split across a browser-JSON-viewer DOM (the three shapes the module's own
  doc comment describes at `json.rs:1-17`).
- **Dictionary:** `json.dict` + `html.dict`.

**F-T3 — `adapters_generic_series_page`** · *Priority 2*

```rust
fuzz_target!(|data: &str| {
    let adapter = GenericConfigAdapter::new(kunmanga_preset_config());
    let _ = adapter.parse_series_page_for_fuzz("https://p.test/m/x/", data);
});
```
- **Target:** the `scraper`-driven extraction in `crates/adapters/src/generic.rs` (`parse_selector`,
  `extract_first`, `extract_all`, then `parse_chapter_number`/`parse_year`/`map_status` on the results) —
  i.e. malformed upstream HTML end-to-end against a real preset config.
- **Invariant:** returns `Ok` or a typed `AdapterError`; never panics; the returned `Vec<ChapterMeta>` is
  bounded by a documented cap.
- **Requires:** a `#[doc(hidden)]` seam that takes an HTML string rather than going through `Fetcher`. The
  `SiteFetcher` pattern in `crates/adapters/tests/madara_presets_fixture.rs:26` is already exactly this
  seam — reuse it rather than inventing another.
- **Corpus:** all four fixture sites' `catalog.html` / `series.html`.

**F-T4 — `solver_challenge_detection`** · *Priority 2*

```rust
fuzz_target!(|data: &str| {
    let _ = tankovault_solver::detect_challenge_body(data);
    let _ = tankovault_solver::is_rate_limit_page(data);
});
```
- **Invariant:** no panic (`is_rate_limit_page` at `crates/solver/src/detection.rs:96-105` does manual
  `is_char_boundary` walking — correct today, but exactly the shape of F-01 and worth pinning), and
  **linear time** in body length.
- **Additional differential invariant:** `detect_challenge_body(x).is_some()` implies
  `detect_challenge(&Resp{status: 403, server: "cloudflare", body: x}).is_some()` — the narrow classifier
  must never accept what the broad one rejects. This is a property, not a fuzz assertion; put it in
  `crates/solver/tests/prop_detection.rs`.
- **Corpus:** real Cloudflare interstitials, a real 429 notice, plus ordinary chapter pages (negative cases).

**F-T5 — `auth_jwt_verify`** · *Priority 3*

```rust
fuzz_target!(|data: &[u8]| {
    if let Ok(s) = std::str::from_utf8(data) {
        let _ = tankovault_auth::verify_access_token(b"fuzz-secret-please-rotate", s);
    }
});
```
- **Invariant:** never panics, never returns `Ok` for input not produced by `issue_access_token` with the
  same secret. `jsonwebtoken` is the actual parser so hits are unlikely — this is a cheap regression net
  around the `AccessClaims` shape (`crates/auth/src/token.rs:35-45`), particularly `user_id()`'s
  `Uuid::from_str` on an attacker-controlled `sub`.
- **Corpus:** tokens minted by `issue_access_token`, then bit-flipped.

**F-T6 — `config_env_load`** · *Priority 3*

- Target `tankovault_config::load::<AppConfig>()` under a `figment::Jail` populated from fuzz-derived
  `TANKOVAULT_*` key/value pairs (structured input via `arbitrary::Arbitrary` on
  `Vec<(String, String)>`).
- **Invariant:** returns `Ok` or `ConfigError`; never panics. `crates/config/src/lib.rs:32-36` splits on
  `__`, and `effective_port()`/`resolve()` do further parsing — a `TANKOVAULT_DATABASE__MAX_CONNECTIONS=`
  with a `u32`-overflowing value should be a typed error.

### 3.4 Property targets (proptest, stable, run in the normal `test` job)

**P-01 — `crates/adapters/tests/prop_html.rs`**

| Property | Assertion |
|---|---|
| `parse_chapter_number_never_panics` | `proptest!(|(s in ".*")| { parse_chapter_number(&s); })` — the direct stable-toolchain guard for F-01 |
| `parse_number_is_a_prefix_of_its_input` | if `parse_number(s) == Some(n)`, the decimal rendering of `n` appears in `s` (modulo trailing `.`) |
| `parse_chapter_number_agrees_with_parse_number_when_no_marker` | for `s` containing none of `chapter/episode/chap/ch./ch /#`, the two functions return the same value |
| `relativize_output_always_starts_with_slash` | `proptest!(|(page in url_strategy(), href in ".*")| assert!(relativize(&page, &href).starts_with('/')))` |
| `absolutize_is_idempotent_on_absolute_urls` | `absolutize(p, &absolutize(p, h)) == absolutize(p, h)` |
| `unescape_entities_is_a_contraction` | output length ≤ input length, always |
| `map_status_is_total` | never panics, returns a valid variant for any input |

**P-02 — `crates/domain/tests/prop_normalize.rs`**

| Property | Assertion | Rationale |
|---|---|---|
| `normalize_title_is_idempotent` | `normalize_title(&normalize_title(s)) == normalize_title(s)` | The normalized key is stored in a DB column *and* recomputed at match time; if it is not idempotent, a re-normalization migration silently orphans rows. Currently assumed, never proven. |
| `normalize_title_never_panics` | over `".*"` including combining marks and Turkish dotted I | `fold_char` runs after `to_lowercase`; F-01 shows this codebase has a blind spot there |
| `normalize_title_of_nonempty_is_nonempty` | the `NOISE_WORDS` fallback at `normalize.rs:38-40` claims this; assert it | The comment says "we never produce an empty key from a non-empty title" — that is a specification, so test it |
| `normalize_title_is_whitespace_canonical` | no leading/trailing/double spaces in output | |
| `normalize_title_is_case_insensitive` | `normalize_title(s) == normalize_title(&s.to_uppercase())` for ASCII `s` | |

**P-03 — `crates/matcher/tests/prop_scoring.rs`**

| Property | Assertion |
|---|---|
| `token_set_ratio_is_symmetric` | `token_set_ratio(a,b) == token_set_ratio(b,a)` — currently only asserted for two literal pairs (`matcher/src/lib.rs:320`) |
| `token_set_ratio_is_bounded` | result in `[0.0, 1.0]` and never `NaN`, for any input including empty and whitespace-only |
| `token_set_ratio_is_reflexive` | `token_set_ratio(a,a) == 1.0` for non-empty `a` |
| `score_is_bounded` | `score(q,c)` in `[0.0,1.0]` for arbitrary `Query`/`Candidate`, including `similarity` values outside `[0,1]` (the DB supplies it — nothing validates the range at the boundary) and `release_year` at `i32::MIN`/`MAX` (`(a-b).abs()` at `lib.rs:92` **overflows** on `i32::MIN - i32::MAX`) |
| `score_is_monotonic_in_similarity` | `s1 <= s2` implies `score(q, c_with_s1) <= score(q, c_with_s2)` |
| `decide_agrees_with_best_match` | `decide` returns `Attach(id)` iff `best_match` returns `(id, s)` with `s >= high`; they are two implementations of the same max (`lib.rs:197` and `lib.rs:206`) and can drift |
| `exact_title_equality_never_loses_to_a_non_match` | for `q.normalized_title == c.normalized_title`, `score >= token_set_ratio(...)` |

The `release_year` overflow is a concrete, likely-live defect: `(a - b).abs()` at `crates/matcher/src/lib.rs:92`
panics in debug on `i32` overflow, and `release_year` originates from `parse_year`
(`crates/adapters/src/html.rs:103`), which accepts the **full** `i32` range from provider text. **UNVERIFIED**
whether a provider can realistically emit a year near `i32::MIN`, but the parser permits it and nothing
clamps between the two. `saturating_sub` is the one-line fix.

**P-04 — `services/worker/tests/prop_content_hash.rs`**

| Property | Assertion |
|---|---|
| `content_hash_is_order_independent` | permuting `chapters` yields the same hash — *or* pin the opposite if order is deliberately significant |
| `content_hash_is_collision_resistant_on_field_changes` | changing any single `ChapterMeta` field changes the hash |
| `content_hash_is_deterministic` | same input, same hash, across processes (no `HashMap` iteration order leaking in) |

`content_hash` (`services/worker/src/engine.rs:546`) decides whether a scan reports "changed". A hash that is
accidentally stable across a real change stops all updates for a series, silently.

**P-05 — `crates/contracts/tests/prop_roundtrip.rs`**

| Property | Assertion |
|---|---|
| `scan_task_message_round_trips` | `from_slice(&to_vec(&m)?)? == m` for arbitrary `ScanTaskMessage` — generalises the single literal case at `contracts/src/messages.rs:8` |
| `user_notification_round_trips` | as above; the existing test asserts three fields, not equality |
| `subject_round_trips_through_its_lane` | `worker_consumer_lane(&worker_consumer(slug, mode)) == Some((mode, slug))` for arbitrary valid slugs — `subjects.rs` tests this for one hand-picked slug with a `-` in it; a proptest over slug shapes is the right form |
| `sync dto round-trips` | `crates/contracts/src/sync.rs` has **no tests at all**, and the project memory records three past bugs from hand-mirrored sync DTOs. Highest-value item in this group. |

**P-06 — `xtask/src/main.rs::tests` (proptest)**

| Property | Assertion |
|---|---|
| `downgrade_to_3_0_is_idempotent` | `downgrade(downgrade(v)) == downgrade(v)` over arbitrary `serde_json::Value` |
| `downgrade_preserves_every_non_type_key` | the rewriter touches only `openapi`, `type`, `examples` |
| `downgrade_never_panics_on_arbitrary_json` | including deeply nested values (guard the recursion at `main.rs:246`, which has **no depth limit**) |

**P-07 — `crates/service/tests/prop_ratelimit.rs`**

| Property | Assertion |
|---|---|
| `classify_is_total` | `RouteClassifier::classify` returns a class for any method/path, never panics |
| `longest_prefix_wins_regardless_of_insertion_order` | for an arbitrary rule set, classification is invariant under permutation of `.gate(...)` calls — `flags.rs` and `ratelimit/mod.rs` each assert this for one hand-built pair; the property is the real contract |
| `memory_bucket_never_exceeds_capacity` | over an arbitrary sequence of `(key, timestamp_delta)`, granted tokens in any window ≤ configured budget |

---

## 4. CI Gates

### 4.1 Enforced today (`.github/workflows/ci.yml`)

| Gate | Job | Line |
|---|---|---|
| `cargo fmt --all --check` | `lint` | 42 |
| `cargo clippy --workspace --all-targets -- -D warnings` (+ `RUSTFLAGS: -D warnings`) | `lint` | 44, 19 |
| `cargo test --workspace --all-targets` | `test` | 56 |
| OpenAPI artifact freshness (`xtask openapi --check`) | `test` | 64 |
| `cargo sqlx prepare --workspace --check` against a live migrated PG | `sqlx` | 104 |
| DB + HTTP integration suites (testcontainers) | `integration` | 124 |
| Frontend `cargo check --target wasm32-unknown-unknown` | `frontend` | 139 |
| `cargo-deny check advisories licenses sources bans` | `deny` | 148 |
| `cargo-audit` | `audit` | 155 |
| Docker build ×8 + container-structure-test | `docker` | 182-221 |
| Frontend Docker build + CST | `docker-frontend` | 232-260 |

This is a strong supply-chain and artifact-integrity story. Also good: a `build.rs`-installed pre-commit hook
(`xtask/hooks/pre-commit`) that regenerates OpenAPI artifacts, with the CI check as a backstop — the comment
at `ci.yml:57-62` explains exactly that layering.

### 4.2 Missing

| Gap | Impact |
|---|---|
| **Frontend tests never run** (F-03) | 41 tests dead |
| **Frontend clippy never runs** | `web/frontend/Cargo.toml:67-86` lint config is decorative in CI |
| **No coverage measurement or threshold** | nothing notices 1,267-LOC modules with 2 tests |
| **No mutation testing** | no check that tests discriminate |
| **No `cargo nextest`** | slower feedback, no flake retry policy, no per-test timeout, no JUnit output |
| **No doc-test job** | `--all-targets` excludes doctests (F-11) |
| **No fuzz smoke run** | new parser code gets no adversarial input, ever |
| **No `--no-default-features` / feature-matrix build** | the `integration` feature is only ever built *on*; a non-integration-only compile error in `crates/db`'s test cfg would not surface until someone ran the fast path locally |
| **`integration` job has `needs: [lint, test]`** | correct for cost, but it means an authz regression is reported late; acceptable |
| **No timeout on the `integration` job** | a hung testcontainers pull consumes the full 6 h runner budget |

### 4.3 Proposed gate ladder

**Rung 0 — free, do immediately (S)**
- Add the `frontend-test` job from F-03 (host `cargo test` + clippy on `web/frontend`).
- Add `cargo test --workspace --doc` to the `test` job.
- Add `timeout-minutes: 30` to `integration`, `timeout-minutes: 20` to `test`.
- Switch to `cargo nextest run --workspace --all-targets` with `.config/nextest.toml`:
  `slow-timeout = { period = "30s", terminate-after = 4 }`, `fail-fast = false`, JUnit output.

**Rung 1 — visibility, no enforcement (S/M)**
- `cargo llvm-cov nextest --workspace --lcov --output-path lcov.info`, uploaded as an artifact and posted as
  a PR comment. **Report only, no threshold** for the first four weeks — a threshold set before the baseline
  is known just gets lowered.
- Publish per-crate coverage so the F-05/F-06 gaps are visible on every PR.

**Rung 2 — ratchet (M)**
- Once a baseline exists, enforce **per-crate floors** (not one workspace number, which averages the gap
  away):
  `crates/domain`, `crates/auth`, `crates/matcher`, `crates/solver`, `crates/contracts` ≥ 80%;
  `crates/adapters`, `crates/service`, `crates/fetch`, `crates/config` ≥ 70%;
  `crates/db`, `services/api`, `services/sync`, `services/worker` ≥ 40% initially, +5 pts per quarter.
- Enforce **no-regression on the diff**: `cargo llvm-cov --fail-under-lines <baseline>` where baseline is the
  merge-base value. Blocks coverage decreasing; does not block low-coverage areas staying low.

**Rung 3 — adversarial (M)**
- **Fuzz smoke, per PR:** `cargo +nightly fuzz run <target> -- -max_total_time=60 -timeout=2 -rss_limit_mb=512`
  for each of F-T1/F-T2/F-T4, seeded from the committed corpus. 3 minutes total; catches F-01/F-02-class
  regressions on the PR that introduces them.
- **Fuzz soak, nightly `schedule:`** — 30 min per target, corpus persisted via `actions/cache`, crashes
  uploaded as artifacts and auto-filed as issues.
- **Mutation testing, weekly:** `cargo mutants --workspace --in-place --timeout 300` restricted to
  `crates/domain`, `crates/matcher`, `crates/auth`, `crates/solver` (the pure crates where it is fast and the
  signal is clean). Report only; triage the survivors as missing assertions.

**Rung 4 — build-matrix hygiene (S)**
- `cargo check --workspace --no-default-features` and `cargo check -p tankovault-db -p tankovault-api
  --features integration` as separate fast jobs, so a feature-gated compile break is caught without running
  Docker.
- Add `fuzz/` to the root `exclude` and give it its own `cargo +nightly fuzz build` check so fuzz targets
  cannot silently rot.

---

## 5. Frontend Testing (`web/frontend`)

**Does it have tests?** Yes — **41**, across 9 files. They are *good*. They just do not run anywhere (F-03).

| File | Tests | What they cover |
|---|---:|---|
| `src/util.rs` | 9 | formatting/date helpers — highest assertion density in the crate |
| `src/views/series/model.rs` | 8 | chapter part grouping, source ranking, pinned-vs-primary resolution, read-state merge |
| `src/i18n.rs` | 6 | placeholder interpolation + **locale key-set equality across `en`/`de`** |
| `src/views/notifications.rs` | 6 | notification model |
| `src/state/jwt.rs` | 4 | client-side JWT claim decoding |
| `src/models.rs` | 3 | DTO shaping |
| `src/views/console/providers.rs` | 3 | provider console model |
| `src/api/error.rs` | 2 | error mapping |

`i18n.rs::locales_define_the_same_keys` (lines 47-66) is a model example: it walks both catalogues to leaf
dot-paths and asserts set equality in both directions, with a comment explaining that `i18nrs` otherwise
renders the literal `Key '…' not found`.

### 5.1 Testable without a browser (host `cargo test`, no wasm)

Everything above already is. Expand along the same lines — all of it runs on the host target:

- **Pure model/util functions** — `util.rs`, `models.rs`, `views/**/model.rs`. The codebase already follows
  the right pattern: view state lives in a `model.rs` next to the `rsx!` component. Extend it to the views
  that lack one: `views/console/users.rs` (1,395 LOC, 0 tests), `views/discover.rs` (913),
  `views/series/tracking.rs` (701), `views/series/chapters.rs` (674). **Extracting a `model.rs` from each is
  the single highest-leverage frontend testing action** — it converts untestable render code into testable
  data transforms without a browser.
- **i18n completeness** — already done for key sets. Add:
  `every_locale_key_is_referenced_by_the_code` (grep the source for `t!("…")` call sites and diff against the
  catalogue — catches dead keys) and `placeholders_match_across_locales` (a message with `{count}` in `en`
  must have `{count}` in `de`, or interpolation silently no-ops).
- **API DTO round-trips** — `serde_json::from_str::<T>(&to_string(&x)?)? == x` over the
  `tankovault-api-client` types. Better: **contract tests against `openapi.json`** — deserialize a
  representative example of each response schema from the committed spec into the generated type. That
  catches backend/frontend drift at its actual source, which the project memory records as a recurring bug
  class.
- **Routing** — `Route` enum ↔ URL round-trip (`route.to_string().parse::<Route>() == route`) for every
  variant. Pure, no DOM.
- **JWT/claims decode** — already 4 tests in `state/jwt.rs`; add expiry-boundary and malformed-token cases.

### 5.2 Needs `wasm-bindgen-test` (headless Chrome/Firefox)

Only where the browser is genuinely the subject:
- `web-sys` `Blob`/`Url`/`HtmlAnchorElement` export download (`util::save_text_file`).
- `js-sys` ISO-8601 date parsing for relative-time rendering (behaviour differs from `time` crate parsing).
- `EventSource` SSE subscription lifecycle (`gloo-net` eventsource).
- `localStorage`/`sessionStorage` token persistence.

These are ~4 test modules, not a suite. Add `wasm-bindgen-test` as a dev-dependency and a
`wasm-pack test --headless --chrome` CI job **only after** rung 0 lands; the host tests are 90% of the value
for 10% of the infrastructure.

### 5.3 Needs Playwright / real browser (defer)

Full journeys: register → verify → login → search → track → mark-read → notification. Worth exactly one
smoke path, run nightly, against the docker-compose stack that CI already knows how to build
(`docker-frontend` job). **Do not** build a Playwright suite before the host-test and model-extraction work
above — it would be slower, flakier, and would test the same logic through six more layers.

---

## 6. Testing Roadmap

### Phase 0 — Stop the bleeding (1 week, S)
1. **Fix F-01** (`parse_chapter_number` panic) + regression test. *Ship first — it is a live crash.*
2. **Fix F-02** (quadratic `collect_objects`) + bounded-input regression test.
3. **Add the `frontend-test` CI job** (F-03) — 41 tests go from dead to enforced for the cost of 8 YAML lines.
4. Add `cargo test --workspace --doc`, job timeouts, and `cargo-nextest`.
5. Add `cargo check --workspace --no-default-features` and the `--features integration` check job.

### Phase 1 — Property testing on stable (2 weeks, M)
6. Add `proptest` as a workspace dev-dependency.
7. Land **P-01** (`prop_html`), **P-02** (`prop_normalize`), **P-03** (`prop_scoring`) — the three suites
   guarding the parsers that already produced bugs. Expect the `release_year` overflow (P-03) to fire.
8. Land **P-05** (`prop_roundtrip`), prioritising `crates/contracts/src/sync.rs`, which has zero tests and a
   documented history of drift bugs.
9. Land **P-06** (`xtask` downgrade idempotence) — cheap, and it guards the generated wire layer.

### Phase 2 — Harness capability (2-3 weeks, M/L)
10. **Deterministic `Clock`** in `crates/service`, threaded through `AppState` and `SyncEngine`. Unlocks every
    TTL, backoff, rate-limit-window and sync-ancestor test that is currently impossible.
11. `RecordingMailer` + `TestApp::spawn_with(TestConfig)` (mailer, feature overrides, clock).
12. Entity builders (`SeriesBuilder`, `ChapterBuilder`, `ProviderBuilder`) in `crates/test-support`.
13. `wiremock` workspace dev-dependency + `MockUpstream` helper for `services/sync`, `services/notifier`,
    `crates/fetch`.

### Phase 3 — Fill the critical gaps (4-6 weeks, L) — *depends on Phase 2*
14. **F-04**: the six auth-lifecycle tests, including the never-executed email-verification branch.
15. **F-08 + F-05**: `repo_gdpr.rs` with the schema-driven export-completeness test, then `repo_tracking.rs`
    (including the SQL-vs-`covers` differential test), `repo_catalog.rs`, `repo_users.rs`, `repo_scans.rs`.
16. **F-06**: `services/sync/tests/reconcile.rs` against a `FakeProvider` + `TestDb`; paused-clock test for
    the AniList `min_interval` throttle.
17. **F-07**: worker retry/backoff unit tests + **P-04** (`content_hash` properties).
18. **F-12**: `xtask` unit tests.

### Phase 4 — Fuzzing (2 weeks, M) — *can run parallel to Phase 3*
19. Stand up `fuzz/` with **F-T1**, **F-T2**, **F-T4**; seed corpora from `crates/adapters/fixtures/`.
20. Add the 60-second-per-target PR fuzz smoke gate (Rung 3).
21. Add the nightly 30-minute soak with corpus caching and crash-artifact upload.
22. Add **F-T3**, **F-T5**, **F-T6** once the first three are stable.

### Phase 5 — Ratchet and measure (ongoing)
23. Coverage reporting (report-only) → four-week baseline → per-crate floors → diff-no-regression gate.
24. Weekly `cargo mutants` on the pure crates; triage survivors as missing assertions.
25. Frontend: extract `model.rs` from the four large untested views, then a small
    `wasm-bindgen-test` module for the genuinely browser-bound helpers.
26. One nightly Playwright smoke journey against the docker-compose stack. Last, not first.

---

## Appendix — Verification notes

- F-01 and F-02 were reproduced by extracting the exact functions from
  `crates/adapters/src/html.rs` and `crates/adapters/src/json.rs` into standalone programs compiled with
  `rustc -O` and executed. Outputs are quoted verbatim in the findings. No project file was modified.
- Test counts are `grep -cE '#\[(tokio::test|test|sqlx::test|wasm_bindgen_test)'` occurrences per file,
  aggregated per crate. Accuracy ±1 per crate: `crates/test-support` initially matched on a doc comment
  containing the literal `` `#[sqlx::test]` `` (`lib.rs:15`) and has been corrected to 0.
- LOC counts are raw line counts of `*.rs` files excluding `target/`, so they include comments and blank
  lines. This codebase is heavily commented, so *effective* code density is lower than the table suggests —
  which makes the tests/kLOC figures **optimistic**, not pessimistic.
- The `release_year` overflow noted in P-03 is a code-reading finding, not executed. Marked **UNVERIFIED**
  as to real-world reachability; the arithmetic itself (`(a - b).abs()` on `i32` at
  `crates/matcher/src/lib.rs:92`) is plainly overflow-capable.
</content>
</invoke>
