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

## 3. GDPR data-subject endpoints

| Endpoint | Right | Notes |
|---|---|---|
| `GET /v1/me/export` | Art. 20, portability | One JSON document assembled in a single query, so the export is a consistent snapshot. Served as an attachment. |
| `DELETE /v1/me` | Art. 17, erasure | Requires the caller's own username echoed back in `{"confirm_username": "..."}`. |

Both act only on the authenticated principal; there is no operator override on these paths.
Both draw from the `expensive` rate-limit class.

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

## 4. Shutdown

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
