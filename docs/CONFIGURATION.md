# Configuration reference

Every `TANKOVAULT_*` key the deployment reads, what it does, its default, and which services
consume it. This document exists because the surface was previously discoverable only by
reading `crates/config/src/lib.rs` and eight per-service `struct Config` definitions —
`docs/OPERATIONS.md` named three keys out of ~70, and `TANKOVAULT_PROFILE`, which gates the
production safety checks, appeared nowhere at all.

`docs/OPERATIONS.md` remains the *behavioural* reference (what the toggles mean, how the
limiter buckets, what the audit sink records). This is the *surface*: the keys themselves.

---

## 1. How configuration is loaded

Layered, lowest precedence first ([`crates/config/src/lib.rs`](../crates/config/src/lib.rs)):

1. The `#[serde(default)]` value compiled into each field.
2. TOML at `$TANKOVAULT_CONFIG` (default: `./config.toml`, silently skipped if absent). If it
   names a **directory**, every `*.toml` directly inside it is merged in file-name order, later
   winning — a `ConfigMap` mounted as a set of fragments.
3. Environment variables prefixed `TANKOVAULT_`.
4. Files in `$TANKOVAULT_SECRETS_DIR`, one per key ([§7](#7-secrets)).
5. `TANKOVAULT_<KEY>_FILE=/path`, which reads `<KEY>` from that path ([§7](#7-secrets)).

**Layers 3, 4 and 5 are mutually exclusive per key.** A key supplied by two of them fails the
boot, naming the key and both sources — it is not resolved by precedence. The failure that
prevents is a half-migrated deployment where a stale environment variable shadows a mounted
secret that has since been rotated: the service keeps working, on the old credential, and the
discrepancy surfaces during an incident rather than during a deploy.

**Nesting is `__` (two underscores).** A single underscore is part of a field name, not a
separator, which is the single most common way to get this wrong:

| TOML | Environment |
|---|---|
| `database.url` | `TANKOVAULT_DATABASE__URL` |
| `database.max_connections` | `TANKOVAULT_DATABASE__MAX_CONNECTIONS` |
| `rate_limit.auth.per_minute` | `TANKOVAULT_RATE_LIMIT__AUTH__PER_MINUTE` |
| `security.cors.allowed_origins` | `TANKOVAULT_SECURITY__CORS__ALLOWED_ORIGINS` |

A file name in the secrets directory uses the same spelling minus the prefix, in any case:
`auth__jwt_secret` is `auth.jwt_secret`. A `.` in a file name is **refused**, not treated as a
separator — Kubernetes allows it in a `Secret` key, and `auth.jwt_secret` would otherwise look
like it worked while nesting somewhere else.

Lists and structured values are **JSON**, quoted as a single shell word:

```bash
TANKOVAULT_SECURITY__CORS__ALLOWED_ORIGINS='["https://app.example.com"]'
TANKOVAULT_CHANNELS__EMAIL_TO='["ops@example.com"]'
```

An **unknown** `TANKOVAULT_*` key is ignored, not rejected. A typo therefore fails silently —
so does a key that has been removed (see [§8](#8-removed-keys)).

> **This document is gated.** `cargo run -p xtask -- config-docs --check` derives every
> `TANKOVAULT_*` key from the config structs and from the `std::env::var` call sites, and fails
> if this document does not match — in either direction. It reads keys from the **leftmost cell
> of a table row** and nowhere else, so a key named in a Notes cell is explanation rather than a
> claim that it exists. A cell may continue with a suffix (`` `…__GLOBAL__PER_MINUTE` /
> `__BURST` ``), which replaces the last segment of the key before it. Keys under
> [§8](#8-removed-keys) are the inverse claim and are asserted **absent** from the code.
> `cargo run -p xtask -- config-docs` (no `--check`) prints the derived list.

---

## 2. Required values

These have no working default. Where a service refuses to start, it does so at boot rather
than at first use, deliberately: a misconfiguration that surfaces during an incident is worse
than one that surfaces during a deploy.

| Key | Services | Failure mode if unset |
|---|---|---|
| `TANKOVAULT_DATABASE__URL` | api, control-plane, worker, notifier, sync, bootstrap | Boot fails: figment reports a missing field. Note the error names `database.url`, not the environment spelling. |
| `TANKOVAULT_TELEMETRY__SERVICE_NAME` | all | Boot fails. The compose file sets it per service. |
| `TANKOVAULT_NATS__URL` | control-plane, worker, notifier | Boot fails. **Optional for `api`**, where its absence only degrades `/v1/me/stream`. |
| `TANKOVAULT_AUTH__JWT_SECRET` | api | Boot fails. Minimum 32 characters; a known placeholder is refused **in every profile**, not just production, because the previous default is published in this repository and every session signed with it is forgeable. |
| `TANKOVAULT_ANILIST__TOKEN_ENCRYPTION_KEY` | sync | Boot fails. Base64 (standard alphabet) of exactly 32 bytes — `openssl rand -base64 32`. This key seals every user's AniList access and refresh tokens at rest; the previous fallback was 32 zero bytes. |
| `TANKOVAULT_ANILIST__CLIENT_ID` / `__CLIENT_SECRET` / `__REDIRECT_URI` | sync | Boot fails. |
| `TANKOVAULT_SEED_ADMIN_PASSWORD` | bootstrap (`seed-admin`), `xtask seed` | The step fails. **`bootstrap` has no default**; `xtask seed` falls back to `changeme12345`, which is a known placeholder the api refuses — a local convenience that cannot survive into a deployment. |
| `TANKOVAULT_INTERNAL__TOKEN` | api, control-plane, worker, sync, render, challenge-solver | **Under `TANKOVAULT_PROFILE=production` only**, boot fails. Elsewhere its absence leaves the internal tier unauthenticated so local development stays frictionless. When present it is length-checked (≥32) in every profile. `openssl rand -hex 32`. |
| `TANKOVAULT_SOLVER__TRAWL_ENDPOINT` | challenge-solver | Boot fails. |

---

## 3. Process-level keys

Read directly from the environment rather than through the layered config, so they cannot be
set in a TOML file.

| Key | Default | Meaning |
|---|---|---|
| `TANKOVAULT_PROFILE` | *(unset)* | The **only** value with an effect is `production` (case-insensitive). It turns on the production safety posture: `internal.token` becomes required, and `/scalar` + the OpenAPI document default to **off**. Nothing else reads it. Setting it to `staging`, `prod` or `dev` is the same as leaving it unset — a real trap, because `prod` looks like it should work. |
| `TANKOVAULT_CONFIG` | `config.toml` | Path to the optional TOML layer — a file, or a directory whose `*.toml` entries are merged in name order. A missing file is not an error; a *misspelled path* is therefore also not an error. A named directory that cannot be read **is**. |
| `TANKOVAULT_SECRETS_DIR` | *(unset)* | Directory of key-named files, one value per file — the shape a Kubernetes `Secret` mounted as a volume has. Unset disables the layer; set-but-unreadable fails the boot, because an operator who named it meant it and booting on defaults instead is the outcome worth avoiding. Entries starting with `.` and anything that is not a regular file are skipped, which is what makes a projected volume's `..data` layout work. |
| `RUST_LOG` | *(unset)* | Standard `EnvFilter` syntax. When set it **replaces** `TANKOVAULT_TELEMETRY__LOG_FILTER` entirely rather than merging with it. |
| `DATABASE_URL` | — | Required by `xtask` (`migrate`, `reset`, `seed`, `sqlx-prepare`) only. The services **and the `bootstrap` image** use `TANKOVAULT_DATABASE__URL`; these two are not interchangeable. |
| `TANKOVAULT_CONFIRM_RESET` | *(unset)* | Must be exactly `1` for `xtask reset`, which **drops and recreates the `public` schema**. Local development only. |
| `SQLX_OFFLINE` | *(unset)* | Build-time, not runtime: resolves sqlx's compile-time-checked queries from the committed `.sqlx/` cache instead of a live database. |

---

## 4. Shared blocks

Defined in `crates/config` and composed into each service's config struct. A block appears in
a service only if that service names it — the *Services* column is the authoritative list.

### `database` — Postgres

| Key | Default | Services | Notes |
|---|---|---|---|
| `TANKOVAULT_DATABASE__URL` | *(required)* | api, control-plane, worker, notifier, sync | `postgres://user:pass@host:5432/tankovault` |
| `TANKOVAULT_DATABASE__MAX_CONNECTIONS` | `16` | as above | Per **replica**. Two workers at 16 is 32 connections against Postgres's default 100. |
| `TANKOVAULT_DATABASE__ACQUIRE_TIMEOUT_SECS` | `10` | as above | How long a request waits for a pooled connection before failing. |

### `redis` — cache, cross-replica rate-limit counters, leader election

| Key | Default | Services | Notes |
|---|---|---|---|
| `TANKOVAULT_REDIS__URL` | *(optional)* | api, control-plane | Absent on `api` → two fallbacks, both per-process: the rate limiter falls back to in-memory counters, so the effective limit is multiplied by the replica count, **and** the SSE stream tickets (`POST /v1/me/stream-ticket` → `GET /v1/me/stream`) become process-local, so with more than one `api` replica a ticket minted on one is unredeemable on the others and the live-notification stream fails to open. Absent on `control-plane` → this replica assumes it is the sole scheduler leader. All fail-open by design; all wrong for a multi-replica deployment. |

### `nats` — JetStream

| Key | Default | Services | Notes |
|---|---|---|---|
| `TANKOVAULT_NATS__URL` | *(required, except on `api`)* | api (optional), control-plane, worker, notifier | The stream names (`TANKOVAULT_TASKS`, `TANKOVAULT_EVENTS`) are compiled-in constants, **not** environment variables. |

### `telemetry` — logging

| Key | Default | Services | Notes |
|---|---|---|---|
| `TANKOVAULT_TELEMETRY__SERVICE_NAME` | *(required)* | all | Reported on every log line and every metric. |
| `TANKOVAULT_TELEMETRY__LOG_FILTER` | `info` | all | `RUST_LOG` syntax, e.g. `info,tankovault=debug`. Overridden wholesale by `RUST_LOG`. |
| `TANKOVAULT_TELEMETRY__JSON_LOGS` | `false` | all | Structured JSON with span context. Set it in any deployment whose logs are shipped anywhere. |

### `metrics` — Prometheus

| Key | Default | Services | Notes |
|---|---|---|---|
| `TANKOVAULT_METRICS__ENABLED` | `true` | all | A real off switch: `false` means the process-wide recorder is never installed, so nothing is retained and the scrape route answers `404`. |
| `TANKOVAULT_METRICS__ROUTE` | `/metrics` | all | |
| `TANKOVAULT_METRICS__LISTEN` | `0.0.0.0:9090` | all | The scrape gets its **own listener**, so it never shares the request-facing port. Set to `null` (JSON) to merge it back onto the main port instead. |
| `TANKOVAULT_METRICS__HTTP_REQUESTS` | `true` | all | Per-request counter and latency histogram. Separate from `ENABLED` because this is the expensive part: a service can keep cheap domain counters while dropping per-route cardinality. |

### `security` — edge hardening

Read by every service **except `frontend`**; see [§5](#5-per-service-blocks) for why not.

| Key | Default | Notes |
|---|---|---|
| `TANKOVAULT_SECURITY__MAX_BODY_BYTES` | `1048576` (1 MiB) | Rejected before the body is buffered. |
| `TANKOVAULT_SECURITY__REQUEST_TIMEOUT_SECS` | `30` | Bounds time-to-response, not response-body streaming. |
| `TANKOVAULT_SECURITY__SECURITY_HEADERS` | `true` | `X-Content-Type-Options`, `X-Frame-Options`, `Referrer-Policy`, `Cross-Origin-Resource-Policy`, and a JSON-API CSP. |
| `TANKOVAULT_SECURITY__HSTS` | `false` | Only meaningful where the edge terminates TLS. Over plain HTTP browsers ignore it and it is merely misleading. |
| `TANKOVAULT_SECURITY__HSTS_MAX_AGE_SECS` | `63072000` (2 years) | The preload minimum. |
| `TANKOVAULT_SECURITY__TRUST_REQUEST_ID` | `false` | Echo an inbound `X-Request-Id` instead of minting one. **Requires a trusted proxy** — a client-supplied id can otherwise collide with or poison log correlation. |
| `TANKOVAULT_SECURITY__EXPOSE_API_DOCS` | `true`, or `false` under `TANKOVAULT_PROFILE=production` | Serves `/scalar` and the OpenAPI document. Unauthenticated, it hands out the complete admin surface — every `/v1/admin/*` path and exact request body — with no failed probe. |
| `TANKOVAULT_SECURITY__CORS__ALLOWED_ORIGINS` | `[]` | JSON array of exact origins. **Empty disables CORS entirely** (same-origin only), which is correct for the reference deployment because the frontend proxies `/v1/*` from its own origin. |
| `TANKOVAULT_SECURITY__CORS__ALLOW_CREDENTIALS` | `false` | Meaningless without a non-empty allowlist. |
| `TANKOVAULT_SECURITY__CORS__MAX_AGE_SECS` | `3600` | |

### `rate_limit` — inbound request limiting

Read by `api`, `control-plane`, `sync`, `render` and `challenge-solver`. `worker` and
`notifier` expose only an ops listener, and `frontend` deliberately mounts no limiter (one
page load fetches the shell plus every hashed asset, so any bucket tight enough to matter
would throttle a legitimate cold load; the API behind it applies the limits that protect
state, and sees the real client because the proxy appends `X-Forwarded-For`).

| Key | Default | Notes |
|---|---|---|
| `TANKOVAULT_RATE_LIMIT__ENABLED` | `true` | `false` leaves the layer unmounted — no per-request cost, and no `X-RateLimit-*` headers. |
| `TANKOVAULT_RATE_LIMIT__BACKEND` | `memory` | Or `redis`. `memory` is per-replica: with `N` replicas the effective limit is `N` times the configured one. `redis` requires `TANKOVAULT_REDIS__URL` and **fails open** if Redis is unreachable. |
| `TANKOVAULT_RATE_LIMIT__TRUST_FORWARDED_FOR` | `false` | **Security setting.** Enable *only* behind a proxy that overwrites `X-Forwarded-For`. With it on and no such proxy, any client forges a fresh bucket per request and bypasses the limiter completely. |
| `TANKOVAULT_RATE_LIMIT__GLOBAL__PER_MINUTE` / `__BURST` | `300` / `60` | Anything without a stricter class. |
| `TANKOVAULT_RATE_LIMIT__AUTH__PER_MINUTE` / `__BURST` | `10` / `5` | Login, register, reset, refresh — the online-guessing control. |
| `TANKOVAULT_RATE_LIMIT__EXPENSIVE__PER_MINUTE` / `__BURST` | `30` / `10` | Data export, scan triggers, sync push/pull. |

`per_minute` is the bucket's refill rate; `burst` is its depth. A burst below the sustained
rate is the normal case, not a misconfiguration.

### `audit` — the privileged-action trail

Read by `api` only — it is the sole service that performs privileged user-facing actions.

| Key | Default | Notes |
|---|---|---|
| `TANKOVAULT_AUDIT__ENABLED` | `true` | `false` installs a no-op sink. Deliberately silent — logging each dropped event would recreate the trail in the log stream. |
| `TANKOVAULT_AUDIT__RECORD_IP` | `false` | An IP is personal data (GDPR Art. 4(1)), so retention is an explicit opt-in. |
| `TANKOVAULT_AUDIT__RECORD_USER_AGENT` | `false` | As above. |
| `TANKOVAULT_AUDIT__RETENTION_DAYS` | `365` | `0` disables the sweep and keeps records forever — rarely what a GDPR-scoped deployment wants (Art. 5(1)(e)). |
| `TANKOVAULT_AUDIT__SWEEP_INTERVAL_HOURS` | `24` | Ignored when retention is `0`. |

### `legal` — operator-published legal documents

Read by `api` only, which serves them unauthenticated at `GET /v1/legal` and
`GET /v1/legal/{slug}`; the SPA's footer builds its Legal column from that index.

Every deployment is a different operator under different law, the text changes without a release,
and an Imprint is a statutory requirement in some jurisdictions and meaningless in others — so
none of it is in the bundle. The whole block is optional: with no `[legal]` section the API
returns an empty index and the footer publishes **no Legal column**, rather than links that 404.

Files are read on demand behind an mtime check, so correcting a policy is an edit, not a restart.
A file that disappears degrades to `404` and a warning, never a panic.

| Key | Default | Notes |
|---|---|---|
| `TANKOVAULT_LEGAL__DIR` | *(unset)* | Root that relative `sources` paths resolve against. An absolute `source` wins over it, so a single absolute path in one variable works without also setting a root. |
| `TANKOVAULT_LEGAL__DOCUMENTS` | `{}` | The published documents, **keyed by URL slug** — so an operator can publish a document this build has never heard of (`dmca`, `acceptable_use`) with no code change. Each value is `{ sources: { <locale>: <path> }, url, updated, title: { <locale>: <text> } }`. `sources` and `url` are mutually exclusive and one is required: a document with neither, or with both, is **refused at boot** naming the slug, because the alternative is a permanent 404 on a link the footer publishes from the same config. As a map it is one figment value, so it takes either a whole JSON object or the usual `__` nesting per leaf: `TANKOVAULT_LEGAL__DOCUMENTS__TERMS__SOURCES__EN=/etc/tankovault/legal/terms.en.md`. `updated` is free text shown verbatim — a file mtime is the wrong answer, since touching a file is not amending a policy. |

`deploy/legal/` holds working samples that say so in their first line, mounted read-only by the
reference compose file. They are not legal advice and not fit for a deployment; replace them.

### `features` — runtime feature-flag plumbing

| Key | Default | Services | Notes |
|---|---|---|---|
| `TANKOVAULT_FEATURES__REFRESH_SECS` | `15` | api, control-plane, notifier, sync | How long a flag change takes to reach *other* replicas. **Which flags are on is not configured here** — that is a runtime decision stored in the database and made from the console, which is the whole point of flags existing alongside these boot-time toggles. |

### `matching` — "is this the same series?"

The confidence policy behind series canonicalisation. Read by **both** the worker's ingest path
and external sync's remote-entry resolution, deliberately: they used to take their thresholds from
different places, so the worker could attach a source that sync would refuse to map with no single
place to reason about it.

Scores are in `[0, 1]` and come from `tankovault_matcher::score` — the strongest of three views of
the two titles (the database's trigram similarity, a token-set ratio, and a whitespace-insensitive
"compact" comparison), adjusted by content-type agreement, release-year proximity, tag overlap and
shared author credits, and vetoed outright when the two titles carry different numbers.

| Key | Default | Services | Notes |
|---|---|---|---|
| `TANKOVAULT_MATCHING__HIGH` | `0.85` | worker, sync | At or above this, attach to the existing series outright. **Raising it makes the matcher conservative**: fewer wrong merges, more duplicate series for an operator to reconcile. Lowering it does the reverse, and a wrong merge is the harder one to undo. |
| `TANKOVAULT_MATCHING__LOW` | `0.6` | worker, sync | At or above this but below `HIGH`, the worker creates the series *and* files a merge candidate for review. Sync ignores this band — it declines to map rather than guessing. |
| `TANKOVAULT_MATCHING__AUTO_MERGE` | `0.97` | control-plane | At or above this — **and** only when a structural identity rule fired (identical titles, identical modulo whitespace, or an exact hit on a name the series already answers to) — the duplicate sweep merges two *already-existing* series without asking. A separate knob from `HIGH` because it governs a different act: `HIGH` files an incoming source, this one deletes a series row and the id it carries. A score alone never suffices; see `tankovault_matcher::adjudicate`. |
| `TANKOVAULT_MATCHING__CANDIDATE_LIMIT` | `10` | worker, sync | Trigram candidates scored per query title. More costs a wider index scan and buys nothing once the true match is in the set. |

### `chapter_outliers` — refusing implausible chapter numbers

Aggregator sites publish listing entries that are not releases: a slug carrying a date
(`chapter-180302`), a year (`chapter-2025`), or a number lifted out of the series title
(`Demon-Lord-2099` → `chapter-2099`). The adapter parses them correctly — the source really does
say that — so a scan judges the listing as a whole and skips the entries that cannot be releases.
Left in, one of them becomes the series' latest chapter and every reader's progress against it
reads as hundreds of chapters behind.

Every threshold is relative to the listing's own spacing rather than an absolute chapter number,
so one setting covers a 20-chapter series and a 4,000-chapter one. Rejections are logged at
`warn` with the numbers, and counted by `chapters_rejected_total{provider}`.

This guard runs at ingest only. Chapters indexed before it existed stay indexed — nothing in the
normal path deletes a chapter — so clearing those is a separate, opt-in sweep:
`cargo run -p xtask -- prune-chapters` reports what the same rule would remove, and
`-- prune-chapters --apply` deletes it.

| Key | Default | Services | Notes |
|---|---|---|---|
| `TANKOVAULT_CHAPTER_OUTLIERS__ENABLED` | `true` | worker | Whether a scan rejects anything at all. The escape hatch for a provider the rule is wrong about. Turning it off does not restore skipped chapters — but ingest is idempotent, so the next scan re-indexes them. |
| `TANKOVAULT_CHAPTER_OUTLIERS__SPARSE_FACTOR` | `20` | worker | **The knob to reach for first.** A trailing run spread more thinly than this multiple of the listing's typical spacing is noise rather than a continuation. Raising it rejects less; lowering it toward `10` starts taking genuine renumbered arcs (a series resuming at 505 after 359) with the junk. |
| `TANKOVAULT_CHAPTER_OUTLIERS__GAP_FACTOR` | `20` | worker | Multiple of typical spacing past which a jump is suspicious enough to consider a cut at all. Only gates which positions are examined; `SPARSE_FACTOR` decides. |
| `TANKOVAULT_CHAPTER_OUTLIERS__MIN_GAP` | `10` | worker | Absolute floor, in chapter numbers, under which a jump is never suspicious. Stops ordinary holes — a pulled chapter, a merged double release — from being examined. |
| `TANKOVAULT_CHAPTER_OUTLIERS__MIN_SAMPLE` | `6` | worker | Smallest listing worth judging. Below this there is no rhythm to compare against, so newly-added series are trusted whole until a later scan grows them. |
| `TANKOVAULT_CHAPTER_OUTLIERS__MIN_BODY` | `5` | worker | Chapters that must survive. Stops a listing being judged down to nothing. |
| `TANKOVAULT_CHAPTER_OUTLIERS__MAX_REJECTED_FRACTION` | `0.25` | worker | Ceiling on the fraction of one listing a single scan may reject. A source that trips this has had its numbering misread wholesale — an adapter to fix, not a catalogue to quietly empty. |

### `internal` — service-to-service authentication

| Key | Default | Services | Notes |
|---|---|---|---|
| `TANKOVAULT_INTERNAL__TOKEN` | *(required in production)* | api, control-plane, worker, sync, render, challenge-solver | One shared secret in `X-Internal-Token`, **identical on every service in the tier**. Minimum 32 characters, checked in every profile when present. `/health` and `/ready` stay reachable without it, so an orchestrator never needs the secret. |

### `email` — transactional mail

Shared verbatim by `api` (welcome, password reset, address-change confirmations) and
`notifier` (new-chapter alerts), so one deployment has one relay configuration rather than two
that can disagree.

| Key | Default | Notes |
|---|---|---|
| `TANKOVAULT_EMAIL__URL` | *(unset)* | A single lettre relay URL (`smtps://user:pass@host:465`). **Takes precedence** over the explicit fields below. |
| `TANKOVAULT_EMAIL__HOST` | *(unset)* | |
| `TANKOVAULT_EMAIL__PORT` | per `SECURITY`: `465` / `587` / `25` | |
| `TANKOVAULT_EMAIL__SECURITY` | `starttls` | `tls`, `starttls` or `none`. Chosen explicitly rather than inferred from the port, so intent is never ambiguous. |
| `TANKOVAULT_EMAIL__USERNAME` | *(unset)* | For OVH-hosted Exchange this is the full mailbox address. |
| `TANKOVAULT_EMAIL__PASSWORD` | *(unset)* | |
| `TANKOVAULT_EMAIL__FROM` | *(unset)* | e.g. `TankoVault <no-reply@example.com>`. **Required to send.** |
| `TANKOVAULT_EMAIL__ENVELOPE_FROM` | falls back to `USERNAME` | The SMTP `MAIL FROM`. Relays that enforce "send as" — notably OVH Exchange — reject a message whose envelope sender is not the authenticated mailbox (`550 5.7.60`). Leave unset unless bounces must go elsewhere. |
| `TANKOVAULT_EMAIL__BASE_URL` | `http://localhost:8080` | Public web origin used to build absolute links inside emails (password reset, verification). **Wrong here means unusable links**, and nothing detects it. |
| `TANKOVAULT_EMAIL__TIMEOUT_SECS` | `15` | |

> **Silent degradation.** The channel is enabled only when a relay (`URL` *or* `HOST`) **and**
> `FROM` are both present. A partial configuration falls back to a no-op mailer that logs and
> drops, so password reset stops working with no error anywhere an operator is looking. Check
> for `no-op mailer` in the service log after any change here.

---

## 5. Per-service blocks

### `api`

| Key | Default | Notes |
|---|---|---|
| `TANKOVAULT_BIND_ADDR` | `0.0.0.0:8080` | |
| `TANKOVAULT_CONTROL_PLANE_URL` | `http://control-plane:8081` | |
| `TANKOVAULT_SYNC_URL` | `http://sync:8083` | |
| `TANKOVAULT_WORKER_URL` | `http://worker:8085` | The worker's ops listener, which also serves the internally-authenticated adapter dry-run behind `POST /v1/admin/providers/{id}/test`. Replaces `TANKOVAULT_CHALLENGE_SOLVER_URL`, which the API no longer needs: the dry-run ran in-process and reached the solver directly, which meant the API binary linked the whole `wreq`/BoringSSL crawl stack (PERF-18). Only the **worker** talks to the challenge solver now. |
| `TANKOVAULT_AUTH__JWT_SECRET` | *(required, ≥32 chars)* | |
| `TANKOVAULT_AUTH__PASSWORD_PEPPER` | `""` | Optional server-side secret mixed into every argon2id hash, so a database leak alone cannot be brute-forced offline. **Once set it must never change** or every existing password stops verifying, and it must be given to both `api` and the `seed` step with the same value. |
| `TANKOVAULT_AUTH__ACCESS_TTL_MINUTES` | `15` | |
| `TANKOVAULT_AUTH__REFRESH_TTL_DAYS` | `30` | |
| `TANKOVAULT_AUTH__COOKIE_SECURE` | `true` | Turn off **only** for local plain-HTTP development, where a `Secure` cookie is simply never sent. This is a 30-day credential. It also selects the cookie's **name and path**: `__Host-refresh_token` at `Path=/` when on, plain `refresh_token` at `/v1/auth` when off — a `__Host-` cookie without `Secure` is refused by the browser rather than downgraded, so the two configurations cannot share a spelling. Consequence: **flipping this setting signs everyone out once**, because already-issued cookies are stored under the other name. |
| `TANKOVAULT_AUTH__WEBAUTHN_ORIGIN` | *(falls back to `TANKOVAULT_EMAIL__BASE_URL`)* | The public origin of the web app, e.g. `https://tanko.example.com`. Passkeys are bound to it by the browser, so it cannot be inferred from a request — a `Host` header is attacker-controlled, and trusting it would let anyone mint credentials under a domain of their choosing. Leave both this and `EMAIL__BASE_URL` unset and passkeys are simply unavailable (logged at boot); set it to something malformed and the API **refuses to start**, because the browser-side symptom is every ceremony failing with an opaque `SecurityError`. Changing it invalidates every registered passkey: a credential bound to the old origin cannot be presented at the new one, and users re-register. |
| `TANKOVAULT_AUTH__WEBAUTHN_RP_ID` | *(the origin's host)* | The registrable domain credentials are bound to. Set it to a **parent** of the origin's host only if the app moves between subdomains and keys must survive the move — a passkey registered under a parent domain works on every child of it. It must cover the origin; if it does not, the API refuses to start. |
| `TANKOVAULT_AUTH__WEBAUTHN_RP_NAME` | `TankoVault` | The name the authenticator shows in its prompt ("Save a passkey for …"). Cosmetic, but it is what the user reads at the moment they decide to trust the site. |

### `control-plane`

| Key | Default | Notes |
|---|---|---|
| `TANKOVAULT_BIND_ADDR` | `0.0.0.0:8081` | |
| `TANKOVAULT_SCHEDULER__FAST_INTERVAL_SECS` | `300` | Seconds between fast-scan sweeps of every active provider. `0` disables. |
| `TANKOVAULT_SCHEDULER__FULL_INTERVAL_SECS` | `0` (disabled) | Full scans are normally on demand. |
| `TANKOVAULT_SCHEDULER__MERGE_SWEEP_INTERVAL_SECS` | `3600` | Seconds between duplicate-reconciliation sweeps. `0` disables. Hourly rather than per-scan because what the sweep is waiting for — enrichment giving a series its authors, year and alternative titles — happens on the order of hours. Also gated by the `scanning.auto_merge` feature flag. |
| `TANKOVAULT_SCHEDULER__MERGE_SWEEP_PAIRS` | `500` | Duplicate pairs shortlisted per sweep that the sweep has **never recorded a verdict for**. Pairs it has are excluded here and revisited under the two budgets below, which is what gives this ordered-and-limited query a progress guarantee — without the exclusion it re-offered the same prefix hourly and never advanced. |
| `TANKOVAULT_SCHEDULER__MERGE_SWEEP_REQUEUE` | `250` | Open review-queue rows re-scored per sweep, least-recently-scored first. A candidate filed at ingest was scored before the series had tags, authors or synonyms, so its score is a floor; this is how it gets revisited. |
| `TANKOVAULT_SCHEDULER__MERGE_SWEEP_RECHECK` | `250` | Pairs the sweep previously judged **distinct**, reconsidered per sweep, least-recently-scored first. Same argument as the requeue budget and the opposite verdict: a pair scored apart before enrichment filled both sides in has to be able to come back. Re-scoring bumps `updated_at`, so this drains as a round-robin. An operator's dismissal is never in this rotation. |
| `TANKOVAULT_SCHEDULER__MERGE_SWEEP_MAX_AUTO_MERGES` | `200` | **Ceiling on a destructive background action.** Nothing else bounds how many series one sweep may delete, so a mistaken threshold or normalization rule would otherwise collapse the catalogue between two ticks. Exceeding it defers the rest to the next sweep and reports the count. A `deferred` that is non-zero on *every* run means the ceiling is binding rather than protecting — raise it and the pairs budget together to drain the backlog, then put both back. |

### `worker`

| Key | Default | Notes |
|---|---|---|
| `TANKOVAULT_BIND_ADDR` | `0.0.0.0:8085` | Liveness, readiness **and** one internal route: `POST /internal/providers/{id}/test`, the adapter dry-run the API proxies here so that binary need not link the crawl stack (PERF-18). The dry-run sits inside `HttpStack::with_internal_auth`, so it needs `TANKOVAULT_INTERNAL__TOKEN`; `/health` and `/ready` are merged outside it and stay reachable to a probe without the secret. This port is also what the container healthcheck connects to. |
| `TANKOVAULT_WORKER__CHALLENGE_SOLVER_ENDPOINT` | `http://challenge-solver:8090` | |
| `TANKOVAULT_WORKER__MAX_CATALOG_PAGES` | `20000` | A runaway-paginator backstop, not a budget: real termination is the adapter's `has_next` marker. Some catalogues legitimately paginate into the thousands, so a value near a real catalogue size **silently truncates it**. |
| `TANKOVAULT_WORKER__PROVIDER_REFRESH_SECS` | `60` | How often the round-robin queue re-reads the provider list. A newly created provider does not start scanning until its lane opens on the next refresh. |

### `notifier`

| Key | Default | Notes |
|---|---|---|
| `TANKOVAULT_BIND_ADDR` | `0.0.0.0:8082` | Ops listener only. |
| `TANKOVAULT_CHANNELS__EMAIL_TO` | `[]` | JSON array of recipients. Empty disables the email channel. The relay comes from `TANKOVAULT_EMAIL__*`. |
| `TANKOVAULT_CHANNELS__DISCORD_WEBHOOK_URL` | *(unset)* | Also gated by the `notifications.discord` feature flag, which ships **off**. |
| `TANKOVAULT_CHANNELS__WEBHOOK_URL` | *(unset)* | Also gated by `notifications.webhook`, which ships **off**. |
| `TANKOVAULT_CHANNELS__TIMEOUT_SECS` | `10` | |

### `sync`

| Key | Default | Notes |
|---|---|---|
| `TANKOVAULT_BIND_ADDR` | `0.0.0.0:8083` | |
| `TANKOVAULT_RECONCILE_INTERVAL_SECS` | `900` | `0` disables the scheduled reconciliation loop. |
| `TANKOVAULT_ANILIST__CLIENT_ID` | *(required)* | |
| `TANKOVAULT_ANILIST__CLIENT_SECRET` | *(required)* | |
| `TANKOVAULT_ANILIST__REDIRECT_URI` | *(required)* | Must point at the **frontend's** callback route, not the API's: the API callback needs the SPA's in-memory bearer token, which a raw browser OAuth redirect cannot carry. |
| `TANKOVAULT_ANILIST__TOKEN_ENCRYPTION_KEY` | *(required)* | Base64 of exactly 32 bytes. |
| `TANKOVAULT_ANILIST__GRAPHQL_URL` | `https://graphql.anilist.co` | |
| `TANKOVAULT_ANILIST__OAUTH_BASE` | `https://anilist.co/api/v2/oauth` | |
| `TANKOVAULT_ANILIST__DEFAULT_CONFLICT_POLICY` | `newest_wins` | `local_wins`, `remote_wins`, `newest_wins` or `ask_me`. |
| `TANKOVAULT_ANILIST__MIN_REQUEST_INTERVAL_MS` | `700` | Outbound pacing against AniList's rate limit. |
| `TANKOVAULT_METADATA__ENRICH_ENABLED` | `true` | The background metadata-enrichment sweep. |
| `TANKOVAULT_METADATA__ENRICH_INTERVAL_SECS` | `3600` | |
| `TANKOVAULT_METADATA__ENRICH_BATCH` | `200` | Series per database page. |
| `TANKOVAULT_METADATA__ENRICH_MAX_SERIES` | `2000` | Upper bound per sweep. One `AniList` request each, paced by `TANKOVAULT_ANILIST__MIN_REQUEST_INTERVAL_MS`, so the default is ~23 min of work inside the hourly interval. Lower it if the sweep is crowding a shared rate-limit budget; raise it to walk a large catalogue sooner. |
| `TANKOVAULT_METADATA__PRIORITY__DESCRIPTION` / `__TITLE` / `__COVER` / `__DEFAULT` | `["anilist","adapter"]` | Which source wins per field; `__DEFAULT` is the fallback order for fields without one of their own. Only `anilist` and `adapter` are accepted; anything else is a startup error rather than a silently ignored entry. |

### `challenge-solver`

| Key | Default | Notes |
|---|---|---|
| `TANKOVAULT_BIND_ADDR` | `0.0.0.0:8090` | |
| `TANKOVAULT_SOLVER__BACKEND` | `trawl` | The only wired back-end today. |
| `TANKOVAULT_SOLVER__TRAWL_ENDPOINT` | *(required)* | e.g. `http://trawl:8191`. The client posts TRAWL's native `POST /scrape`, not its FlareSolverr-compatible `/v1` — that endpoint answers with an empty `headers` object, which loses `Retry-After`. |
| `TANKOVAULT_SOLVER__MAX_TIMEOUT_MS` | `60000` | Sent as TRAWL's `maxTimeout`; the HTTP client's own timeout sits 15s above it so TRAWL's budget is the one that expires. |
| `TANKOVAULT_SOLVER__SESSION_TTL_SECS` | `900` | How long *this* deployment replays a solved session for. Distinct from TRAWL's own `SESSION_TTL_SECONDS` (its internal per-domain cookie jar, default 3600). |

### `render`

`CHROME_PATH` and `NO_SANDBOX` are baked into the `runtime-browser` image, so the compose
stack sets neither.

| Key | Default | Notes |
|---|---|---|
| `TANKOVAULT_BIND_ADDR` | `0.0.0.0:8084` | |
| `TANKOVAULT_RENDER__CHROME_PATH` | auto-detected (`/usr/bin/chromium` in the image) | |
| `TANKOVAULT_RENDER__HEADLESS` | `true` | |
| `TANKOVAULT_RENDER__NO_SANDBOX` | `true` (image sets it explicitly) | Required inside the container — no user namespaces. |
| `TANKOVAULT_RENDER__NAV_TIMEOUT_MS` | `30000` | |
| `TANKOVAULT_RENDER__DEFAULT_WAIT_MS` | `0` | Extra settle delay after every navigation. |
| `TANKOVAULT_RENDER__USER_AGENT` | *(unset)* | When set it is applied to the page **and reported back**, so a solved `cf_clearance` cookie stays paired with a stable UA. |
| `TANKOVAULT_RENDER__SESSION_TTL_SECS` | `900` | |
| `TANKOVAULT_RENDER__CHALLENGE_WAIT_MS` | `5000` | Settle time for a bot-management challenge during `/v1/solve`. |

### `frontend`

| Key | Default | Notes |
|---|---|---|
| `TANKOVAULT_BIND_ADDR` | `0.0.0.0:3000` | Unprivileged by necessity: the `scratch` image runs as UID 1001, which cannot bind port 80. |
| `TANKOVAULT_FRONTEND__STATIC_DIR` | `/srv/www` | Baked into the image. |
| `TANKOVAULT_FRONTEND__NOTICES_PATH` | `/THIRD-PARTY-NOTICES` | The generated third-party licence notices, published as `text/plain` at `/third-party-notices` and linked from the app's rail. Points at the image's own copy (beside `/LICENSE`), *outside* the static dir, so the ~1 MB document is not duplicated into the bundle. Point it at a checkout's `THIRD-PARTY-NOTICES` when running the SPA locally. |
| `TANKOVAULT_FRONTEND__API_UPSTREAM` | `http://api:8080` | Origin the `/v1/*` proxy targets, and the target of this service's readiness probe. No trailing slash. |
| `TANKOVAULT_FRONTEND__MAX_BODY_BYTES` | `10485760` (10 MiB) | Largest request body accepted on this hop. Enforced both by the shared body-limit layer and by the proxy's own buffering guard. |
| `TANKOVAULT_FRONTEND__CONNECT_TIMEOUT_SECS` | `10` | Connection establishment only, **not** a whole-request timeout — an SSE stream is one request that stays open indefinitely. |

`TANKOVAULT_SECURITY__*` is deliberately **not read by this service**. Its hardening config is
derived in code (`stack_security` in `services/frontend/src/main.rs`) because the shared
security-header set is API-shaped: its `Content-Security-Policy: default-src 'none'` is correct
for a JSON API and fatal for an HTML document — it blocks the WASM bundle, so the app does not
boot. The SPA sends its own policy instead. Offering the keys anyway would only expose settings
that silently do nothing.

### `bootstrap`

Not a long-running service: a one-shot image (`bootstrap <migrate|seed-admin|seed-providers>`)
that a deployment runs before a rollout and once at install. It reads `database` and nothing
else from the shared blocks — no telemetry, no internal token, no JWT secret; a migration job
holding the credential that signs sessions would be privilege it never uses.

| Key | Default | Notes |
|---|---|---|
| `TANKOVAULT_SEED_ADMIN_EMAIL` | `admin@tankovault.local` | Address of the account `seed-admin` creates. |
| `TANKOVAULT_SEED_ADMIN_USERNAME` | `admin` | |
| `TANKOVAULT_SEED_ADMIN_PASSWORD` | *(required)* | Section 2. Not echoed to stdout, unlike `xtask seed` — a `Job`'s logs outlive the shell that started it. |
| `TANKOVAULT_AUTH__PASSWORD_PEPPER` | *(empty)* | **Must be the value the api runs with.** The hash is peppered at rest, so seeding with one value and serving with another leaves an account whose correct password is rejected, with nothing in the logs naming the cause. Empty under `TANKOVAULT_PROFILE=production` is refused outright. |

`seed-admin` is the one place in the system where privilege is minted rather than granted:
registration never confers a permission, so without this account nobody could grant
`users.permissions` to anyone. Both seed steps are create-only — re-running them leaves an
existing installation exactly as it is, so a revoked permission stays revoked.

---

## 6. Getting it wrong: the failure modes worth knowing

| Symptom | Likely cause |
|---|---|
| Password reset and verification emails never arrive, no error anywhere | `TANKOVAULT_EMAIL__FROM` or the relay is unset → the no-op mailer. Grep the log for `no-op mailer`. |
| Emails arrive with unusable links | `TANKOVAULT_EMAIL__BASE_URL` still points at `localhost`. |
| The rate limit is `N`× looser than configured | `TANKOVAULT_RATE_LIMIT__BACKEND` left at `memory` with more than one replica. |
| The rate limit does nothing at all | `TRUST_FORWARDED_FOR=true` with the service reachable without going through the proxy. |
| A configuration change had no effect | `_` where `__` was needed, or the key was ignored as unknown. Unknown keys are silent. |
| Production safety checks are not running | `TANKOVAULT_PROFILE` set to something other than exactly `production`. |
| A provider's catalogue scans only partially | `TANKOVAULT_WORKER__MAX_CATALOG_PAGES` below its real page count. |
| Sessions do not persist / users are signed out on refresh | `TANKOVAULT_AUTH__COOKIE_SECURE` left at `true` over plain HTTP. |
| Every password suddenly fails to verify | `TANKOVAULT_AUTH__PASSWORD_PEPPER` changed or lost. |

---

## 7. Secrets

```bash
openssl rand -hex 32      # TANKOVAULT_AUTH__JWT_SECRET, TANKOVAULT_INTERNAL__TOKEN
openssl rand -base64 32   # TANKOVAULT_ANILIST__TOKEN_ENCRYPTION_KEY (must decode to 32 bytes)
```

Placeholder values published in this repository are **refused at boot in every profile**, not
only production. That is deliberate: a working default here would only move the failure to a
point where it is harder to notice.

### 7.1 From the environment (local development)

`deploy/local.env.example` is the template. Copy it to `deploy/local.env` (git-ignored) and
pass `--env-file deploy/local.env`.

### 7.2 From files (Kubernetes, and anything else that mounts secrets)

Prefer this everywhere it is available. A value in a process's environment is readable from
`/proc/<pid>/environ` by anything sharing the namespace, is inherited by every child process,
lands in crash dumps, and is printed by anything that dumps the environment. A mounted file is
none of those, and it can be **rotated without restarting the process** (§7.3).

A directory, which is what a `Secret` volume mount looks like:

```bash
kubectl create secret generic tankovault \
  --from-literal=auth__jwt_secret="$(openssl rand -hex 32)" \
  --from-literal=auth__password_pepper="$(openssl rand -hex 32)" \
  --from-literal=internal__token="$(openssl rand -hex 32)" \
  --from-literal=database__url='postgres://tankovault:…@postgres:5432/tankovault'
```

```yaml
env:
  - name: TANKOVAULT_SECRETS_DIR
    value: /etc/tankovault/secrets
volumeMounts:
  - name: secrets
    mountPath: /etc/tankovault/secrets
    readOnly: true
volumes:
  - name: secrets
    secret: { secretName: tankovault }
```

Or one path per key, which is what Docker Compose `secrets:` gives you:

```yaml
services:
  api:
    environment:
      TANKOVAULT_AUTH__JWT_SECRET_FILE: /run/secrets/jwt
    secrets: [jwt]
secrets:
  jwt: { file: ./jwt.txt }
```

The `_FILE` suffix works on any key in [§4](#4-shared-blocks) or [§5](#5-per-service-blocks) —
it is a spelling rule, not a fixed list, which is why no table here enumerates those names. It
does **not** work on the process-level keys in [§3](#3-process-level-keys): they are read before
the layered config exists, so a `_FILE` naming one is refused rather than ignored.

Two behaviours worth knowing:

- **Trailing newlines are stripped**, and only those. `printf 'x\n' > f` and every text editor
  add a newline nobody meant as part of the value. Spaces and tabs are kept, because a trailing
  space can be a real character of a real password.
- **File-sourced values are never parsed.** An environment variable goes through a TOML-ish
  parse, so `TANKOVAULT_AUTH__JWT_SECRET=12345678` becomes a *number* and the boot fails with
  "invalid type: integer, expected a string". The same secret in a file stays a string. Put
  lists and numbers in the TOML layer, which parses them properly.

### 7.3 Rotation without a restart

Every long-running service watches the directories its configuration came from. When the
kubelet updates a mounted `Secret` or `ConfigMap`, the service re-reads the whole configuration
and **rebuilds its runtime**: the connection pool, the application state, the router, the
listener and the background loops. In-flight requests drain before the replacement binds.

- A reload that fails to read, or fails to build, **leaves the running service exactly as it
  was** and logs the reason. A bad file write cannot take down a healthy pod.
- Files that change but resolve to the same values rebuild nothing.
- `telemetry.*` and `metrics.*` are the exception: the `tracing` subscriber and the metrics
  recorder are process-global and installed once, so those two blocks still need a restart.
- Rotating `auth.jwt_secret` **signs every user out** — sessions signed with the old key stop
  verifying. Rotating `auth.password_pepper` makes every stored password fail to verify, and
  rotating `anilist.token_encryption_key` does not re-seal tokens already at rest. Those three
  are rotations with a migration, not drop-in replacements; the reload applies them faithfully
  either way.

---

## 8. Removed keys

Setting one of these has no effect. They are listed because silence is exactly the problem
they were removed for.

| Key | Removed because |
|---|---|
| `TANKOVAULT_TELEMETRY__OTLP_ENDPOINT` | It never exported anything. Four OpenTelemetry crates were declared in `[workspace.dependencies]`, used by zero members, while this knob logged `"collector export is pending"` and installed no layer. An operator who set it believed traces were being exported and would have found out during an incident. Re-add it only together with a real `OpenTelemetryLayer` in `crates/service/src/telemetry.rs`. |

`TANKOVAULT_TASKS` and `TANKOVAULT_EVENTS` are **not** environment variables — they are the
JetStream stream names, compiled in at `crates/contracts/src/subjects.rs`.
