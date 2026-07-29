# TankoVault / Kanpai — Security Audit

Scope: security only (AuthN, AuthZ, SQL, SSRF/crawler, input validation, secrets/config,
transport/headers, rate limiting, GDPR paths, dependency posture, logging).
Read-only analysis of `E:\Rust\manga-tracker-v3` @ `2c9a22e`.

Findings are ordered most-severe first. Every finding carries file:line evidence.
A "Verified safe" section at the end lists controls checked and found correct.

---

## 1. Internal `sync` service takes `user_id` from the request body with no authentication, and is published on the host

**Severity: Critical** — CWE-306 (Missing Authentication for Critical Function), CWE-639 (Authorization Bypass Through User-Controlled Key)

**Evidence**

`services/sync/src/main.rs:341-356` — the entire router, with no auth extractor, no shared
secret, no mTLS:

```rust
let routes = Router::new()
    .route("/v1/sync/providers", get(providers_list))
    .route("/v1/sync/push-series", post(push_series))
    .route("/v1/sync/enrich", post(enrich))
    .route("/v1/sync/{provider}/authorize-url", get(authorize_url))
    .route("/v1/sync/{provider}/status/{user_id}", get(status))
    .route("/v1/sync/{provider}/link", post(link).delete(unlink))
    .route("/v1/sync/{provider}/pull", post(pull))
    .route("/v1/sync/{provider}/push", post(push))
    .route("/v1/sync/conflicts/{user_id}", get(list_conflicts))
    .route("/v1/sync/conflicts/{id}/resolve", post(resolve_conflict))
    .route("/v1/sync/history/{user_id}", get(list_history))
```

`services/sync/src/main.rs:437-450` — the subject is whatever the caller says it is:

```rust
struct LinkRequest { user_id: UserId, code: String }

async fn link(State(state): State<AppState>, Path(provider): Path<String>,
              Json(req): Json<LinkRequest>) -> Result<StatusCode, AppError> {
    state.engine.link(&provider, req.user_id, &req.code).await?;
```

`services/control-plane/src/main.rs:173-174` — same pattern:
`Router::new().route("/internal/scans", post(trigger_scan))`, unauthenticated.

`deploy/docker-compose.yml:221` — `ports: ["8083:8083"]` (sync), `:163` — `["8081:8081"]`
(control-plane), `:33` — `["5432:5432"]` (Postgres with `tankovault:tankovault`,
`docker-compose.yml:29-31`), `:43` `["6379:6379"]` (Redis, no password), `:53`
`["4222:4222"]` (NATS).

**Exploit scenario**
Anyone who can reach the host on 8083 (any co-tenant, anyone on the LAN, anyone at all if
the host has a public interface — Docker's port publishing punches through the host
firewall via DOCKER-USER/iptables) issues:

- `GET /v1/sync/anilist/status/<victim-uuid>` → the victim's linked AniList identity.
- `GET /v1/sync/history/<victim-uuid>`, `GET /v1/sync/conflicts/<victim-uuid>` → the
  victim's full reading/sync history.
- `POST /v1/sync/anilist/link {"user_id": "<victim>", "code": "<attacker's OAuth code>"}`
  → binds the attacker's AniList account to the victim, so every subsequent push writes
  the victim's reading data into the attacker's list.
- `DELETE /v1/sync/anilist/link {"user_id":"<victim>"}` → destroys the link.
- `POST /v1/sync/anilist/pull {"user_id":"<victim>","policy":"remote"}` → overwrites the
  victim's local progress.

Every one of these bypasses the API's `AuthUser` extractor entirely. Postgres on 5432 with
a documented default password is a direct route to the whole dataset, including the
argon2 hashes.

**Fix**
1. Remove `ports:` from `sync`, `control-plane`, `render`, `challenge-solver`,
   `flaresolverr`, `postgres`, `redis` and `nats` in `deploy/docker-compose.yml`; they are
   already reachable over the compose network by service name. If host access is wanted
   for debugging, bind loopback only (`127.0.0.1:8083:8083`).
2. Add a shared-secret gate to the internal services regardless — a
   `tower::ServiceBuilder` layer that requires a constant-time-compared
   `X-Internal-Token` header sourced from config, applied in `HttpStack` for services
   whose config declares `internal_only = true`. Port hygiene alone is one
   misconfiguration away from failing.

**Effort: S** (compose change) / **M** (internal auth layer)

---

## 2. `render` and `challenge-solver` fetch any caller-supplied URL, unauthenticated, and return the body

**Severity: Critical** — CWE-918 (SSRF), CWE-306, CWE-73 (External Control of File Name or Path)

**Evidence**

`services/render/src/main.rs:94` — `.route("/v1/render", post(render))`, no auth layer.
`services/render/src/main.rs:110-119` — the URL is copied straight from the body into
`RenderOptions` with no validation:

```rust
async fn render(State(state): State<AppState>, Json(req): Json<RenderRequest>) -> impl IntoResponse {
    let url = req.url.clone();
    let opts = RenderOptions { url: req.url, wait_selector: req.wait_selector, wait_ms: req.wait_ms };
    match state.manager.render(opts).await {
```

`services/render/src/browser.rs:94` — handed to Chrome verbatim:

```rust
let page: Page = browser.new_page(opts.url.as_str()).await?;
```

`services/render/src/browser.rs:114-121` — the rendered DOM **and the browser's cookies**
are returned to the caller. There is no import of `tankovault_fetch::ssrf` anywhere in
`services/render` — the SSRF guard is only wired into `crates/fetch/src/base.rs:91,115`.

`services/challenge-solver/src/main.rs:99,114-116` — same shape; `SolveRequest.url` goes
unvalidated to `crates/solver/src/flaresolverr.rs:81-86`, which POSTs it to FlareSolverr's
`request.get`.

`deploy/docker-compose.yml:248` — `ports: ["8084:8084"]` (render), `:233` —
`["8090:8090"]` (challenge-solver), `:64` — `["8191:8191"]` (FlareSolverr itself).

**Exploit scenario**
An unauthenticated attacker who can reach port 8084:

- `POST /v1/render {"url":"file:///etc/passwd"}` → file contents in `html`. `file:///proc/self/environ`
  yields the container's environment, which for the render container is limited, but
  `file:///` traversal of any mounted volume is available.
- `POST /v1/render {"url":"http://169.254.169.254/latest/meta-data/iam/security-credentials/"}`
  → cloud instance credentials, returned in the response body.
- `POST /v1/render {"url":"http://postgres:5432"}`, `http://api:8080/v1/admin/...`,
  `http://nats:8222/varz` → full internal network read via a browser that also returns
  cookies it collected.

The `no_sandbox` flag is baked into the runtime-browser image
(`docker-compose.yml:246` comment), so a Chrome renderer bug is also a container escape
rather than a sandbox escape.

**Fix**
1. In `services/render/src/main.rs::render` and `services/challenge-solver`'s `solve`,
   call `tankovault_fetch::ssrf::validate_url` plus `resolve_checked` on the parsed URL
   before dispatch, and reject anything that is not `http`/`https` resolving to a public
   address. `crates/fetch/src/ssrf.rs:96-105` already provides `resolve_checked` and it is
   currently dead code — nothing in the workspace calls it.
2. Chrome cannot be constrained by a Rust DNS resolver, so additionally pass
   `--host-resolver-rules` / an explicit proxy, or run the browser in a network namespace
   with egress restricted to public ranges.
3. Apply the internal-auth gate from finding 1.

**Effort: M**

---

## 3. Rate limiting on `/v1/auth/*` is bypassable by forging `X-Forwarded-For`

**Severity: High** — CWE-307 (Improper Restriction of Excessive Authentication Attempts), CWE-348 (Use of Less Trusted Source)

**Evidence**

`deploy/docker-compose.yml:129-132` turns proxy trust on, with a comment asserting the
proxy overwrites the headers:

```yaml
# The frontend server fronts the API in this compose file and overwrites the forwarded
# headers, so they can be trusted here. Do NOT enable this when the API is exposed
# directly: a client can then forge a fresh bucket per request and bypass the limiter.
TANKOVAULT_RATE_LIMIT__TRUST_FORWARDED_FOR: "true"
```

The comment is factually wrong. `services/frontend/src/main.rs:266-271` **appends**:

```rust
let forwarded_for = match headers.get(&X_FORWARDED_FOR).and_then(|v| v.to_str().ok()) {
    Some(existing) if !existing.is_empty() => format!("{existing}, {client_ip}"),
    _ => client_ip.to_owned(),
};
```

and `crates/service/src/ratelimit/mod.rs:348-357` reads the **left-most** entry — the one
the client controls:

```rust
if let Some(forwarded) = headers.get("x-forwarded-for").and_then(|v| v.to_str().ok()) {
    // The left-most entry is the original client; everything after it was appended by
    // successive proxies.
    if let Some(first) = forwarded.split(',').next() {
```

The frontend's own doc comment at `services/frontend/src/main.rs:260-262` states the
opposite convention ("the peer this proxy actually accepted the connection from is the
trustworthy **right-most** entry"). The two halves of the system disagree.

Independently, `deploy/docker-compose.yml:148` publishes `ports: ["8080:8080"]` for the
API itself, so the proxy can be skipped entirely while `trust_forwarded_for` is still on.

**Exploit scenario**
`for i in $(seq 1 1000000); do curl -H "X-Forwarded-For: 10.0.$((i/256)).$((i%256))" \
-d '{"login":"victim@example.com","password":"..."}' https://host/v1/auth/login; done`
— every request lands in a fresh token bucket, so the 10/min auth budget
(`crates/config/src/lib.rs:468-470`) never engages. Unlimited online password guessing,
unlimited password-reset email flooding at `/v1/auth/password/forgot`, and unlimited
account registration. Redis-backed counters make it worse, not better: the attacker fills
Redis with a million distinct bucket keys.

Amplifier: `services/api/src/auth.rs:670-673` caps the password at a **minimum** of 8
characters and imposes no maximum, and `crates/auth/src/password.rs:16-24` uses
`Params::default()` (19 MiB, t=2). With the limiter neutralised, concurrent
`POST /v1/auth/register` requests each pin 19 MiB of argon2 memory until the 1 MiB body
cap is the only bound — a cheap memory-exhaustion DoS on the API replica.

**Fix**
1. Make the two ends agree. Either have `services/frontend` **replace**
   `X-Forwarded-For` with the peer address (`headers.insert(X_FORWARDED_FOR, peer)`), or
   change `forwarded_client_ip` to take the right-most entry. Replacing at the proxy is
   the stronger choice — the API then needs no trust assumption about entry position.
2. Better still: add a `trusted_proxies: Vec<IpNet>` to `RateLimitConfig` and only honour
   the header when `ConnectInfo` peer is in that set; strip untrusted hops right-to-left.
3. Remove `ports: ["8080:8080"]` from the api service in compose.
4. Cap password length (e.g. 4096 bytes) in `validate_registration` and `reset_password`.

**Effort: S**

---

## 4. Email address can be changed with no re-authentication and no re-verification → full account takeover from a 15-minute token

**Severity: High** — CWE-620 (Unverified Password Change), CWE-306

**Evidence**

`services/api/src/me/account.rs:53-70` — no current-password confirmation, no format
validation, no length cap, no re-verification, no session revocation:

```rust
pub async fn patch_profile(
    State(state): State<AppState>, user: AuthUser, Json(body): Json<ProfileUpdate>,
) -> ApiResult<Json<ProfileDto>> {
    let username = body.username.as_deref().map(str::trim).filter(|s| !s.is_empty());
    let email    = body.email.as_deref().map(str::trim).filter(|s| !s.is_empty());
    let updated = tankovault_db::repo::users::update_profile(&state.pool, user.user_id, username, email).await?;
```

`crates/db/src/repo/users.rs:524-534` — `email_verified_at` is left untouched:

```sql
UPDATE users SET username = COALESCE($2, username), email = COALESCE($3, email)
WHERE id = $1
```

So the new address inherits the old one's verified status. `services/api/src/auth.rs:419`
(`forgot_password`) then sends a reset link to it, and `services/api/src/auth.rs:237-241`
lets it sign in.

Compare `services/api/src/auth.rs:456-490` (`reset_password`), which does the right thing
— revokes every session on credential change. `patch_profile` does none of that. There is
also **no authenticated change-password endpoint at all** in
`services/api/src/lib.rs:207-214` (the auth route block); the only path to a new password
is the emailed reset.

**Exploit scenario**
An attacker obtains an access token — via a shared/kiosk browser, an XSS foothold in any
future frontend regression, a leaked SSE URL (finding 8), or a proxy log. Within the
15-minute window (`services/api/src/main.rs:89-91`) they call
`PATCH /v1/me/profile {"email":"attacker@evil.tld"}`, then
`POST /v1/auth/password/forgot {"email":"attacker@evil.tld"}`, receive the reset link, and
set a new password. `reset_password` then revokes the legitimate owner's sessions
(`auth.rs:489`). The owner is locked out of an account whose recovery address they no
longer control. The victim never receives a notification: no email is sent on address
change.

**Fix**
1. Require the current password in the request body for any email change, verified with
   `tankovault_auth::verify_password` before the update.
2. Do not write the new address to `users.email` directly. Write it to a pending column,
   issue a confirmation token via the existing `send_verification_email`
   (`services/api/src/auth.rs:585-605`) to the **new** address, and only swap on
   confirmation. Send a "your address is being changed" notice to the **old** address.
3. Set `email_verified_at = NULL` in `update_profile` whenever `$3 IS NOT NULL` and
   differs, so the fail-open path is at worst "must re-verify".
4. Revoke the refresh-token family on email change, as `reset_password` already does.
5. Add `POST /v1/me/password` requiring the current password.
6. Validate the new username/email with `validate_registration`'s rules — `patch_profile`
   currently accepts a 1 MiB username or an email with no `@`.

**Effort: M**

---

## 5. SSRF guard is bypassed entirely by an IP-literal URL

**Severity: High** — CWE-918

**Evidence**

`crates/fetch/src/ssrf.rs:83-92` — the pre-flight checks only the scheme and the presence
of a host:

```rust
pub fn validate_url(url: &Url) -> Result<(), SsrfError> {
    if !matches!(url.scheme(), "http" | "https") { return Err(SsrfError::Scheme(...)); }
    if url.host_str().is_none() { return Err(SsrfError::NoHost); }
    Ok(())
}
```

`crates/fetch/src/base.rs:113-116` relies on the resolver for the address check:

```rust
// Cheap pre-flight; the resolver enforces the address-range check at connect time.
ssrf::validate_url(&url)?;
```

`crates/fetch/src/base.rs:91` — `.dns_resolver(Arc::new(SsrfResolver))`. But
`hyper-util`'s `HttpConnector` (which `wreq` 5.3 uses beneath `wreq::Client`) short-circuits
DNS when the authority parses as an IP literal — the custom `Resolve` impl is never called.
`SsrfResolver::resolve` (`crates/fetch/src/ssrf.rs:113-129`) is therefore unreachable for
`http://127.0.0.1/`, `http://169.254.169.254/`, `http://[::1]:5432/`.

`is_forbidden_ip` is never invoked outside `ssrf.rs` — confirmed by grep across
`crates/` and `services/`: the only call sites of anything in that module are
`base.rs:20,91,115`. `resolve_checked` (`ssrf.rs:96`) is dead code.

The supplying path is unvalidated too — `services/api/src/admin/providers.rs:94-105`
stores `base_url` with no scheme or host check at all:

```rust
let provider = tankovault_db::repo::providers::create(&state.pool, NewProvider {
    slug: req.slug, name: req.name, base_url: req.base_url, ...
```

**Exploit scenario**
An operator holding `ProvidersCreate` + `ProvidersTest` (a role well short of full admin —
see `crates/domain/src/permissions.rs`) creates a provider with
`base_url = "http://169.254.169.254"` and calls
`POST /v1/admin/providers/{id}/test`. `services/api/src/admin/providers.rs:388-416` returns
the adapter's parsed output — or, on a parse failure, `e.to_string()` — for both
`list_latest` and `fetch_series`, disclosing cloud IMDS credentials, `http://api:8080`
internals, or the response of any internal HTTP service. The scheduled scan workers will
then keep hitting the target on a timer.

Note the endpoint's own doc comment at `services/api/src/admin/providers.rs:356` claims
"SSRF and rate limits are enforced by the injected fetch stack" — that guarantee does not
hold for IP literals.

**Fix**
In `ssrf::validate_url`, add an explicit literal check before any I/O:

```rust
if let Some(url::Host::Ipv4(ip)) = url.host() {
    if is_forbidden_ip(IpAddr::V4(ip)) { return Err(SsrfError::ForbiddenAddress(ip.to_string())); }
}
if let Some(url::Host::Ipv6(ip)) = url.host() {
    if is_forbidden_ip(IpAddr::V6(ip)) { return Err(SsrfError::ForbiddenAddress(ip.to_string())); }
}
```

Add a redirect-hop check for the same (the custom policy at `base.rs:83-92` validates the
scheme per hop but not the address). Validate `base_url` at
`admin/providers.rs::create_provider`/`update_provider` with `ssrf::validate_url` +
`resolve_checked` so a hostile provider row cannot be stored in the first place. Add a
regression test asserting `validate_url(&Url::parse("http://169.254.169.254/")?)` errors.

**Effort: S**

---

## 6. Live third-party credentials committed to the repository

**Severity: High** — CWE-798 (Use of Hard-coded Credentials), CWE-540 (Inclusion of Sensitive Information in Source Code)

**Evidence**

`deploy/local.env` (tracked — confirmed by `git ls-files deploy/`, last touched in
`e5cff29`):

```
TANKOVAULT_ANILIST__CLIENT_ID: "46552"
TANKOVAULT_ANILIST__CLIENT_SECRET: "<REDACTED-ANILIST-CLIENT-SECRET>"
TANKOVAULT_ANILIST__TOKEN_ENCRYPTION_KEY: <REDACTED-AES-256-KEY>
```

The second value is a 40-char AniList OAuth client secret for a specific registered
application (`client_id 46552`); the third is a base64 32-byte AES-256-GCM data-encryption
key consumed by `crates/auth/src/crypto.rs:47-53` (`SecretBox::from_base64_key`) and
`services/sync/src/main.rs:306` — the key that protects every user's AniList OAuth
access/refresh token at rest.

Related weak defaults in the same tree:
- `deploy/docker-compose.yml:100` — `TANKOVAULT_AUTH__JWT_SECRET: dev-jwt-secret-change-me`
  (22 bytes, below the 32-byte floor at `services/api/src/main.rs:107`; the floor is only
  enforced when `TANKOVAULT_PROFILE=production`, which the compose file never sets —
  `services/api/src/main.rs:114-119`).
- `deploy/docker-compose.yml:85` — `TANKOVAULT_SEED_ADMIN_PASSWORD: changeme12345`.
- `deploy/docker-compose.yml:220` — `TOKEN_ENCRYPTION_KEY` fallback of
  `AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=` (an all-zero key).
- `deploy/docker-compose.yml:115` — `TANKOVAULT_EMAIL__PASSWORD` default `change-me`.

**Exploit scenario**
Anyone who clones (or has ever cloned) the repository holds the AniList client secret and
can impersonate the TankoVault OAuth application: mount a consent-phishing flow under the
app's registered name, or — with the token-encryption key — decrypt any `external_accounts`
ciphertext obtained from a database backup and act on users' AniList accounts directly.
Because the values are in git history, rotation is the only remedy; deleting the file is
not.

**Fix**
1. Rotate the AniList client secret at AniList immediately, and rotate the
   token-encryption key (re-encrypt `external_accounts.access_token`/`.refresh_token`
   under the new key, or force a re-link).
2. `git rm --cached deploy/local.env`, add it to `.gitignore`, ship
   `deploy/local.env.example` with placeholders only. Purge history with
   `git filter-repo` if the repository is or will be public.
3. Make the production guard unconditional for the encryption key and the seed password:
   extend `validate_auth_secrets` (`services/api/src/main.rs:132`) to reject the known
   placeholder strings (`dev-jwt-secret-change-me`, the all-zero key, `changeme12345`) in
   **every** profile, not just production. A weak-secret check that a deployment can skip
   by forgetting one env var is not a check.
4. Add a secret scanner (gitleaks / `trufflehog`) as a CI job in `.github/workflows/ci.yml`
   alongside the existing `deny` and `audit` jobs.

**Effort: S** (repo) + **M** (key rotation / re-encryption)

---

## 7. Refresh cookie is not `Secure` by default; no CSP is sent anywhere

**Severity: Medium** — CWE-614 (Sensitive Cookie Without 'Secure'), CWE-1021, CWE-693

**Evidence**

`services/api/src/main.rs:73-74`:

```rust
#[serde(default)]
cookie_secure: bool,
```

`#[serde(default)]` on a `bool` is `false`. `deploy/docker-compose.yml` never sets
`TANKOVAULT_AUTH__COOKIE_SECURE`, so the reference deployment ships refresh cookies
without the `Secure` attribute (`services/api/src/auth.rs:643-649`):

```rust
let cookie = Cookie::build((REFRESH_COOKIE, raw_refresh))
    .http_only(true)
    .secure(state.cookie_secure)   // false
    .same_site(SameSite::Strict)
    .path(REFRESH_PATH)
```

`deploy/docker-compose.yml:140` also sets `TANKOVAULT_SECURITY__HSTS: "false"` and
`crates/config/src/lib.rs:574` defaults `hsts: false`.

No `Content-Security-Policy` is emitted by either server:
- `crates/service/src/http.rs:175-208` (`apply_security_headers`) sets `nosniff`,
  `X-Frame-Options`, `Referrer-Policy`, `Cross-Origin-Resource-Policy` and conditionally
  HSTS — no CSP.
- `services/frontend/src/main.rs:145-158` sets the same three on the SPA shell — no CSP,
  and no `Cache-Control` on `index.html` either, so an intermediary may cache the app shell.

**Exploit scenario**
A user on an untrusted network reaches the app over plain HTTP once (typo, HTTP link,
captive portal, SSL-strip). The `refresh_token` cookie — a 30-day credential
(`services/api/src/main.rs:92-94`) — is transmitted in clear and captured. With no HSTS,
the browser has no memory that this origin should be HTTPS-only, so the downgrade is not
prevented on the next visit either. The absence of CSP means any injected script in the
SPA (a compromised build artefact, a future `dangerous_inner_html` regression) can
exfiltrate the in-memory access token to an arbitrary origin with no browser-side block.

**Fix**
1. Default `cookie_secure` to `true` (`fn default_true` already exists in
   `crates/config/src/lib.rs:583`), with an explicit opt-out for local HTTP dev. Set
   `TANKOVAULT_AUTH__COOKIE_SECURE: "true"` and `TANKOVAULT_SECURITY__HSTS: "true"` in the
   deployed compose/helm values.
2. Rename the cookie to `__Host-refresh_token`; the `__Host-` prefix makes `Secure` +
   `Path=/` + no `Domain` browser-enforced, which a config toggle cannot undo. (Note this
   requires `Path=/` rather than the current `/v1/auth`.)
3. Add a CSP to `services/frontend/src/main.rs`'s `static_service` stack:
   `default-src 'self'; script-src 'self' 'wasm-unsafe-eval'; connect-src 'self';
   img-src 'self' https: data:; object-src 'none'; base-uri 'none'; frame-ancestors 'none'`.
   Adjust `img-src` for remote cover art. Add
   `Cache-Control: no-cache` on `index.html` and `immutable` on hashed assets.
4. Add a CSP to the API's `apply_security_headers` too:
   `default-src 'none'; frame-ancestors 'none'` — a JSON API needs nothing else.

**Effort: S**

---

## 8. `/v1/me/stream` skips the suspension/existence check and carries the access token in the URL

**Severity: Medium** — CWE-613 (Insufficient Session Expiration), CWE-598 (Information Exposure Through Query Strings)

**Evidence**

`services/api/src/me/notifications.rs:145-154` — the only route that authenticates without
the `AuthUser` extractor:

```rust
pub async fn stream(
    State(state): State<AppState>, Query(q): Query<StreamQuery>,
) -> ApiResult<Sse<...>> {
    let claims = tankovault_auth::verify_access_token(&state.jwt_secret, &q.access_token)?;
    let user_id = claims.user_id().ok_or(ApiError::Unauthorized)?;
    let bus = state.bus.clone().ok_or(ApiError::Unavailable)?;
    let subscriber = bus.subscribe_user_notifications(user_id.as_uuid()).await
```

Compare `services/api/src/state.rs:204-210`, which every other route goes through:

```rust
let principal = tankovault_db::repo::permissions::resolve(&state.pool, user_id).await?
    .ok_or(ApiError::Unauthorized)?;
if !principal.status.may_authenticate() { return Err(ApiError::Suspended); }
```

The doc comment at `services/api/src/me/notifications.rs:121-124` claims the token "is
verified exactly like a `Bearer` token and never logged". Neither clause holds: the
suspension and account-existence checks are absent, and the token rides in the URI, which
`TraceLayer::new_for_http()` (`crates/service/src/http.rs:113`) records as a span field,
`services/frontend/src/main.rs:170` also traces, and any reverse proxy or CDN in front
writes to its access log verbatim (`services/frontend/src/main.rs:187-190` explicitly
preserves the query string for exactly this reason).

**Exploit scenario**
An administrator suspends an abusive account, or a user deletes their account. Their
already-issued access token remains valid for up to 15 minutes, and an open (or newly
opened) `GET /v1/me/stream?access_token=…` keeps delivering their notification feed for
that whole window — the one route where "revoke now" does not mean now, defeating the
design intent documented at `crates/auth/src/token.rs:17-25`. Separately, the token
appears in proxy/CDN logs and in the browser's history; anyone with log access replays it
against `Authorization: Bearer` on any `/v1/me/*` route.

**Fix**
1. Resolve the principal and check `may_authenticate()` in `stream`, mirroring
   `state.rs:204-210`. Also poll the account state periodically and terminate the stream
   when it changes, since the stream outlives the token.
2. Replace the query-string token with a short-lived (30 s), single-use, stream-scoped
   ticket: `POST /v1/me/stream-ticket` returns an opaque id stored in Redis; `EventSource`
   passes `?ticket=…`; the handler consumes it. A leaked log line is then worthless.
3. Failing that, cap the stream's lifetime at the access token's `exp`.

**Effort: M**

---

## 9. Username may contain `@`, and login resolves `email = $1 OR username = $1` ambiguously

**Severity: Medium** — CWE-287 (Improper Authentication), CWE-706 (Use of Incorrectly-Resolved Name)

**Evidence**

`crates/db/src/repo/users.rs:100-108`:

```rust
"SELECT id, email, username, ... FROM users WHERE email = $1 OR username = $1",
```
…with `.fetch_optional(exec)` — if two rows match, Postgres returns an arbitrary one and
`fetch_optional` silently takes it (it does not error on multiple rows).

`migrations/0004_users.sql:4-5`:

```sql
email         citext NOT NULL UNIQUE,
username      citext NOT NULL UNIQUE,
```

Two **separate** unique constraints. Nothing prevents user B's `username` from equalling
user A's `email`. `services/api/src/auth.rs:659-676` (`validate_registration`) checks only
`username.len()` in 3..=32 — no character-class check, no `!username.contains('@')`.
`services/api/src/me/account.rs:53-70` (`patch_profile`) does not validate the username at
all, so the collision can also be created after the fact on an existing account.

**Exploit scenario**
An attacker registers with `username = "victim@example.com"` (the victim's real address,
which is discoverable from any public profile or a data breach). From then on
`POST /v1/auth/login {"login":"victim@example.com", ...}` matches two rows. Whichever
Postgres returns first decides the outcome: if the attacker's row wins, the victim's
correct password is rejected (`auth.rs:199-211`) and they are locked out with a plain
401 and no explanation; the attacker meanwhile authenticates into their own account with
the victim's identifier, which poisons the audit trail
(`auth.rs:186-192` records the identifier, not the resolved account). Under a plan change
or a table rewrite the winning row can flip, making the lockout intermittent and
undebuggable.

**Fix**
1. Reject `@` (and any non `[A-Za-z0-9_.-]`) in usernames in `validate_registration`, and
   apply the same validator in `patch_profile` and `admin::update_user`.
2. Add a database-level guarantee rather than relying on validation alone:
   `ALTER TABLE users ADD CONSTRAINT username_not_an_email CHECK (position('@' in username) = 0);`
3. Split the lookup: if the input contains `@`, match `email` only; otherwise match
   `username` only. Ambiguity then cannot arise regardless of stored data.
4. Add a regression test asserting a username that equals another account's email is
   rejected at registration.

**Effort: S**

---

## 10. Login discloses account existence through a timing side channel

**Severity: Medium** — CWE-208 (Observable Timing Discrepancy), CWE-204 (Observable Response Discrepancy)

**Evidence**

`services/api/src/auth.rs:182-198` — an unknown identifier returns before any hashing
happens; a known one pays the full argon2 cost:

```rust
let Some(creds) = tankovault_db::repo::users::find_credentials(&state.pool, login).await? else {
    audit_anonymous(..., "unknown_identifier", ...).await;
    return Err(ApiError::Unauthorized);          // ~1 ms
};
let ok = verify_password(&req.password, &creds.password_hash, &state.password_pepper)
```

`crates/auth/src/password.rs:16-24` uses `Params::default()` — 19 MiB, t=2, p=1, which on
typical hardware is 30-60 ms. The gap between the two branches is two orders of magnitude
and needs no statistics to read.

`forgot_password` (`services/api/src/auth.rs:414-440`) has the same shape at a smaller
magnitude: the known-address branch performs a token insert plus a `tokio::spawn`; the
unknown branch returns immediately. The uniform `202` response is defeated by the timing.

**Exploit scenario**
An attacker submits a candidate address with an arbitrary password and measures the
response time. <5 ms → no such account; >30 ms → the account exists. Iterating a breach
corpus enumerates the entire user base, which is the input to a credential-stuffing run
and, in a manga-tracking service, is itself sensitive (it links an email to a reading
profile). Finding 3 removes the rate limit that would otherwise slow this down.

**Fix**
Hash a fixed dummy PHC string on the not-found branch so both paths perform one argon2
verification:

```rust
const DUMMY_HASH: &str = "$argon2id$v=19$m=19456,t=2,p=1$…";   // generated once, constant
let Some(creds) = ...find_credentials(...).await? else {
    let _ = verify_password(&req.password, DUMMY_HASH, &state.password_pepper);
    audit_anonymous(...).await;
    return Err(ApiError::Unauthorized);
};
```

For `forgot_password`, move the whole body (including the DB write) into a
`tokio::spawn` and return `202` unconditionally, so response time is independent of the
address.

**Effort: S**

---

## 11. `panic = "abort"` with no panic-catching layer: one panicking request kills the replica

**Severity: Medium** — CWE-248 (Uncaught Exception), CWE-400

**Evidence**

`Cargo.toml:160-165`:

```toml
[profile.release]
opt-level = 3
lto = "thin"
codegen-units = 1
strip = true
panic = "abort"
```

`crates/service/src/http.rs:97-107` (`HttpStack::apply`) mounts compression, body limit,
timeout, rate limit, CORS, security headers, metrics, request-id and tracing — but no
`tower_http::catch_panic::CatchPanicLayer`. Grep confirms `CatchPanicLayer` appears nowhere
in the workspace.

Reachable arithmetic that overflows on attacker-controlled input:
`services/api/src/series.rs:113-114,131`

```rust
let limit = params.limit.clamp(1, 100);
let page  = params.page.or(params.cursor).unwrap_or(0).max(0);   // no upper bound
...
offset: page * limit,
```

`GET /v1/series?page=92233720368547758&limit=100` overflows `i64`. Release builds have
`overflow-checks = false` so this currently wraps to a negative `offset` and yields a
Postgres error → 500 rather than a crash; but the same expression panics under
`overflow-checks = true` (any debug or CI build), and with `panic = "abort"` a panic
anywhere in a handler terminates the **process**, not just the task, so every in-flight
request on that replica dies with it.

**Exploit scenario**
Any unauthenticated client that finds one panicking code path — an overflow, an
out-of-range slice, an `unwrap` on a `HeaderValue` — can restart the API replica at will
by repeating the request, producing a rolling outage that survives Kubernetes restarts
because the trigger is a request, not a state.

**Fix**
1. Add `CatchPanicLayer::new()` as the innermost layer in `HttpStack::apply` so a panic
   becomes a 500 for one request. This is orthogonal to `panic = "abort"` (which governs
   unwinding cost) but requires `panic = "unwind"` to work — reconsider the abort profile,
   or accept that a panic is fatal and treat every panic path as a security bug.
2. Use `saturating_mul` / `checked_mul` at `series.rs:131` and clamp `page` to a sane
   ceiling (e.g. `.clamp(0, 100_000)`).
3. Enable `overflow-checks = true` in the release profile — the cost is negligible for
   this workload and silent wrapping in a pagination offset is worse than a caught error.

**Effort: S**

---

## 12. Rate-limit buckets are per-exact-IP: trivially defeated by an IPv6 allocation

**Severity: Medium** — CWE-307

**Evidence**

`crates/service/src/ratelimit/mod.rs:326-343`:

```rust
fn key(&self, req: &Request) -> String {
    if let Some(Principal(id)) = req.extensions().get::<Principal>() { return format!("u:{id}"); }
    if self.trust_forwarded_for { if let Some(ip) = forwarded_client_ip(req.headers()) { return format!("ip:{ip}"); } }
    req.extensions().get::<ConnectInfo<SocketAddr>>()
        .map_or_else(|| "ip:unknown".to_owned(), |ConnectInfo(addr)| format!("ip:{}", addr.ip()))
}
```

The `Principal` extension is **never inserted** anywhere in the workspace — grep for
`Principal(` returns only its definition (`ratelimit/mod.rs:50`) and this read
(`:327`). So the per-user path documented at `ratelimit/mod.rs:29-31` is dead, and all
traffic — authenticated or not — is bucketed by exact IP. An IPv6 client with a routed
/64 (the standard residential and VPS allocation) has 2^64 distinct keys.

**Exploit scenario**
An attacker on a €4/month VPS with a /64 rotates the source address per request and gets
the auth budget (10/min) once per address — effectively unlimited password guessing and
reset-email flooding, with no forged headers required. Each distinct address also creates a
permanent Redis key, so the counter store grows without bound.

**Fix**
1. Normalise the key: for IPv6, mask to /64 (or /56) before formatting; for IPv4, use the
   full address. `format!("ip:{}", canonicalise(addr.ip()))`.
2. Insert `Principal(user_id)` from an authentication middleware so authenticated traffic
   is limited per account, and add a per-**account** counter to the login handler itself
   (keyed on the submitted `login` identifier, not the IP) with progressive delay — the
   only control that actually bounds guessing against one target.
3. Cap the number of distinct in-memory/Redis keys, or use an approximate structure, so
   the counter store cannot be used as a memory-exhaustion vector.

**Effort: M**

---

## 13. OpenAPI document and Scalar UI are served unauthenticated

**Severity: Low** — CWE-200 (Exposure of Sensitive Information)

**Evidence**

`services/api/src/lib.rs:179-182`:

```rust
.apply(
    router
        .merge(Scalar::with_url("/scalar", api))
        .with_state(state)
```

Merged inside the middleware stack but with no auth gate and no feature flag — the route
table at `services/api/src/lib.rs:180` deliberately excludes `/scalar` from
`route_features()`. `GET /scalar` therefore returns the full schema for every route
including `/v1/admin/users/{id}/permissions`, `/v1/admin/privacy/requests/{id}/export`,
the permission names, and the request/response shapes.

**Exploit scenario**
An attacker enumerates the complete admin surface, permission vocabulary and exact request
bodies without a single failed probe, then targets exactly the endpoints worth targeting.
This is reconnaissance, not compromise — but it removes the entire discovery cost of every
other finding in this document.

**Fix**
Gate the Scalar route behind a config toggle defaulting to off in production, or behind
`AuthUser::require(Permission::SystemStats)`. Keep it on in dev.

**Effort: S**

---

## 14. Username is interpolated unescaped into transactional HTML email

**Severity: Low** — CWE-79 / CWE-116 (Improper Encoding or Escaping of Output)

**Evidence**

`services/api/src/mailer.rs:21-27` and `:41-48`:

```rust
let html = format!(
    "<p>Hi {username},</p>\
     <p>Welcome to <strong>TankoVault</strong>! ...
```

`username` is user-controlled with only a length check
(`services/api/src/auth.rs:665-669`) and, via `patch_profile`
(`services/api/src/me/account.rs:58-62`), no check at all. No HTML escaping is applied.
The `{link}` interpolations at `mailer.rs:45,67` are server-built and safe.

**Exploit scenario**
A user sets `username` to
`x</p><a href="https://evil.tld/reset">Click here to reset your password</a><p>` and
triggers the verification or welcome email. The rendered message contains an attacker-chosen
link inside a message that is genuinely from the service, with a valid DKIM signature —
a high-credibility phishing primitive. Impact is limited because the message only ever goes
to the address on that same account; it becomes materially worse the moment any
admin-facing or shared email template includes a username.

**Fix**
HTML-escape every interpolated value in `mailer.rs`. A three-line
`fn esc(s: &str) -> String` covering `& < > " '` is sufficient here; a templating crate
with auto-escaping (`askama`, `minijinja`) is the durable answer. Also restrict the
username character class per finding 9.

**Effort: S**

---

## 15. GDPR self-export includes audit rows that name other data subjects

**Severity: Low** — CWE-359 (Exposure of Private Personal Information to an Unauthorized Actor)

**Evidence**

`crates/db/src/repo/privacy.rs:61-62`:

```sql
'audit_entries', (SELECT coalesce(json_agg(to_jsonb(a) ORDER BY a.created_at), '[]'::json)
                    FROM audit_log a WHERE a.actor_id = $1),
```

Whole `audit_log` rows, including `target` and `detail`. Those details contain third
parties' personal data when the exporting user is an operator —
`services/api/src/admin/users.rs:179-185`:

```rust
audit(&state, &user, "user.update", &id.to_string(),
      &serde_json::json!({ "username": username, "email": email })).await;
```

and `services/api/src/admin/privacy.rs:323-324`, which records
`{ "subject_id": subject.as_uuid(), "kind": request.request.kind }`.

**Exploit scenario**
An operator (or a former operator whose grants were revoked but whose historical actions
remain) calls `GET /v1/me/export` and receives a file containing other users' email
addresses, username changes, and GDPR-request identifiers. Under Art. 15(4) an access
request must not adversely affect the rights of others; this export does. It is also the
kind of file people forward by email.

**Fix**
Project the audit rows rather than dumping them: export `created_at`, `action`, `outcome`
and the `target` **only when `target = the subject's own id`**; drop `detail` entirely, or
whitelist keys per action. The compliance goal — showing the subject what was recorded
about them — is met without disclosing the other party.

**Effort: S**

---

## 16. Dependency posture: an unfixable advisory sits in the tree against an empty ignore list

**Severity: Low** (informational) — CWE-1104 (Use of Unmaintained Third Party Components)

**Evidence**

`Cargo.lock:2453` — `jsonwebtoken 10.3.0` (`Cargo.lock:2440`) depends on `rsa`, resolved to
`rsa 0.9.10` (`Cargo.lock:3706`). RUSTSEC-2023-0071 (Marvin attack — timing side channel
enabling RSA key recovery) affects all `rsa` 0.9.x and has **no fixed upstream release**.

`deny.toml:5-9`:

```toml
[advisories]
version = 2
ignore = []
```

`.github/workflows/ci.yml:141-157` runs both `cargo-deny` (EmbarkStudios action) and
`cargo-audit` (`rustsec/audit-check@v2`). With an empty `ignore` list and an unfixable
advisory present, either those jobs are red and being merged past, or the advisory is not
being surfaced. Both are worth knowing. **UNVERIFIED**: I did not execute `cargo deny` /
`cargo audit` (no network), so I cannot confirm the current CI colour.

Exploitability here is nil: `crates/auth/src/token.rs:84` pins `Algorithm::HS256`, so the
RSA code path is never entered. The risk is process, not cryptography.

Also noted:
- `paste 1.0.15` (`Cargo.lock`) — RUSTSEC-2024-0436, unmaintained. Transitive; no fix.
- `chromiumoxide 0.9.1` drives a `--no-sandbox` Chrome
  (`services/render/src/browser.rs:60-62`, `deploy/docker-compose.yml:246`). Combined with
  finding 2, a renderer bug on an attacker-chosen page is a container-level compromise.

**Fix**
1. Add an explicit, dated, justified entry to `deny.toml`'s `ignore` list for
   RUSTSEC-2023-0071 and RUSTSEC-2024-0436, with the reasoning ("HS256 only; RSA path
   unreachable") — so the gate stays green for real regressions and the exception is
   reviewed.
2. If `jsonwebtoken` exposes a feature to drop RSA support, enable `default-features =
   false` and select only HMAC, removing `rsa` from the graph entirely — the cleanest fix.
3. Give the render container a real sandbox (seccomp + user namespace) rather than
   `--no-sandbox`.

**Effort: S**

---

# Verified safe

Controls examined and found correct. Listed so the document records what holds, not only
what does not.

**JWT and token handling**
- `crates/auth/src/token.rs:84` — `Validation::new(Algorithm::HS256)` restricts the
  accepted algorithm set to HS256 alone. `alg:none` and RS256/HS256 confusion are both
  closed; `validate_exp` is explicitly on. Tested at `token.rs:149-161`.
- `crates/auth/src/token.rs:29-43` — the access token carries **no** authorization claim.
  `AuthUser` (`services/api/src/state.rs:204`) re-resolves permissions from the database on
  every request, so a revoked grant takes effect immediately. Pinned by a test at
  `token.rs:126-146` that fails if a `role`/`perms`/`scope` claim is ever added. This is a
  materially better design than the common role-in-token pattern.
- `crates/auth/src/token.rs:95-110` — refresh tokens are 256 bits from `rand::thread_rng`,
  base64url-encoded, and only their SHA-256 is persisted.

**Refresh-token rotation and reuse detection**
- `services/api/src/auth.rs:304-330` — presenting an already-rotated token revokes the
  whole family, emits a `tracing::warn!` and an audit record. Correct reuse-detection
  semantics.
- `services/api/src/auth.rs:336` — rotation revokes the presented token before minting the
  successor.
- `services/api/src/auth.rs:342-358` — a suspension applied mid-session terminates the
  family at refresh time rather than waiting for cookie expiry.
- Session fixation: `login`, `register` and `verify_email` all mint a fresh
  `Uuid::now_v7()` family (`auth.rs:139,271,550`); only `refresh` reuses one
  (`auth.rs:360`). Correct.

**Password and secret cryptography**
- `crates/auth/src/password.rs:16-24` — argon2**id**, v0x13, `Params::default()` = m=19456
  KiB, t=2, p=1, which meets the current OWASP minimum. Per-hash random salt from `OsRng`
  (`password.rs:35`).
- The pepper is supplied as argon2's `secret` input (`Argon2::new_with_secret`), not
  concatenated — the correct keyed-hash construction. Backward compatibility with an empty
  pepper is tested (`password.rs:96-102`).
- `crates/auth/src/crypto.rs` — AES-256-GCM with a fresh random 96-bit nonce per message,
  prepended to the ciphertext; length-checked on open; `Debug` is manually implemented to
  print `SecretBox(<redacted>)` (`crypto.rs:91-96`). Tampering, wrong-key and truncation
  are all tested.

**Password reset and email verification**
- Reset and verification tokens reuse the 256-bit generator and are stored as SHA-256 only
  (`services/api/src/auth.rs:421-422,586-587`).
- TTLs are bounded and sensible: 1 h reset (`auth.rs:28`), 24 h verification (`auth.rs:32`).
- Single-use is enforced by an atomic `used_at` flip whose affected-row count is checked,
  closing the concurrent-redemption race (`auth.rs:478-484`, `:538-544`).
- `reset_password` revokes every session for the user after the change
  (`auth.rs:489`) — a stolen refresh token dies with the old credential.
- `forgot_password` and `resend_verification` both return a uniform `202` regardless of
  whether the address exists (`auth.rs:439`, `:580`). (The timing channel is finding 10;
  the response-shape control itself is correct.)

**Authorization**
- Every handler in `services/api/src/admin/*` calls `user.require(...)` or
  `require_all(...)` as its first statement. Verified exhaustively across
  `flags.rs`, `merge.rs`, `privacy.rs`, `providers.rs`, `scans.rs`, `sync.rs`,
  `system.rs`, `users.rs` — 45 handlers, no gaps.
- `AuthUser::require_all` (`services/api/src/state.rs:148-171`) audits every denial with
  the full list of missing capabilities. Refusals are observable, which is unusual and
  right.
- Suspension is checked in the extractor **before** any capability
  (`state.rs:208-210`), so no permission can override it.
- `guard_not_self` (`admin/users.rs:581-600`) and `guard_not_last_administrator`
  (`admin/users.rs:607-640`) prevent self-escalation and administrative lockout.
- No IDOR found in `/v1/me/*`: every path-id handler scopes by the token's own user in the
  SQL — `revoke_session(pool, user.user_id, id)` (`me/account.rs:141`),
  `gdpr::cancel_own(pool, id, user.user_id)` (`me/privacy.rs:306`),
  `get_pending_conflict(pool, user_id, conflict_id)` (`services/sync/src/engine.rs:292`).
  Non-owned ids yield 404, not 403 — correct, since 403 would confirm existence.

**SQL injection**
- No dynamic SQL anywhere in `crates/db/src/repo/`. Every statement is a compile-time
  `sqlx::query!` / `query_as!` / `query_scalar!` macro with bind parameters.
- The dynamic sort in `catalog.rs:948` is a `match filter.sort.as_deref()` over a closed
  set of literal `query_as!` invocations — no identifier interpolation is possible.
- The only `format!` calls near SQL (`repo/sync.rs:726,823`) build `%…%` LIKE *values*,
  which are then bound as parameters.
- Pagination is clamped where it is caller-supplied: `series.rs:113`
  (`.clamp(1, 100)`), `admin/users.rs:78` (`.clamp(1, MAX_PAGE)` with `MAX_PAGE = 200` at
  `:42`). Other listings use fixed constants.

**Edge hardening**
- `crates/service/src/http.rs:120-165` — CORS is an explicit allowlist, default empty
  (`crates/config/src/lib.rs:499-511`), replacing a previous `CorsLayer::permissive()`.
  Unparseable origins are dropped with a warning rather than widening the policy, and this
  is tested (`http.rs:472-486`). `allow_credentials` is opt-in and cannot combine with a
  wildcard.
- `crates/service/src/http.rs:175-208` — `nosniff`, `X-Frame-Options: DENY`,
  `Referrer-Policy: no-referrer`, `Cross-Origin-Resource-Policy: same-origin` on **every**
  response including 429s and timeouts, set with `insert` not `append`.
- 1 MiB body cap (`config/lib.rs:557-559`), 30 s request timeout (`:560-562`), both
  applied at `http.rs:101-106`. No inbound decompression layer is mounted, so request-side
  zip bombs are not reachable.
- Ops probes are mounted **outside** the stack (`services/api/src/lib.rs:186`), so a rate
  limit cannot make a healthy replica look unhealthy.
- `services/frontend/src/main.rs:246-259` correctly strips hop-by-hop headers in both
  directions per RFC 9110 §7.6.1, and `ServeDir` (tower-http) rejects `..` traversal —
  I found no path-traversal vector in the static branch.

**Outbound crawler safety**
- `crates/fetch/src/base.rs:83-92` — redirects capped at 5, and each hop's scheme is
  re-validated (`http`/`https` only), so a redirect to `file:`/`gopher:` is stopped.
- `crates/fetch/src/base.rs:152-161` — the response body is **streamed** with a hard 8 MiB
  cap checked per chunk, applied after `wreq`'s decompression, so a decompression bomb
  errors with `BodyTooLarge` instead of exhausting memory.
- Connect and whole-request timeouts are mandatory constructor arguments
  (`base.rs:76-79`), defaulted to 10 s / 30 s (`builder.rs:78-79`).
- `crates/fetch/src/ssrf.rs:44-77` — the forbidden-range table is thorough and correct:
  RFC1918, loopback, link-local (incl. `169.254.169.254`), CGNAT `100.64/10`, benchmarking
  `198.18/15`, TEST-NET 1-3, `240/4`, IPv6 ULA `fc00::/7`, link-local `fe80::/10`,
  documentation `2001:db8::/32`, and IPv4-mapped/compatible unwrapping. Well tested
  (`ssrf.rs:131-195`). The gap is the delivery mechanism (finding 5), not the table.
- `crates/domain/src/link.rs:39-50` — `resolve_link` rejects any non-`http`/`https` scheme
  on both the pass-through and the base, so a provider cannot inject a `javascript:` URL
  into a link the frontend renders.

**Logging and audit**
- Swept every `tracing::{info,warn,error,debug}!` in `services/api/src`,
  `crates/service/src`, `crates/db/src`, `services/sync/src`. No passwords, password
  hashes, tokens, cookies, request bodies or email addresses are logged. Errors are logged
  as `error = %e` with the value returned to the client reduced to a generic string.
- `services/api/src/error.rs:1` and `:104-108` — internal errors map to a fixed
  `"internal server error"` problem body; the underlying `DbError`/`AuthError` is logged
  server-side only (`error.rs:136,148`). No stack traces or SQL text reach the client.
- `crates/config/src/lib.rs` — no `Debug`-printing of any config struct was found at any
  call site, so the derived `Debug` on `DatabaseConfig`/`EmailConfig` (which hold the DSN
  password and SMTP password) is not currently a leak. It remains a latent one: a future
  `tracing::debug!(?cfg)` would dump both. Consider manual redacting `Debug` impls.
- `crates/service/src/audit.rs:63-67` + `crates/config/src/lib.rs:341-347` — client IP
  and User-Agent are off by default and filtered in the sink rather than at the call site,
  so a handler cannot accidentally retain an IP by constructing an event differently. GDPR
  Art. 5(1)(c) data-minimisation applied correctly. Retention sweep defaults to 365 days
  with a bounded 10 000-row batch (`services/api/src/main.rs:100`).
- `services/api/src/state.rs:99-111` — `ClientContext` deliberately uses the connection
  peer address rather than `X-Forwarded-For` for audit records, with the reasoning
  documented. Correct: an audit record naming a client-supplied address is worse than one
  naming none.

**GDPR paths**
- `crates/db/src/repo/privacy.rs:37-73` — the export is a single server-side
  `json_build_object` (one consistent snapshot, no interleaving with concurrent writes),
  every subquery is scoped `WHERE … = $1`, and credentials are stripped column-by-column:
  `- 'password_hash'`, `- 'token_hash'`, `- 'access_token' - 'refresh_token'`. No other
  user's rows are reachable (finding 15 concerns the subject's own audit rows, which name
  third parties).
- `crates/db/src/repo/privacy.rs:92-97` — erasure is one `DELETE FROM users`, relying on
  `ON DELETE CASCADE`, with `audit_log.actor_id` set to `NULL` instead — retaining an
  unlinkable record of privileged actions while destroying the link to the person. The
  legal reasoning (Art. 6(1)(f)) is documented at `privacy.rs:86-91`.
- `services/api/src/me/privacy.rs:118-159` — self-erasure requires the username typed back
  and audits both the refusal and the action, recording **before** the delete so the
  explaining record survives.

**Transactional email**
- `crates/email/src/lib.rs:214-231` — every recipient is parsed into a lettre `Mailbox`
  before use, which rejects embedded CR/LF. SMTP header injection via a crafted address is
  closed. Subjects are compile-time constants.

**Frontend**
- `web/frontend/src/state/mod.rs:3` — the access token lives **only in memory**, never in
  `localStorage`. `localStorage` is used only for the theme and language preferences
  (`state/prefs.rs`, `i18n.rs`) and a per-series pinned-source id (`views/series/mod.rs:69`).
- `web/frontend/src/state/jwt.rs` — the unverified client-side decode is safe by
  construction, because the token carries no authorization claims to forge (documented at
  `jwt.rs:1-8`). Malformed input returns `None` rather than panicking, and this is tested.
- The single `dangerous_inner_html` (`web/frontend/src/icons.rs:93`) is fed by
  `path_for(icon) -> &'static str` (`icons.rs:99`), a match over compile-time constants.
  No user data reaches it.
