# TankoVault production readiness — closed

**Status: closed 2026-08-05.** This was a living analysis-and-roadmap document. It is now a
record of one, kept at this path because [`README.md`](../README.md) and
[`docs/audit/README.md`](audit/README.md) link here.

It is closed for two reasons, and the second is the interesting one. Everything it scheduled
has shipped. And every load-bearing claim it made about the tree was false — an API service
with "only pure-logic unit tests", a `db` layer with "almost no DB-backed tests", a CI `test`
job under `SQLX_OFFLINE=true` where "those tests could not run even if they existed".

Not eventually false: false on arrival. Commit `8df8a2c` added this file, and the same commit
added `crates/test-support`, `crates/db/tests/repo_access_control.rs`,
`services/api/tests/{access_control,auth_flows,resilience}.rs`, the fail-fast secret validation
in `services/api/src/main.rs`, and the `integration` job in `.github/workflows/ci.yml`
(`git show 8df8a2c --stat`). It was written as a plan, committed alongside the work that
completed it, and never re-read. Nine days and 122 commits later the gap was 4,083 lines of API
tests and 8,269 lines of repo tests wide.

That is the failure mode [`ENGINEERING_GUIDE.md`](ENGINEERING_GUIDE.md) is organised around:
*where the codebase records a decision in prose, what would notice when the prose stops being
true?* This document had no answer, so it is retired rather than refreshed — its content has
moved to places that do.

## Where to look instead

| For | Read | What notices when it goes stale |
| --- | --- | --- |
| live status of every finding | [`docs/audit/PROGRESS.md`](audit/PROGRESS.md) | the hand-off convention: a row is updated in the same commit as its fix |
| every rule and what enforces it | [`ENGINEERING_GUIDE.md`](ENGINEERING_GUIDE.md) §5 | each row names a command; `xtask ci` runs the offline set |
| the authorization model | [`design.md`](design.md) §16 | `me_access_matrix.rs` / `admin_access_matrix.rs` reconcile it against `openapi.json` |
| what is built and what is next | [`IMPLEMENTATION_STATUS.md`](IMPLEMENTATION_STATUS.md) | nothing — it is a session log, and it lags |

## What it planned, and what delivered it

Its §8 phased plan is complete. The phases, and the artefacts that closed them:

| Phase | Delivered by |
| --- | --- |
| 2 — shared integration harness | `crates/test-support` (testcontainers Postgres, migration, `seed::*` builders) and `services/api/test-support` (`TestApp`: `AppState` + `build_router` + `oneshot`, token minting, recording audit sink) |
| 3 — repo-layer guard-rail SQL | `crates/db/tests/` — 12 files, 8,269 lines. Its §6 list is `repo_access_control.rs`: `resolve` live grants and suspension, `other_active_holders` last-holder protection, `set_status` round trip, `cancel_own` ownership scoping |
| 4 — HTTP access-control matrix | `services/api/tests/` — 12 files, 4,083 lines. `admin_access_matrix.rs` (55 gates) and `me_access_matrix.rs` (22 gates) drive every route anonymous / holding-every-permission-but-one / holding exactly it; `auth_lifecycle.rs` and `auth_flows.rs` cover refresh rotation, reuse detection and token expiry |
| 5 — security hardening | `services/api/src/main.rs` refuses to boot on an empty, short or known-placeholder `jwt_secret` under a production profile; [`design.md`](design.md) §16 is rewritten to the per-capability model and carries the rotation guidance |
| 6 — CI and supply chain | the `integration` job in [`.github/workflows/ci.yml`](../.github/workflows/ci.yml) runs `cargo test -p tankovault-db -p tankovault-api -p tankovault-sync --features integration`; `renovate.json` keeps the lockfiles current; `resilience.rs` pins the probes and the bus-down `503` |

Its §3 gap list is closed the same way — all seven rows, including the two it rated Critical.
The strengths in its §2 are still true and now live in [`design.md`](design.md) §16 and
[`ENGINEERING_GUIDE.md`](ENGINEERING_GUIDE.md) §2, next to what enforces each of them.

## The one thing worth carrying forward

Its §5 was a hand-maintained table of every permission-gated route, with the instruction *"when
a new gated route is added, its row is added here and a test case with it."* That instruction
had already failed: the table's first row is `GET /v1/admin/flags`, and no such path is
published — `openapi.json` has `/v1/admin/feature-flags`. The table carried 26 rows; the matrix
that replaced it carries 55 admin gates over 47 published admin paths.

The replacement does not ask anyone to remember. `admin_access_matrix.rs` and
`me_access_matrix.rs` hold the gates in code and **reconcile them against the published OpenAPI
document**, so an endpoint nobody classified fails with its operation id in the message; the
route has to gain a row in `me_gates()`, `public_gates()` or `covered_elsewhere()` — the last
carrying the reason and where it is covered instead. Both need Docker, so no offline gate
mentions them, which is why [`CLAUDE.md`](../CLAUDE.md) does.

That is the general lesson, and it is why this file is a stub: a list of routes maintained by
prose drifts silently, and the same list maintained by a test that reads `openapi.json` cannot.

## Still open

One row across the whole audit that a commit cannot close: **SEC-2b**, renderer DNS rebinding,
which needs container-level egress restriction. See [`audit/PROGRESS.md`](audit/PROGRESS.md) for
its current state and for the operator actions alongside it. Nothing from this document's own
gap list remains.
