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
concept is welcome and not required — a clear description of the flaw is usually enough.

There is no bounty. Reports are acknowledged and credited unless you ask otherwise.

## Scope

This project is deployed by its operators; there is no hosted instance to test against. Please
test against your own deployment, and treat these as in scope:

- **Authentication and session handling** — access/refresh token issuance, the refresh-cookie
  shape, session revocation, password reset and email verification.
- **Authorization** — the permission model, the `/v1/admin` surface, and anything that lets one
  account read or write another's data.
- **The internal tier** — `sync`, `control-plane`, `render` and `challenge-solver` accept
  privileged requests behind a shared `X-Internal-Token`. Anything that reaches them without it
  is in scope, including through the API's proxies.
- **SSRF** — `render`, `challenge-solver` and the provider `base_url` all take URLs chosen by
  someone else. The guard is `tankovault_domain::ssrf`; a bypass of it is in scope.
- **Data at rest** — external-tracker OAuth tokens are encrypted with
  `TANKOVAULT_ANILIST__TOKEN_ENCRYPTION_KEY`; anything that discloses them is in scope.
- **The crawl stack** — a malicious provider response that escapes parsing into memory
  corruption, resource exhaustion, or a stored value that changes behaviour elsewhere.

Out of scope: the reference `deploy/docker-compose.yml` uses documented dev-only placeholder
credentials and is not a deployment; findings that amount to "the example stack has example
secrets" will be closed. So will reports about missing headers on `/health`, `/ready` or the
metrics port, which are not request-facing surfaces.

## Known issue an operator must act on

This is tracked, not secret. It is here because a deployment is affected by it today and no
code change closes it:

- **`SEC-2b` — renderer DNS rebinding.** The SSRF guard validates and re-resolves in-process, but
  Chromium and TRAWL resolve independently, so a name that answers differently on a second
  lookup can still reach an internal address from the `render` tier. Closing it needs
  container-level egress restriction, not a patch.

It is, with its full reasoning, in `docs/audit/PROGRESS.md`.

## What the deployment expects of you

`TANKOVAULT_PROFILE=production` refuses to boot without `TANKOVAULT_INTERNAL__TOKEN`, and every
profile refuses the placeholder secrets published in this repository. That is deliberate: the
failure modes those settings produce are silent, and a service that will not start is a better
outcome than one that starts insecure. See `docs/CONFIGURATION.md`.
