# Operations & Hardening Reference

Every backend service shares one runtime, `crates/service` (`tankovault-service`). This
document is the operator-facing reference for what it provides and how to configure it.

Configuration is layered (defaults → `$TANKOVAULT_CONFIG` TOML → `TANKOVAULT_*` env), so
every key below has an environment form: nest with `__`, e.g. `rate_limit.auth.per_minute`
→ `TANKOVAULT_RATE_LIMIT__AUTH__PER_MINUTE`.

---

## 1. Operational endpoints

Mounted **outside** the middleware stack, so a rate limit or a body cap can never make a
healthy replica look unhealthy to its orchestrator.

| Path | Meaning |
|---|---|
| `GET /health` | **Liveness.** The process is up and its executor is scheduling. Checks nothing external — a failing dependency must not trigger a restart loop that deepens the outage. |
| `GET /ready` | **Readiness.** Probes every registered dependency concurrently, each bounded at 2s. `200` when all answer; `503` with a per-dependency JSON body otherwise. |
| `GET /metrics` | Prometheus exposition, or `404` when metrics are disabled. |

`/ready` body when something is down:

```json
{"status":"down","checks":[{"name":"postgres","status":"up"},
                           {"name":"nats","status":"down","detail":"connection refused"}]}
```

Per-service readiness dependencies:

| Service | Probes | Rationale |
|---|---|---|
| `api` | postgres | NATS absence only disables `/v1/me/stream`, which already degrades to `503`. |
| `control-plane` | postgres, nats | Cannot plan a run without one or publish it without the other. |
| `worker` | postgres, nats | Nothing to consume without NATS; nowhere to record it without Postgres. |
| `notifier` | postgres, nats | Same. |
| `sync` | postgres | A third-party provider outage is expected and degrades per-request; probing it would flap readiness on someone else's uptime. |
| `render`, `challenge-solver` | *(none)* | Stateless. The browser is launched lazily by design, so probing it would report a healthy replica as down until its first render. |

The `worker` has no HTTP contract of its own but now serves this listener anyway
(`TANKOVAULT_BIND_ADDR`, default `0.0.0.0:8085`) — previously a wedged worker was invisible.

---

## 2. Toggles

Each toggle is a **wiring** decision resolved once at boot, never an `if` at a call site.
These are deployment settings, distinct from the runtime **feature flags** in §4 — see the
comparison there for which mechanism a given switch belongs in.

### `metrics`

```toml
[metrics]
enabled       = true       # false → no recorder is installed at all
route         = "/metrics"
http_requests = true       # per-request counter/histogram/gauge
```

`enabled = false` means the process-wide Prometheus recorder is never installed, so
`metrics::counter!` calls throughout the workspace dispatch to the crate's default dropping
recorder and retain nothing. Domain code is unchanged either way. The scrape route answers
`404` rather than an empty body, so a misconfigured scrape target fails loudly in Prometheus
instead of drawing a silently flat graph.

`http_requests` is separate because the request histogram is the expensive part: a service
can keep cheap domain counters while dropping per-route cardinality.

Emitted metrics:

| Metric | Type | Labels |
|---|---|---|
| `http_requests_total` | counter | `method`, `route`, `status` |
| `http_request_duration_seconds` | histogram | `method`, `route` |
| `http_requests_in_flight` | gauge | — |
| `http_rate_limited_total` | counter | `class` |
| `rate_limit_store_errors_total` | counter | `backend` |

`route` is axum's **matched path** (`/v1/series/{id}`), never the concrete URI — an
unrouted path is attacker-controlled and would otherwise be an unbounded label source, so
those are folded into a single `unmatched` label.

### `audit`

```toml
[audit]
enabled              = true
record_ip            = false   # personal data (GDPR Art. 4(1)) — opt in deliberately
record_user_agent    = false
retention_days       = 365     # 0 disables the sweep and keeps records forever
sweep_interval_hours = 24
```

`enabled = false` installs `NoopAuditSink` behind the same trait object. It is deliberately
silent: logging each dropped event would recreate the audit trail in the log stream and
defeat the decision to switch auditing off.

The privacy toggles are enforced **in the sink**, at the single point where data is
persisted, so no handler can retain an IP by constructing its event differently.

Recorded actions include `authz.denied`, `auth.login` (every outcome), `auth.refresh`
(token-reuse detection), `account.export`, `account.delete`, and every privileged provider,
scan, merge and sync mutation.

### `rate_limit`

```toml
[rate_limit]
enabled              = true
backend              = "memory"   # or "redis"
trust_forwarded_for  = false

[rate_limit.global]    # anything without a stricter class
per_minute = 300
burst      = 60
[rate_limit.auth]      # login, register, reset, refresh — the guessing control
per_minute = 10
burst      = 5
[rate_limit.expensive] # data export, scan triggers, sync push/pull
per_minute = 6
burst      = 2
```

`per_minute` is the bucket's **refill rate**; `burst` is its **depth**. A burst below the
sustained rate is the normal case: 300/min with a 60-deep bucket absorbs a page load without
letting one client spend a whole minute's budget instantly.

`enabled = false` leaves the layer unmounted entirely — no per-request cost, and no
`X-RateLimit-*` headers.

**Backends.** `memory` is process-local: correct for one replica, but with `N` replicas the
effective limit is `N` times the configured one. `redis` holds one token bucket per
`(class, client)` in a Lua check-and-consume, so the limit means what it says across a
fleet. The Redis backend **fails open** — a counter-store outage must not take the edge
down — and logs plus counts every such failure.

> **`trust_forwarded_for` is a security setting.** Enable it *only* behind a reverse proxy
> that overwrites `X-Forwarded-For`/`X-Real-IP`. With it on and no such proxy, any client
> can forge a fresh identity per request and bypass the limiter entirely.

Buckets are keyed by client IP, deliberately not by anything the client supplies — keying
on a bearer token would let an attacker mint a fresh bucket per request by sending a
different (even invalid) one. A service that has already *verified* a principal in an outer
layer may insert a `ratelimit::Principal` request extension, which the limiter prefers.

### `security`

```toml
[security]
max_body_bytes       = 1048576   # 1 MiB
request_timeout_secs = 30
security_headers     = true
hsts                 = false     # enable only where the edge terminates TLS
hsts_max_age_secs    = 63072000
trust_request_id     = false

[security.cors]
allowed_origins   = []           # empty = same-origin only
allow_credentials = false
max_age_secs      = 3600
```

The CORS default is an **empty allowlist**, which rejects every cross-origin request. This
replaces `CorsLayer::permissive()`, which reflected any origin and allowed any method and
header — on an API serving authenticated user data, that let any site on the internet read a
signed-in user's watchlist, progress and account settings. The reference deployment serves
the frontend and API from one nginx origin, so no CORS hop exists; a split-origin deployment
must name its origins explicitly.

Always-on response headers when `security_headers = true`: `X-Content-Type-Options: nosniff`,
`X-Frame-Options: DENY`, `Referrer-Policy: no-referrer`,
`Cross-Origin-Resource-Policy: same-origin`.

### Middleware order

Fixed in `HttpStack` so no service can get it subtly wrong. Outermost first:

```
request-id → tracing → metrics → security headers → CORS → rate limit
           → timeout → body cap → compression → handler
```

Security headers and CORS sit *above* the rate limiter so they are applied to its `429` and
to the timeout's `408`, not only to handler responses.

---

## 3. Authorization: permissions, not roles

Authorization is **per-capability**. There is no role, no tier, and no ordering: a principal
holds a set of named [`Permission`](../crates/domain/src/permissions.rs) grants, each endpoint
asks for the specific thing it does, and permissions never imply one another —
`users.write` does not confer `users.read`.

The previous `user < operator < admin` tier was removed rather than extended. It could not
express least privilege (letting someone triage the merge queue also handed them provider
editing, scan triggering and every user's linked-account state), and it made the requirement
invisible in the model — `at_least(Operator)` says how privileged a caller must be, not what
they are allowed to do.

**Grants are resolved from the database on every authenticated request**, not read out of the
access token. The token carries identity only. That is a deliberate cost: one indexed lookup
per request buys immediate revocation, which a claim baked into a 15-minute token cannot
offer at any price.

| Concept | Where |
|---|---|
| The capability registry (24 permissions, their groups and descriptions) | `tankovault_domain::Permission` |
| Storage | `user_permissions` (one row per user × capability, with `granted_by` provenance) |
| Resolution | `tankovault_db::repo::permissions::resolve` |
| Enforcement | `AuthUser::require` / `require_all` — a refusal is audited as `authz.denied` |
| What a caller may do | `GET /v1/me/capabilities` (also carries the enabled feature list) |
| Catalogue + preset bundles for the admin UI | `GET /v1/admin/permissions` |

**Presets are not roles.** The console offers Reader / Operator / Administrator bundles as a
starting point; applying one expands to a checklist the administrator then edits, and only the
resulting set is stored. Nothing in the database or in an authorization decision knows presets
exist.

### Account status

Suspension is *not* modelled as the absence of every permission — an account with no grants can
still read its own watchlist, which is not what suspending it means. `users.status` is an
identity-level state checked **before** authorization: a suspended account cannot sign in,
cannot refresh, and is refused at the extractor with `403 account_suspended`.

### Guard rails that live in code, not in the schema

Three refusals are properties of the deployment that no constraint can express, enforced in
`services/api/src/admin/users.rs`:

1. **No self-administration.** Suspension, erasure and permission edits refuse to target the
   caller — `/v1/me` is where someone acts on their own account, and an administrator who can
   quietly grant themselves a capability produces a trail nobody can rely on.
2. **The last administrator is protected.** Revoking, suspending or erasing the final *active*
   holder of `users.permissions` is refused; it would leave no way to grant anything again,
   recoverable only by editing the database by hand.
3. **Erasure demands the username back**, typed by the administrator.

All three refusals are audited, not just the successes.

---

## 4. Feature flags — the control plane

Every product capability is switchable at runtime from the console. Flags are a **different
mechanism from the toggles in §2** and the distinction matters:

|  | Toggles (§2) | Feature flags |
|---|---|---|
| Resolved | Once, at boot | Per request, from a cached snapshot |
| Changed by | Redeploy | `PUT /v1/admin/feature-flags/{key}` |
| Scope | This process | The whole deployment |

What keeps a runtime flag from becoming `if flag { }` scattered through handlers is that the
check is **declarative**. `route_features()` in `services/api/src/lib.rs` is a table, sitting
next to the route registration, mapping route-pattern prefixes to features; one middleware
(`tankovault_service::flags::enforce`) reads it. Handlers contain no flag logic. Background
loops, which have no route to declare against, check their own flag at the top of each
iteration — there the loop *is* the feature.

- **Registry:** `tankovault_domain::Feature` — 37 features in 8 groups, each with a compiled
  default and an operator-facing description of what switching it off does.
- **Storage:** `feature_flag_overrides` holds *only* deviations from the shipped defaults. An
  empty table is a fully working deployment; a feature added in code appears in the console at
  its default with no migration and no seed row.
- **Three states, not two:** a feature is on or off, and *separately* at its default or
  explicitly overridden. `PUT` records a decision (which pins it against a future change of the
  default); `DELETE` withdraws it.

```bash
curl -X PUT $API/v1/admin/feature-flags/notifications.email \
  -H "authorization: Bearer $TOKEN" -H 'content-type: application/json' \
  -d '{"enabled":false,"note":"SMTP relay is bouncing"}'
```

**Propagation.** The replica serving the write refreshes before responding, so the operator's
next request already behaves the new way. Other replicas converge within
`features.refresh_secs` (default 15). A refresh that cannot reach the database keeps the
previous snapshot rather than falling back to defaults — a database blip must not silently
re-enable something an operator switched off.

**Response when a feature is off:** `404` with an RFC 9457 body naming the feature, so the
answer to "why is this 404ing" is in the response rather than only on the flag page.

```json
{"type":"about:blank#feature_disabled","title":"feature_disabled","status":404,
 "detail":"the \"Watchlist\" feature is switched off on this deployment",
 "feature":"tracking.watchlist"}
```

**Two features are locked and refuse to be switched off:** `admin.feature_flags` and
`admin.users`. Disabling the flag surface removes the only way to turn anything back on;
disabling user administration removes the only way to grant the permission that reaches it.
Either would brick the deployment from the operator's side. The API rejects the write *and* the
runtime ignores a stored override for them, so a hand-edited database cannot do it either.

**Two ship off by default** — `notifications.webhook` and `notifications.discord` — because
they send data to third parties from configuration the installer has not necessarily reviewed.
Everything else ships on: flags exist so an operator can narrow a working deployment, not so a
fresh install arrives inert.

---

## 5. GDPR data-subject endpoints

### Self-service

| Endpoint | Right | Feature | Notes |
|---|---|---|---|
| `GET /v1/me/export` | Art. 20, portability | `privacy.self_export` | One JSON document assembled in a single query, so the export is a consistent snapshot. Served as an attachment. |
| `DELETE /v1/me` | Art. 17, erasure | `privacy.self_erasure` | Requires the caller's own username echoed back in `{"confirm_username": "..."}`. Closes the caller's own open requests first, so the compliance record shows the erasure was carried out. |

Both act only on the authenticated principal; there is no operator override on these paths.
Both draw from the `expensive` rate-limit class.

### The request queue

The self-service endpoints answer the two rights people actually exercise. They cannot satisfy
the rest of Chapter III, so a tracked queue sits alongside them (`gdpr_requests`, feature
`privacy.requests`):

- **Art. 16/18/21** — rectification, restriction and objection have no self-service shape.
  They are decisions someone has to make.
- **Art. 12(3)** — a one-month deadline needs a tracked object with a due date. An HTTP call
  that either happened or did not cannot be overdue.
- **Art. 5(2)** — the controller must be able to *demonstrate* it responded.
- When `privacy.self_erasure` is off, the right is not removed — it becomes mediated through
  this queue.

| Endpoint | Permission | Purpose |
|---|---|---|
| `POST/GET /v1/me/privacy/requests`, `DELETE …/{id}` | *(own account)* | File, list, withdraw |
| `GET /v1/admin/privacy/requests` | `privacy.read` | The queue, most-urgent-first, overdue flagged |
| `POST …/{id}/claim` | `privacy.write` | Take ownership; a second claim `409`s rather than silently stealing it |
| `POST …/{id}/resolve` | `privacy.write` | Complete or reject. A rejection **must** state reasons (Art. 12(4)) |
| `POST …/{id}/extend` | `privacy.write` | Art. 12(3) extension; only ever moves the deadline later |
| `GET …/{id}/export` | `privacy.export` | Disclose the subject's record to fulfil an access request |
| `POST …/{id}/fulfil-erasure` | `privacy.write` **+** `users.delete` | Carry out an erasure and complete the request in one call |

Recording an outcome and performing it are **separate endpoints with separate permissions**, so
the trail distinguishes *we said we did it* from *we did it*. `privacy.export` is split out from
`privacy.write` because it is the one action that discloses another person's entire record.

**No subject snapshot.** The queue stores no copy of the subject's email or username;
`user_id` is `ON DELETE SET NULL`. While a request is open its subject exists and is reachable
by join, and the moment an erasure completes the row degrades by itself into "an erasure
request was filed on D1 and completed on D2 by operator O" — an accountability record that is
no longer personal data. Copying the email in for the operator's convenience would have
re-created, in the compliance log, the identifier the erasure destroyed.

**Export redaction** is by explicit column exclusion, spelled out in the SQL next to the
table it applies to: `users.password_hash`, `refresh_tokens.token_hash`, and
`external_accounts.access_token`/`.refresh_token`. The *fact* of a linked account and its
metadata are the user's data and are exported; the bearer credentials are not, because an
export is a commonly-emailed artefact and those grant access to an entirely different
service.

**Erasure and the audit trail.** Every user-owned table declares `ON DELETE CASCADE`, so one
statement removes the profile, sessions, watchlist, progress, notifications, linked accounts
and sync state. `audit_log.actor_id` is `ON DELETE SET NULL` instead — deliberately. Records
of *what privileged actions occurred* survive in pseudonymised form while the identity
linking them to a person is destroyed. Retaining an unlinkable record of an administrative
action rests on legitimate interest (Art. 6(1)(f)), and once the actor reference is gone the
record is no longer personal data.

**Retention** (Art. 5(1)(e), storage limitation) is the `audit.retention_days` sweep. It
deletes in bounded batches so a first sweep over a long-neglected table cannot hold locks
long enough to stall the writers appending to it, and it is idempotent, so replicas need no
leader election — concurrent sweeps simply share the work.

---

## 6. Shutdown

`install_shutdown()` listens for `SIGINT` and (on Unix) `SIGTERM` — what a container runtime
sends first — and cancels one `CancellationToken` shared by the HTTP server and every
background loop. A second signal reaches the default handler and terminates immediately,
which is the conventional escape hatch when a drain hangs.

Loops stop *between* units of work rather than mid-write. This matters concretely: the
notifier acks a message only after fan-out, so being killed between the two would drop a
notification; a worker task severed mid-scan stays claimed until its visibility timeout
expires, stalling the run.

Use `shutdown::every(interval, token, name, task)` for any new periodic loop rather than
hand-rolling `tokio::time::interval` — it skips its first tick, so a rolling restart does not
stampede every replica's sweep at once.

---

## 7. Scan-queue priority and fairness

Scan tasks are dispatched on a NATS work-queue stream, one subject per provider **and scan
mode** (`scan.tasks.<slug>.<mode>`). Workers bind one durable consumer per subject — a
*lane*, `tankovault-workers-<mode>-<slug>` — and pick between lanes on two rules:

1. **Fast scans before full scans.** Every fast lane is offered a turn before any full lane
   is looked at.
2. **Round-robin between providers**, within a mode, taking a single task from a lane before
   moving to the next.

The reason for both is head-of-line blocking. A full catalogue scan fans out into one
`series` task per catalogue entry — six figures for a large provider — and publishes them
back to back. A single consumer on a wildcard serves the stream in publish order, so
everything else waits for that entire backlog to drain: hours or days in which neither
another provider's scan nor a fast scan that would surface a new chapter runs at all. The
two rules make the same backlog cost another provider *one task* of delay, and cost a fast
scan nothing beyond the task currently in flight.

Priority is strict, and safe to be strict, because the fast tier is bounded by construction:
a fast run enqueues exactly one `latest_feed` task per provider and processes the feed
inline, so the fast lanes hold at most one task per provider and cannot starve the full
tier. **If a fast scan is ever changed to fan out**, that reasoning lapses and the tiers need
a weighted split rather than strict priority — `TIERS` in `services/worker/src/queue.rs` is
where the policy lives.

**What to watch.** `scan_tasks_served_total{provider,scan}` counts tasks handed out per lane.
Over a window in which several providers have work, the per-provider counters should climb
together; one provider climbing alone means either the others' lanes are empty or their lanes
failed to open (`could not open provider task lane` in the worker log). A `scan="fast"`
counter that stays flat while fast runs are being planned points at the lane, not the
scheduler.

**Adding a provider.** Its lanes open on the next refresh — `worker.provider_refresh_secs`,
default 60 — not instantly. Until then its tasks sit queued, which is why a freshly created
provider can take up to a minute to start scanning. Provider slugs are restricted to
letters, digits, `-` and `_` because the slug *is* a subject token and part of the consumer
name; the create endpoint rejects anything else with `400`.

**Removing a provider.** Lanes are never dropped while a worker runs, and per-provider
consumers are not deleted with the provider row. That is deliberate: tasks already queued
under the old slug can only be reached by a consumer with that exact filter, so the lane
stays until it has drained them. An idle lane costs one round trip per poll cycle.

**Upgrading.** Two things happen on the first worker start after this change, both
automatic:

- The legacy `tankovault-workers` wildcard consumer is deleted. A work-queue stream refuses
  two consumers whose filters overlap, and `scan.tasks.*` overlaps every lane, so the lanes
  cannot be created beside it. No work is lost: work-queue retention drops a message when it
  is *acked*, not when a consumer is deleted.
- The tasks stream's subject binding widens from `scan.tasks.*` to `scan.tasks.>`, so the
  two-token tiered subjects are captured. This uses `create_or_update_stream` —
  `get_or_create_stream` returns an existing stream untouched, which would silently leave
  every tiered task published to a subject no stream holds.

Tasks published before the upgrade sit on the untiered `scan.tasks.<slug>` subject; each
full-scan lane binds that subject alongside its own, so they are executed as backfill rather
than stranded. During a rolling deploy, an old replica that has not yet restarted loses its
consumer and stops consuming; it recovers when it is replaced.
