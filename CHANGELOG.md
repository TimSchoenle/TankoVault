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

- **Passkeys (`WebAuthn`), end to end.** A passkey is a first-class credential alongside the
  password: register one from Account → Security, then sign in with no identifier and no
  password at all (discoverable credentials, `UserVerificationPolicy::Required`, so it is two
  factors in one gesture). Keys can be named, renamed and revoked, and show when they were last
  used. Behind `accounts.passkeys`, which gates the sign-in ceremony and the management surface
  together — leaving one half reachable would mean a live credential its owner cannot revoke.
  Needs `TANKOVAULT_AUTH__WEBAUTHN_ORIGIN` (falls back to `TANKOVAULT_EMAIL__BASE_URL`); an
  unconfigured deployment answers `503` rather than `404`, so a missing setting cannot be
  mistaken for a feature that is not in this build.
  - Built on `webauthn-rs` 0.6 rather than the stable 0.5 line, because 0.5 links `openssl-sys`
    and the `scratch` runtime images ship no OpenSSL — a link failure that would surface at
    exec time in production, not at build time in CI.
  - Ceremony state lives in Postgres and is consumed by a `DELETE ... RETURNING`, so a challenge
    cannot be replayed and a `finish` cannot land on a replica that never saw the `start`.
  - Adding a key requires the current password. An access token lasts fifteen minutes; a passkey
    is permanent, and without the check anyone holding a token briefly could install a credential
    that survives every later password change and session revocation.
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
- Marking a part release read left the whole-chapter frontier behind, so reading `46.1` reported
  everything up to `45` as unread and kept pushing the stale number to AniList — which has no
  concept of parts and can only be told a whole chapter. Marking a part now also advances the
  whole frontier to the last chapter before the one the part belongs to.
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
