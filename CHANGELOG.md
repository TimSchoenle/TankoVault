# Changelog

Notable changes, newest first. Format loosely follows [Keep a Changelog]; this project has not
cut a release yet, so everything is under *Unreleased* and every crate is at `0.1.0`.

The release workflow builds, structure-tests, signs and attests on every tag but **does not
push** — see `OP-6` in `docs/audit/PROGRESS.md`: `wreq-util` is GPL-3.0, and distributing the
images is *conveying*. The first entry below this line will be whatever tag resolves that.

## [Unreleased]

### Security

- Refresh cookies use the `__Host-` prefix and are `Secure` by default; the local-HTTP opt-out
  keeps the old name and path, so flipping it signs everyone out once.
- `GET /v1/me/stream` takes a single-use 30-second ticket instead of an access token in the
  query string, and re-checks account suspension on every reconnect. Live notifications had in
  fact **never worked** — the client sent `?token=` while the handler read `?access_token=`.
- The internal tier (`sync`, `control-plane`, `render`, `challenge-solver`) requires
  `X-Internal-Token`, and only `frontend` publishes a host port.
- Account enumeration closed on login, password reset and confirmation resend; email changes
  require the current password and revoke every session.
- **Every lookup by email or username was case-sensitive** despite the columns being `citext` —
  a total, silent lockout for anyone whose casing differed. Fixed at all four comparison sites.

### Added

- `xtask ci` runs every offline gate CI runs, in CI's order.
- `xtask coverage-ratchet` fails the build when line coverage drops below
  `.github/coverage-floor.txt`.
- `deploy/observability/`: 31 recording rules, 25 alerts and a provisioned dashboard, behind a
  compose overlay so a plain `up` is unchanged.
- `docs/CONFIGURATION.md` — the env-var reference, ~70 keys.
- Reversible migrations, container healthchecks, resource limits, and a read-only root
  filesystem on every tier including `render`.

### Fixed

- Reading progress has two frontiers (whole and part). Five implementations of "has this user
  read this chapter?" disagreed, so part releases counted as unread in three places, a dashboard
  card could not be cleared, and the notifier announced chapters the reader had finished.
- `parse_number` returned `f64::INFINITY` for an overlong digit run, which stores, freezes
  `latest_chapter` forever, and serialises to `null` on the bus.
- The notifier acked after a *failed* fan-out — at-most-once delivery, losing notifications with
  one `warn!`.
- `http_requests_in_flight` leaked on every client disconnect, so the one gauge an operator
  reaches for to answer "is this saturated?" only ever rose.
- The admin console's pending-conflict count was per user rather than per linked account.

### Changed

- The `api` binary no longer links `wreq`/BoringSSL: the adapter dry-run moved to the worker,
  which already carries the crawl stack. 557 → 487 crates, and one TLS stack instead of two.
- Postgres 17 everywhere; the reference stack was on a beta major.
- Every suppression is `#[expect(..., reason = "...")]`; seven turned out to suppress nothing.

Every entry above traces to a row in `docs/audit/PROGRESS.md`, which carries the full reasoning
and the test that pins it.

[Keep a Changelog]: https://keepachangelog.com/en/1.1.0/
