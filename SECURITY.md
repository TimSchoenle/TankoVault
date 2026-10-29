# Security policy

## Reporting a vulnerability

**Do not open a public issue.** Use GitHub's private vulnerability reporting on this repository
(*Security* → *Report a vulnerability*), which creates a private advisory only maintainers can
read.

If that is unavailable to you, open an issue containing **only** a request for a private channel
and no detail about the finding.

What helps, in rough order of usefulness: the affected component (`api`, `worker`, `sync`,
`render`, `challenge-solver`, `control-plane`, `notifier`, `frontend`, or a `crates/` library),
the commit or tag, what an attacker gains, and the smallest reproduction you have. A proof of
concept is welcome and not required: a clear description of the flaw is usually enough.

There is no bounty. Reports are acknowledged and credited unless you ask otherwise.

## Scope

This project is deployed by its operators; there is no hosted instance to test against. Please
test against your own deployment, and treat these as in scope:

- **Authentication and session handling**: access/refresh token issuance, the refresh-cookie
  shape, session revocation, password reset and email verification.
- **Authorization**: the permission model, the `/v1/admin` surface, and anything that lets one
  account read or write another's data.
- **The internal tier**: `sync`, `control-plane`, `render` and `challenge-solver` accept
  privileged requests behind a shared `X-Internal-Token`. Anything that reaches them without it
  is in scope, including through the API's proxies.
- **SSRF**: `render`, `challenge-solver` and the provider `base_url` all take URLs chosen by
  someone else. The guard is `tankovault_domain::ssrf`; a bypass of it is in scope.
- **Data at rest**: external-tracker OAuth tokens are encrypted with
  `TANKOVAULT_ANILIST__TOKEN_ENCRYPTION_KEY`; anything that discloses them is in scope.
- **The crawl stack**: a malicious provider response that escapes parsing into memory
  corruption, resource exhaustion, or a stored value that changes behaviour elsewhere.

Out of scope: the reference `deploy/docker-compose.yml` uses documented dev-only placeholder
credentials and is not a deployment; findings that amount to "the example stack has example
secrets" will be closed. So will reports about missing headers on `/health`, `/ready` or the
metrics port, which are not request-facing surfaces.

## Known issue an operator must act on

This is tracked, not secret. It is here because a deployment is affected by it today and no
code change closes it:

- **Renderer DNS rebinding.** `render` and `challenge-solver` resolve every target hostname and
  refuse it unless *every* answer is public (`ssrf::validate_unpinned`), and `render` applies the
  same check to each request Chromium makes (redirects and subresources included) through CDP
  `Fetch` interception on the browser target. What remains:

  - Chromium and TRAWL resolve independently of that check, so a name that answers differently
    on a second lookup can still reach an internal address from either tier.
  - Inside TRAWL, redirects and subresources are not checked at all: it is a third-party image
    this repository does not control.
  - Traffic that CDP `Fetch` never pauses for (WebSocket, WebRTC, `<link rel=preconnect>`) is
    not checked in `render` either.

  The reference compose file narrows what any of these can reach: `render`, `challenge-solver`
  and `trawl` sit on their own `solver` network with only `worker` beside them, and `trawl`
  reaches Redis over a separate network that neither service joins. Postgres, NATS, `api`,
  `sync` and `control-plane` are not addressable from that tier.

  Chromium's `--host-resolver-rules` does not close it: that is a *launch* flag, and
  `BrowserManager` launches one browser lazily and reuses it for the life of the process.
  Pinning resolution to the address `ssrf::validate_and_resolve` already produced would mean a
  browser per request: a Chrome start is seconds against a render's tens of milliseconds, which
  is the entire cost the service exists to amortise. CDP has no per-page substitute: it can
  block URLs and rewrite headers, but nothing in it overrides DNS for one target. So the
  options are a browser per request, a resolver the container controls, or an egress-restricted
  network namespace. **The last is the cheapest, and it is a deployment change, not a
  patch**: restrict the `render`, `challenge-solver` and `trawl` containers' egress to the
  public internet. Outside the reference compose file that is still the operator's job.

## What the deployment expects of you

`TANKOVAULT_PROFILE=production` refuses to boot with `internal.identity=off` or a weak
`auth.jwt_secret`, an unrecognised profile refuses to boot at all, the internal tier identifies
callers by token unless told otherwise, and every profile refuses the placeholder secrets
published in this repository. That is deliberate: the
failure modes those settings produce are silent, and a service that will not start is a better
outcome than one that starts insecure. See `docs/CONFIGURATION.md`.
