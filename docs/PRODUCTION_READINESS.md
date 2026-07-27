# TankoVault Production-Readiness Analysis & Roadmap

Status: living document. Authoritative analysis of the codebase's production posture and the
prioritized roadmap toward a stable, well-maintained, secure, performant, production-ready
system. Cross-references `docs/design.md` §16 (authorization model) and §21 (Definition of
Done). Companion to the delivery plan tracked under `.junie/plans/`.

## 1. Executive summary

TankoVault is an unusually mature Rust workspace. Its authorization design is the strongest
part of the system: capabilities are resolved **fresh from the database on every request**
(`services/api/src/state.rs::AuthUser`), not read out of the access token, so a revoked grant
takes effect immediately. Denials are audited, rate limiting and an SSRF guard are in place,
passwords use Argon2id with a server-side pepper, refresh tokens are hashed and rotated with
reuse detection, and CI already runs `fmt` + `clippy -D warnings` + `cargo-deny` +
`cargo-audit` + an sqlx offline-cache check against a real Postgres + a wasm check + a Docker
build matrix.

The single biggest gap is **automated verification of behavior**, and it is most acute exactly
where correctness matters most — access control. The API service has only pure-logic unit
tests; there is not one test that boots the router and asserts a privileged endpoint returns
`401`/`403`/`2xx` for the right caller. The `db` repository layer has almost no DB-backed
tests despite design DoD §21 explicitly promising "a Postgres-backed repo test via sqlx's test
harness." CI's `test` job runs with `SQLX_OFFLINE=true` and no database, so those tests could
not run even if they existed. Authorization correctness is therefore proven today only by a
**manual** end-to-end smoke script.

This document records the current strengths in detail, the prioritized gap list, the target
test pyramid, the access-control test matrix, and a phased action plan.

## 2. Current strengths

- **Per-request capability resolution.** `AuthUser::from_request_parts` verifies the JWT,
  then calls `tankovault_db::repo::permissions::resolve` to load a fresh `PermissionSet` and
  account status for every request. A grant revoked a second ago is already gone. See
  `services/api/src/state.rs` and the rationale in `crates/db/src/repo/permissions.rs`.
- **No role tiers.** Authorization is a set of fine-grained, non-implying capabilities
  (`crates/domain/src/permissions.rs`). `users.write` does not imply `users.read`; endpoints
  that need two capabilities ask for both via `require_all`.
- **Audited denials.** A refused `require`/`require_all` records an `authz.denied` audit event
  naming every missing capability, attributed to the principal and request origin.
- **Suspension is enforced before capabilities.** A suspended account is rejected outright
  (`ApiError::Suspended`) in the extractor, because "no permissions" is not the same as
  "suspended" — an account with no grants can still read its own watchlist.
- **Last-holder guard rails.** `permissions::other_active_holders` prevents revoking or
  suspending the last active holder of a critical capability (notably `users.permissions`),
  so the console cannot lock everyone out.
- **Credential hardening.** Argon2id with a server-side pepper (`crates/auth`), hashed
  rotating refresh tokens with reuse-detection family revocation.
- **Edge hardening.** Rate limiting (Redis-backed across replicas, in-memory fallback), CORS
  allowlist, body cap, request timeouts, security headers, feature-flag enforcement — all
  assembled in `tankovault_api::build_router`.
- **SSRF guard** in the `fetch` crate; **GDPR** data-subject endpoints; graceful degradation
  (NATS/Redis/email outages degrade features rather than aborting boot).
- **Mature CI.** `fmt`, `clippy -D warnings` (pedantic), `cargo-deny`, `cargo-audit`, an sqlx
  offline-cache check against a real Postgres, a wasm build check, and a Docker build matrix.

## 3. Prioritized gaps

| # | Gap | Severity | Evidence |
|---|-----|----------|----------|
| 1 | No HTTP-level access-control tests | Critical | `services/api/src` has only pure-logic unit tests; authorization wiring is proven only by the manual smoke script. |
| 2 | Near-absent DB-backed repo tests | Critical | `crates/db/src/repo/*` contains only pure-logic tests; the guard-rail SQL (`resolve`, `other_active_holders`, `set_status`, `cancel_own`) is untested. |
| 3 | CI runs tests with no database | High | `.github/workflows/ci.yml` `test` job sets `SQLX_OFFLINE=true` and provides no Postgres, so DB/HTTP tests cannot run. |
| 4 | No fail-fast secret validation | High | `services/api/src/main.rs` moves `jwt_secret` into `AppState` with no presence/strength check; `password_pepper` defaults to empty. |
| 5 | Stale authorization docs | Medium | `docs/design.md` §16 still describes the removed `user`/`operator`/`admin` RBAC model, contradicting the implemented per-capability model. |
| 6 | Dependency-update automation absent | Medium | `cargo-audit`/`cargo-deny` gate CI, but there is no Dependabot/renovate to keep the lockfile current. |
| 7 | Resilience/degradation unverified | Low | Degradation paths (NATS-unreachable SSE `503`, DB pool exhaustion) exist but have no automated coverage. |

## 4. Target test pyramid

```
        /\        HTTP integration (services/api/tests)
       /  \       real router via tower::oneshot against ephemeral Postgres:
      /    \      401 / 403 / 2xx per gated route, suspension, ownership, audit-on-deny
     /------\
    /        \    Repo-layer #[sqlx::test] (crates/db/src/repo)
   /          \   guard-rail SQL against a migrated schema:
  /            \  resolve, other_active_holders, set_status, cancel_own
 /--------------\
/                \ Pure-logic unit tests (existing) — fast, DB-free, always on
```

- **Pure-logic unit tests** stay the fast default; `cargo test --workspace` remains DB-free.
- **Repo-layer tests** pin the SQL guard rails directly.
- **HTTP integration tests** exercise the real extractor, middleware and authorization wiring
  end-to-end, so a wiring regression (e.g. a route mounted outside the auth layer) fails too.

Both integration layers run against an ephemeral Postgres stood up by **testcontainers**, via
a shared `crates/test-support` harness that owns container lifecycle, migration, seeding and
token minting. DB-backed suites are opt-in (feature-gated) so contributors without Docker keep
a fast, green unit path.

## 5. Access-control test matrix

For every permission-gated route, three cases are asserted: `401` with no/invalid token,
`403` (plus an `authz.denied` audit event) for a valid token lacking the capability, and
`2xx` for a token holding it. Suspended-account rejection and `/me/*` ownership are asserted
through real requests.

| Route (method) | Required capability | Handler |
|----------------|---------------------|---------|
| `GET /v1/admin/flags` | `flags.read` | `admin/flags.rs` |
| `PUT /v1/admin/flags/{key}` | `flags.write` | `admin/flags.rs` |
| `GET /v1/admin/merge-candidates` | `merge.read` | `admin/merge.rs` |
| `POST /v1/admin/merge-candidates/merge` | `merge.write` | `admin/merge.rs` |
| `POST /v1/admin/merge-candidates/dismiss` | `merge.write` | `admin/merge.rs` |
| `GET /v1/admin/privacy/requests` | `privacy.read` | `admin/privacy.rs` |
| `POST /v1/admin/privacy/requests/{id}/claim` | `privacy.write` | `admin/privacy.rs` |
| `POST /v1/admin/privacy/requests/{id}/resolve` | `privacy.write` | `admin/privacy.rs` |
| `POST /v1/admin/privacy/requests/{id}/extend` | `privacy.write` | `admin/privacy.rs` |
| provider read/list | `providers.read` | `admin/providers.rs` |
| provider create | `providers.create` | `admin/providers.rs` |
| provider edit | `providers.write` | `admin/providers.rs` |
| provider delete | `providers.delete` | `admin/providers.rs` |
| provider enable/disable | `providers.state` | `admin/providers.rs` |
| provider live test | `providers.test` | `admin/providers.rs` |
| scans read/list | `scans.read` | `admin/scans.rs` |
| trigger scan | `scans.run` | `admin/scans.rs` |
| sync admin read | `sync.admin.read` | `admin/sync.rs` |
| sync admin write | `sync.admin.write` | `admin/sync.rs` |
| user directory/detail | `users.read` | `admin/users.rs` |
| user identity edit / status | `users.write` | `admin/users.rs` |
| grant/revoke permissions | `users.permissions` | `admin/users.rs` |
| erase user | `users.delete` | `admin/users.rs` |
| revoke sessions | `users.sessions` | `admin/users.rs` |
| system stats | `system.stats` | `admin/system.rs` |
| audit read | `audit.read` | `admin/system.rs` |

The list is derived mechanically from the `user.require(Permission::…)` /
`require_all(&[…])` calls; when a new gated route is added, its row is added here and a test
case with it.

## 6. Guard-rail SQL under test (repo layer)

- `permissions::resolve` — exact live grant set + account status, including the suspended path
  (`AccountStatus::may_authenticate()`). A grant revoked between token issuance and request
  must take effect on the next resolution.
- `permissions::grant` / `replace` / `other_active_holders` — the last active holder of a
  critical capability (`users.permissions`) cannot be removed.
- `user_admin::set_status` — suspend/reinstate, including the last-admin protection: the last
  active holder cannot be suspended.
- `gdpr::cancel_own` — ownership scoping: a user cannot cancel another user's request.

## 7. Security hardening

- **Fail-fast secret validation.** Under a production profile, boot is rejected when
  `jwt_secret` is missing or below a minimum length; an empty `password_pepper` is warned.
  Scoped to production so dev/test boot (and the harness's generated secrets) is unaffected.
- **CSRF posture.** The cookie-based `/auth/refresh` flow relies on `SameSite=Strict` and a
  scoped cookie path; this is covered by an integration test so a regression is caught.
- **CSP headers** on the web edge are confirmed present.
- **No secret/token/PII in logs or URLs** — reaffirmed as an invariant during hardening.
- **Doc reconciliation.** `docs/design.md` §16 is rewritten to describe the implemented
  per-capability model and to document secret/pepper rotation guidance.

## 8. Phased action plan

1. **This document** — analysis and roadmap (done as part of this change).
2. **Shared harness** — `crates/test-support`: testcontainers Postgres, `sqlx migrate run`,
   seed builders, `mint_access_token`, `TestApp` (`AppState` + `build_router` + `oneshot`),
   and a `RecordingAuditSink` test double.
3. **Repo-layer tests** — `#[sqlx::test]` for the guard-rail SQL (§6).
4. **HTTP-layer tests** — the access-control matrix (§5), suspension, ownership, audit-on-deny,
   plus auth-flow tests (refresh rotation + reuse detection, reset/verify TTL expiry).
5. **Security hardening** — fail-fast secret validation, CSRF/CSP verification, §16 doc rewrite.
6. **CI & supply chain** — a Docker-enabled integration job, dependency-update automation, and
   resilience/observability checks; keep the fast offline `test` job as the default path.

## 9. Non-functional guarantees

- The default `cargo test --workspace` unit path stays fast and DB-free.
- Integration tests are hermetic and parallel-safe: an isolated database per test, no shared
  mutable fixtures.
- All new code builds under `deny(warnings)` and passes `clippy` pedantic, consistent with the
  existing Definition of Done (§21).
