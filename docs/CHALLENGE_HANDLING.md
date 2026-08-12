# Challenge handling — remediation plan

The bot-management tier (design §9) had a detection defect that took every Cloudflare-fronted
provider with **JS Detections** enabled permanently dark on full-document pages. The defect itself
is fixed on this branch; this document is the plan for everything the investigation found around
it, written so a worker with no context can execute it.

Read [`docs/ENGINEERING_GUIDE.md`](ENGINEERING_GUIDE.md) first. Every workstream below assumes the
house rules — `#[expect]` over `#[allow]`, short comments except where they carry risk, and a fix
that could silently return gets a test whose doc comment says what the bug was.

---

## 0. Status

| # | Workstream | State | Blocking? |
|---|---|---|---|
| W1 | Detection: the JS Detections beacon is not an interstitial | **landed on this branch** | — |
| W2 | Session lifecycle: cache after validation, evict on failure | open | no |
| W3 | Observability: make a dark provider visible | open | no |
| W4 | Cookie scoping: a clearance cookie must not leave its host | open | wire change |
| W5 | Fingerprint coherence between the solver browser and `wreq` | open | needs W3's evidence |
| W6 | A WAF deny is not a solvable challenge; per-provider breaker | open | wire tolerance first |
| W7 | Shared session store across worker replicas | open | only with replicas > 1 |

W2, W3 and W6's breaker are independent and can land in any order. W4 and W6's classification both
change the `/v1/solve` wire and share a deployment-ordering constraint (§4.4). W5 should not be
attempted before W3 is deployed — it is a fingerprint-matching change justified only by numbers
nothing currently measures.

---

## 1. What went wrong, and how to reproduce it

`detect_challenge_body` matched the bare substring `/cdn-cgi/challenge-platform`. That path prefix
is **not** unique to an interstitial: Cloudflare injects its JS Detections beacon,
`/cdn-cgi/challenge-platform/scripts/jsd/main.js`, into ordinary content pages on any zone with the
feature enabled. A fully rendered `200` therefore classified as `ChallengeKind::CloudflareJs`.

The classifier is consulted twice per fetch, and the false positive is fatal at both:

1. [`solving.rs:136`](../crates/fetch/src/solving.rs:136) — the direct `200` is read as an
   interstitial, so a 30–60 s solve is spent on a page already in hand.
2. [`solving.rs:168`](../crates/fetch/src/solving.rs:168) — the solver returns that same real page,
   the "did the solver hand back the interstitial?" guard fires, and the fetch fails as
   `unsolved challenge: CloudflareJs`. No solver back-end could ever have won.

Reproduce against the live site (all three are current as of 2026-08-12):

```bash
curl -s -o /dev/null -w '%{http_code}\n' https://weebcentral.com/series/01J76XY8ZFKR126Q1NDQQ44GAT/Atsu-Atsu-Trattoria
```

| Request | Result | Carries the beacon |
|---|---|---|
| no `User-Agent` | `403`, Cloudflare "Sorry, you have been blocked" (WAF deny, not a challenge) | yes |
| browser `User-Agent` | `200`, ~59 KB of real series markup | **yes** |
| `/search/data?…` (htmx fragment) | `200`, ~195 KB | no |

The last row is why the symptom looked provider-specific rather than systemic: WeebCentral's
catalogue and latest-feed scans read fragments and kept working, while every series and chapter
page — full documents — failed. Any provider whose zone has JS Detections on had the same defect.

### W1, the landed fix

`/cdn-cgi/challenge-platform` is now a **prerequisite**, not a marker: only the orchestration entry
point (`…/orchestrate/`, `chl_page`) counts, alongside the untouched `cf_chl_opt`,
`<title>Just a moment` and Turnstile markers.

- [`crates/solver/src/detection.rs`](../crates/solver/src/detection.rs) — `loads_challenge_orchestration`, the narrowed branch, the doctest contract, and `the_js_detections_beacon_is_not_a_challenge`
- [`crates/solver/tests/prop_detection.rs`](../crates/solver/tests/prop_detection.rs) — the marker generator emits the orchestration URL and the beacon as separate fragments, so the differential property still reaches the `CloudflareJs` branch
- [`fuzz/dictionaries/solver.dict`](../fuzz/dictionaries/solver.dict) and the `cloudflare-jsd-beacon-on-content` seed
- [`docs/design.md`](design.md) — the marker list in the spec, which is what seeded the mistake

The real `403` block page now classifies as `CloudflareManaged` through the status+header fallback,
which is the more accurate label and still buys a solve. That is deliberate: a residential-proxy
tier is the only thing that can beat an IP-level deny. W6 refines it.

**Do not widen these markers again.** If a provider is later found challenging in a shape none of
them catch, add the *specific* new marker with a test carrying the captured markup — never a
substring that a content page can also contain, and never a body-size heuristic, which fails open
on a large interstitial and closed on a small page.

---

## 2. Environment prerequisite — read before starting

`cargo check` on any crate that pulls `wreq` **fails on the current Windows host**, and did before
any of this work: `btls-sys v0.5.6` (BoringSSL) dies in its CMake `TryCompile`, with MSBuild
crashing inside `TrackedVCToolTask` under the VS 18 BuildTools `v180` toolset. Disk is not the
cause (282 GB free). That covers `tankovault-solver`, `tankovault-fetch`, `tankovault-adapters` and
every service — i.e. everything this plan touches.

Options, best first:

1. **Repair the toolchain.** Install the current MSVC build tools and confirm
   `cargo check -p tankovault-solver` completes. This is the only way to run the real suites
   locally, and W2/W4/W5 all need `tankovault-fetch` to compile.
2. **Verify in Docker**, using the deploy image build, which is where BoringSSL builds today.
3. **Isolated harness** for a pure-logic module, as a last resort. `detection.rs` imports one type;
   copying it into a scratch crate beside a stub `ChallengeKind` compiles in seconds and runs the
   real unit tests, doctests and property tests. This is how W1 was verified. It proves the module,
   not the crate, and must be reported that way.

Whatever you use, report verification as scoped: name the command you ran and state plainly that
`xtask ci` was not run.

---

## 3. Workstreams

### W2 — A session is cached before anything has validated it

**Problem.** [`solving.rs:155`](../crates/fetch/src/solving.rs:155) writes the solved session to the
store, and only then checks whether the returned HTML is still an interstitial. A solve that failed
therefore leaves its cookies and user-agent cached for the full TTL, and every later request to that
provider replays them. Symmetrically, nothing evicts a session that keeps coming back challenged:
the replay path at [`solving.rs:196`](../crates/fetch/src/solving.rs:196) errors and leaves the
proven-stale session in place.

**Rule to implement.** A session is stored only once something has succeeded with it — the solver's
own page passed the interstitial guard, or the replay it enabled returned a clean response. Any
request that carried a session and was still challenged evicts it.

**Changes.**

- `SessionStore` gains `async fn invalidate(&self, provider: &str)`; implement for
  `InMemorySessionStore` (and for the Redis store if W7 has landed).
- Move the `store.put` below the `detect_challenge_body(&html)` guard.
- On the no-HTML path the replay needs the session value, not the store entry — keep
  `apply_session(req, &session)` and write to the store only after `detect_challenge(&resp2)` comes
  back `None`.
- Before delegating to the solver, if the request that was challenged had a cached session applied,
  `invalidate` it. Otherwise a solver error leaves the stale session behind.

**Tests** (`crates/fetch/src/solving.rs`, each with a doc comment naming the bug):

- `a_failed_solve_does_not_leave_a_session_behind` — solver returns the interstitial; the fetch
  errors and `store.get(provider)` is `None`.
- `a_replay_that_is_still_challenged_evicts_the_session`.
- `a_solved_page_caches_its_session` — the positive case, so the fix cannot be "never cache".

A recording `SessionStore` double (counting `put`/`invalidate`) is needed; put it in the existing
`mod tests`, not in `test-support`, until a second crate wants it.

**Gate.** `cargo test -p tankovault-fetch solving`.

---

### W3 — A permanently dark provider was invisible

**Problem.** The only signal this defect produced was a per-fetch error string. Nothing counted
challenges by kind, and nothing counted the outcome of a solve *as the fetch layer judged it* —
`solve_attempts_total` lives in [`crates/solver/src/http.rs`](../crates/solver/src/http.rs) and
reports `ok` for a solve that returned the interstitial, because from the service's side it did
succeed. WeebCentral sat at 100 % unsolved from the first scan and no panel could show it.

**Changes** — all in `crates/fetch/src/solving.rs`, following the `metrics::counter!` style already
used in [`base.rs:137`](../crates/fetch/src/base.rs:137):

| Metric | Labels | Incremented |
|---|---|---|
| `challenge_detections_total` | `provider`, `kind` | every positive `detect_challenge` |
| `challenge_solves_total` | `provider`, `kind`, `result` (`solved`/`unsolved`/`error`) | after each solve; `unsolved` is precisely the case that hid this bug |
| `challenge_session_replays_total` | `provider`, `result` (`ok`/`challenged`) | whenever a cached session was applied — this is also W5's evidence |

Cardinality is bounded: `provider` already labels `provider_fetch_total`, `kind` has four values.

**Mandatory, and the compiler cannot see it.** Every emitted series needs a row in
`tankovault_service::metrics::CATALOGUE` and a `names::*` constant beside it
([`crates/service/src/metrics.rs`](../crates/service/src/metrics.rs)) — `name`, `kind`, `unit`,
`emitted_by`, `help`. `xtask repo-lint`'s `metrics-catalogue` rule fails both ways: a counter with
no row, and a row nobody emits. `crates/fetch` sits below `tankovault-service` in the graph and so
spells its names as **literals** rather than importing the constants, exactly as
[`base.rs:137`](../crates/fetch/src/base.rs:137) does; the lint is what keeps the two spellings
equal, and it also rejects a name that is neither a literal nor a `names::*` constant.

**Docs.** Add the three rows to the **Fetch tier** table in
[`docs/OBSERVABILITY.md`](OBSERVABILITY.md) (§1), emitted by `worker`, and check §4 "What is not
measured" for a claim that these three now falsify.

**Follow-up, different repo.** Dashboards and alert rules live in `TimSchoenle/helm-charts` under
`charts/tankovault/`. The alert worth adding there:
`challenge_solves_total{result="unsolved"} / challenge_detections_total > 0.5` sustained 30 m, per
provider — "this provider is dark and every scan is paying full solve cost to discover it". That is
a separate PR against that repo; note it in the PR description rather than leaving it implied.

---

### W4 — A clearance cookie is replayed to hosts it was never issued for

**Problem.** `ScrapeResult`'s cookie type
([`trawl.rs:79`](../crates/solver/src/trawl.rs:79)) decodes only `name` and `value`, discarding
TRAWL's `domain`. Sessions are then keyed by **provider slug alone**
([`solving.rs:131`](../crates/fetch/src/solving.rs:131)) and `apply_session` attaches the whole jar
to every request that provider makes. A `cf_clearance` issued for `weebcentral.com` is therefore
sent to whatever other host that provider's pages point at — image CDNs on unrelated domains
included. That is a session credential leaving its origin, so it is a security fix, not a tidy-up,
and its rationale earns a real comment (rule 4a).

**Changes.**

- `SolveOutcome.cookies` becomes `Vec<SolvedCookie>` — `{ name, value, domain: Option<String> }`,
  `domain` defaulted. Carry the domain through `TrawlSolver` from TRAWL's payload.
- Key the store by `(provider, host)`, host taken from the *request* URL.
- `apply_session` sends only cookies whose domain matches the request host: exact match, or a
  leading-dot domain matched as a suffix on a label boundary. A cookie with no domain belongs to the
  host that was solved and goes nowhere else.

**Wire compatibility.** `/v1/solve` is produced by two services — `challenge-solver` and `render`,
both mounting the router from [`crates/solver/src/http.rs`](../crates/solver/src/http.rs) — and
consumed by the worker. A tuple → struct change is **not** backward compatible in serde. Give the
worker a tolerant decoder for one release (an untagged enum accepting both `["name","value"]` and
`{"name":…,"value":…,"domain":…}`), deploy the solver services first, then drop the tuple arm in the
following release. Write that sequence into the PR description; nothing in CI can catch a skew that
only exists between two running versions.

**Tests.**

- A cookie for `weebcentral.com` is not attached to a request for another host.
- A domain-less cookie is scoped to the solved host.
- A subdomain cookie (`.example.test`) matches `a.example.test` but not `notexample.test`.
- The tolerant decoder accepts both wire shapes (`crates/fetch/src/solver_client.rs`).

---

### W5 — The replayed session may not be usable by the client replaying it

**Problem.** `cf_clearance` is bound to the user-agent, the TLS/HTTP2 fingerprint **and** the egress
IP of the browser that earned it. We replay the solver's user-agent
([`base.rs:167`](../crates/fetch/src/base.rs:167)) over `wreq`'s fixed `Profile::Chrome149`
handshake ([`base.rs:48`](../crates/fetch/src/base.rs:48)). If TRAWL's bundled Chrome drifts from
that profile family, every session is rejected on first replay and each fetch silently pays a fresh
solve — and, as this module's own doc comment says, a browser UA over a non-matching handshake is a
*stronger* bot signal than no disguise.

Worse, TRAWL's tier ladder ends in a **residential proxy**. A session won there is bound to the
proxy's IP and can never work from the worker's egress, so caching it guarantees a replay failure.
`ScrapeResult` currently ignores the `tier` field that would say so
([`trawl.rs:60`](../crates/solver/src/trawl.rs:60)).

**Plan, evidence first.**

1. Ship W3's `challenge_session_replays_total` and read it for a week.
2. Decode `tier` from TRAWL's response and surface it on `SolveOutcome`. Do not cache a session
   whose winning tier was the proxy — use the returned HTML for that one fetch and let the next
   request solve again. Cheap, and it removes a guaranteed-stale class of session.
3. Only if the numbers still justify it: derive the emulation profile from the solved user-agent
   (parse the Chrome major, pick the nearest `wreq_util::Profile`) and, when no profile matches,
   decline to replay rather than send a mismatched pair. Note the cost — `BaseHttpFetcher` builds
   its client once per provider, so a per-session profile means a small client cache keyed by
   profile, not a rebuild per request.

---

### W6 — A WAF deny is not a challenge, and a blocked provider should stop paying for one

**Problem.** Cloudflare's "Attention Required! / Sorry, you have been blocked" page (error 1020) is a
deny, not a puzzle. Today it reaches the managed-challenge fallback and every queued task for that
provider spends the full TRAWL tier ladder — 30–60 s each — rediscovering the same verdict.

**Changes.**

- Classify it: `<title>Attention Required! | Cloudflare`, `Sorry, you have been blocked`,
  `error code: 1020`. Keep the *one* solve attempt (the proxy tier is the only thing that can beat
  an IP deny) but make the label honest.
- Add a per-provider circuit breaker in `SolvingFetcher`: after N consecutive unsolved outcomes,
  short-circuit to `FetchError::Challenge` for T minutes without calling the solver. Defaults worth
  starting from: N = 3, T = 15 min, both config-backed. Count it —
  `challenge_breaker_open_total{provider}` — or the breaker becomes the next invisible failure; the
  catalogue duty in W3 applies to that name too.
- Config surface changed → update [`docs/CONFIGURATION.md`](CONFIGURATION.md);
  `cargo run -p xtask -- config-docs` prints the current surface.

**Wire tolerance, do this first.** `SolveRequest.kind` is `Option<ChallengeKind>` and crosses
worker → solver. An unknown variant string fails deserialization of the **whole request**, so a new
variant deployed to workers before solvers breaks every solve. `#[serde(other)]` does not help here
— serde allows it only on unit variants of internally or adjacently tagged enums, and this is a
plain externally tagged one. Add a `deserialize_with` on the field that maps an unrecognised string
to `None` (the value is advisory; `TrawlSolver` deliberately does not forward it, as
`a_solve_posts_the_target_url_and_its_time_budget` pins), ship that, *then* add the variant.

---

### W7 — Every worker replica solves independently

[`services/worker/src/main.rs:247`](../services/worker/src/main.rs:247) wires
`InMemorySessionStore`, so N replicas mean N solves per provider per TTL, and a rolling restart
discards every session. The trait exists precisely so this can be swapped
([`solving.rs:44`](../crates/fetch/src/solving.rs:44)).

Implement `RedisSessionStore` in `crates/fetch` behind a feature, modelled on
[`crates/service/src/ratelimit/redis.rs`](../crates/service/src/ratelimit/redis.rs) (`fred`, already
a workspace dependency). Key `solved_session:{provider}:{host}` — the same key W4 introduces —
value JSON, Redis TTL set from `ttl_secs` so expiry is enforced in one place. The DSN is a
`SecretString` (rule 9). New config key → regenerate `docs/CONFIGURATION.md`.

Worth doing only when the deployment actually runs more than one worker; until then it adds a
failure mode for no gain.

---

## 4. Executing and verifying

### 4.1 Per-change loop

```bash
cargo check -p tankovault-solver
```

Then the narrowest test that covers what you touched — `cargo test -p tankovault-fetch solving`,
`cargo test -p tankovault-solver detection`. Do not run `xtask ci` per edit.

### 4.2 Before asking for review

```bash
cargo run -p xtask -- ci
```

None of these workstreams touch a published API route, a `query!` or a migration, so the
Docker-gated access-matrix and query-plan suites are not implicated. Two regeneration duties do
apply and are mandatory: a new config key means `docs/CONFIGURATION.md`, and any `Cargo.lock`
movement means `cargo run -p xtask -- notices`.

### 4.3 Validating against the live provider

The unit tests cannot prove a marker set matches reality. After a change to detection or session
handling:

1. Capture the current bodies with the `curl` probes in §1 and confirm the classifier's verdict on
   the real bytes (`detect_challenge_body` on the saved file).
2. Run a real scan of `weebcentral` through a worker and read `challenge_detections_total` and
   `challenge_solves_total` — a correct fix shows **no** detections on series pages, not merely
   successful solves.
3. Keep the captured bodies out of the repo; embed the minimal decisive fragment in a test instead,
   as `the_js_detections_beacon_is_not_a_challenge` does.

### 4.4 Deployment ordering

W4 and W6 both change what crosses `/v1/solve`. In both cases the **consumer tolerates first,
producer changes second**: ship the tolerant decoder, deploy, then ship the shape change. Never in
one release.

---

## 5. Acceptance criteria

- [ ] No content page from any Cloudflare-fronted provider classifies as a challenge; the JS
      Detections beacon has a test carrying the real markup. *(W1, done)*
- [ ] A failed solve leaves no session behind, and a session that is still challenged is evicted.
- [ ] `challenge_detections_total`, `challenge_solves_total` and `challenge_session_replays_total`
      are emitted, documented in `OBSERVABILITY.md`, and a dark provider is visible without reading
      logs.
- [ ] A clearance cookie is never sent to a host outside the domain it was issued for, with a test
      that pins it.
- [ ] A session won on a proxy tier is not cached for direct replay.
- [ ] A blocked provider stops paying full solve cost on every queued task, and the breaker is
      counted.
- [ ] `cargo run -p xtask -- ci` passes on a host with a working MSVC toolchain, and the report says
      which host it ran on.
