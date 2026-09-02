<!--
Generated from .github/templates/README.md.hbs — edit that file, not this one. `auto-fix.yaml`
renders it on every pull request and commits the result back to the branch; a push to `main`,
and a pull request from a fork, are covered by `ci.yml`'s `readme` job, which renders with
`check: true` and fails on a stale file rather than writing one.

Variables come from .github/scripts/readme-variables.sh, which reads them out of the files that
own them:

    version      [workspace.package] version, from Cargo.toml
    license      [workspace.package] license, from Cargo.toml
    description  [workspace.package] description, from Cargo.toml
    edition      [workspace.package] edition, from Cargo.toml
    msrv         [workspace.package] rust-version, from Cargo.toml
    toolchain    [toolchain] channel, from rust-toolchain.toml
    postgres     the major of the postgres service image, from deploy/docker-compose.yml
    redis        the major of the redis service image, from deploy/docker-compose.yml

Run that script to see what CI will render with. Everything else here is prose and belongs in
this file: a number with a home elsewhere is injected, a sentence is written.
-->
# TankoVault

Multi-service Rust aggregator and tracker for manga, manhwa and manhua. Stores links and metadata, not chapter images.

[![Release](https://img.shields.io/github/v/release/TimSchoenle/TankoVault?sort=semver)](https://github.com/TimSchoenle/TankoVault/releases)
[![CI](https://img.shields.io/github/actions/workflow/status/TimSchoenle/TankoVault/ci.yml?branch=main)](https://github.com/TimSchoenle/TankoVault/actions/workflows/ci.yml)
[![License](https://img.shields.io/static/v1?label=license&message=PolyForm-Noncommercial-1.0.0&color=blue)](LICENSE)
[![Rust](https://img.shields.io/static/v1?label=rust&message=1.94%2B&color=orange)](https://www.rust-lang.org)

## What this is

A hobby project, built for my own use. There is no publicly hosted instance, and no support
promise attaches to any of the images below.

TankoVault indexes series metadata across independent provider sites and reconciles it into one
**canonical series** with many **provider sources**. Watchlists, read progress, per-title
notifications and AniList back-sync sit on that model. All of it is one Cargo workspace: sixteen
crates under `crates/` and nine published binaries, of which eight are long-running services and
one is the one-shot installer a deployment runs before a rollout.

**It stores links and metadata, and nothing else.** No chapter image is downloaded, cached or
served. An operator remains responsible for whether crawling a given source is lawful in their
jurisdiction and under that site's terms.

## Quick start

```bash
docker compose -f deploy/docker-compose.yml up --build
```

That applies the migrations, seeds a demo administrator (`admin` / `changeme12345`) and a
placeholder provider, then starts every service. Open <http://localhost:3000>.

Port 3000 is the only one published on the host. The `frontend` service serves the SPA and
reverse-proxies `/v1/*` to the `api` service, so a browser needs that one origin, and the
privileged internal contracts stay on the compose network. Docker writes its own firewall rules
when it publishes a port, so exposing one of those would put it on every interface the host has.

## Table of contents

- [Features](#features)
- [Installation](#installation)
- [Usage](#usage)
- [Configuration](#configuration)
- [Operations](#operations)
- [Compatibility](#compatibility)
- [Documentation](#documentation)
- [Contributing](#contributing)
- [Security](#security)
- [License](#license)

## Features

- One work is one series. Title, alternative titles, description, tags, cover URL, status and
  author are merged across every provider carrying it, behind a search vector. The candidate
  scoring and the merge bands are pure functions in `crates/matcher`.
- Two scan cadences. A full scan rebuilds a provider's archive; a fast scan reads only its
  latest feed, and that is the one that runs on a schedule.
- **Adding a provider is usually configuration rather than code.** A config-driven adapter covers
  the common CMS shape, and anything else drops in behind the same `SourceAdapter` trait.
- Chapter links are stored as paths relative to the provider's base URL and resolved at read
  time, so a site changing domain is a one-field edit rather than a migration.
- Providers behind Cloudflare or DDoS-Guard fingerprint the TLS ClientHello and the HTTP/2
  SETTINGS frame, so the crawl path uses `wreq` with browser emulation while internal
  service-to-service HTTP stays on `reqwest`. A cheap classifier spots an interstitial in
  milliseconds and hands the URL to a pluggable solver tier.
- Recommendations are a scoring model over plain data in `crates/recsys`, with no I/O of its own.
- Passkeys, TOTP and passwords, over argon2id hashing, a JWT access token and a rotating refresh
  token. AniList tokens are encrypted at rest.
- An operator console for live scan progress, provider health and adapter configuration.

## Installation

`deploy/docker-compose.yml` on a single host is the shape this repository builds and tests.

### Container images

Nine images per release, each a `linux/amd64` + `linux/arm64` manifest list, on two registries:

```bash
docker pull ghcr.io/timschoenle/tankovault/api:v8.9.0
docker pull docker.io/timschoenle/tankovault-api:v8.9.0
```

Substitute `api` for `bootstrap`, `challenge-solver`, `control-plane`, `frontend`, `notifier`,
`render`, `sync` or `worker`. Each digest carries a cosign keyless signature, an attested SBOM
and its own configuration contract as an OCI artifact. Pin by digest in production.

All of them come out of one `deploy/docker/Dockerfile`. Eight run on `scratch` as `1001:1001`,
with no shell and no package manager; `render` drives a real Chromium and runs on `debian-slim`
instead.

### Helm

```bash
helm repo add timschoenle https://timschoenle.github.io/helm-charts
helm install tankovault timschoenle/tankovault
```

The chart is in [TimSchoenle/helm-charts](https://github.com/TimSchoenle/helm-charts/tree/main/charts/tankovault)
and versions on its own cadence; a release here opens a pull request there that moves its
`appVersion`. It reads each image's published configuration contract, which is how the chart's
CI catches a key this workspace renamed.

### Desktop application

Every release attaches a portable Windows and a portable Linux archive of the frontend built for
the desktop platform. It talks to an `api` instance you already run.

### From source

```bash
git clone https://github.com/TimSchoenle/TankoVault.git
cd TankoVault
cargo build
```

`rust-toolchain.toml` names the channel, so `rustup` installs it on the first `cargo` invocation.
Node.js is needed for the frontend's Tailwind step, and [`just`](https://github.com/casey/just)
for the recipes that regenerate the configuration contracts.

## Usage

The host workspace excludes `web/frontend`, which targets `wasm32-unknown-unknown`, and `fuzz`,
which needs nightly. Each carries its own toolchain file and lockfile.

```bash
just verify                    # fmt, clippy under -D warnings, and the test suite
cargo test --workspace --doc   # `--all-targets` excludes doc tests, and these are contracts
```

Dev and ops tasks are `xtask` subcommands. `migrate`, `reset`, `seed` and `sqlx-prepare` read
`DATABASE_URL`; the others touch no database.

```bash
cargo run -p xtask -- ci               # every offline gate CI runs, in CI's order
cargo run -p xtask -- migrate          # apply pending migrations
cargo run -p xtask -- seed             # demo administrator and the built-in provider presets
cargo run -p xtask -- openapi          # regenerate openapi.json and the typed client
cargo run -p xtask -- sqlx-prepare     # refresh the committed sqlx offline query cache
cargo run -p xtask -- notices          # regenerate THIRD-PARTY-NOTICES from both lockfiles
cargo run -p xtask -- repo-lint        # the invariants no compiler sees
cargo run -p xtask -- config-contract  # check the nine contracts and the Dockerfile LABEL regions
```

`config-contract` checks and `just regenerate` writes. Splitting them is deliberate: a gate that
carried its own copy of the rendering it judges would be a second opinion rather than a check.

```bash
just regenerate                # rewrite docs/contracts/*.json and the Dockerfile LABEL regions
just render api contract       # print one rendering of one service, writing nothing
```

The SPA is built with the `dx` CLI from its own directory:

```bash
cd web/frontend
npm install          # first time only, for the Tailwind tooling
npm run css:watch    # terminal 1
dx serve             # terminal 2
```

Property tests live beside the code they cover and run in the ordinary `cargo test`.
Coverage-guided fuzzing needs nightly and lives in [`fuzz/`](fuzz/README.md), which no gate runs.

## Configuration

Every service loads the same five layers through `terrace-config`, each overriding the one above
it:

1. **Defaults** — the `Default` impl of each typed block.
2. **TOML** at `$TANKOVAULT_CONFIG` — a file, or every `*.toml` inside it when it names a
   directory.
3. **Environment** — `TANKOVAULT_`-prefixed variables, `__` for nesting.
4. **Secrets directory** at `$TANKOVAULT_SECRETS_DIR` — one file per key, named after it.
5. **File indirection** — `TANKOVAULT_<KEY>_FILE=/path` names a file holding the value.

Prefer the two file layers for anything secret. They keep the value out of `/proc/<pid>/environ`
and out of every child process, and a rotated file is picked up by rebuilding the configuration
rather than by restarting the process.

At boot each service reports which layer supplied each key, in precedence order, and warns when
two layers supply the same one. That warning is the shape of "the rotated secret is not being
read": the mount is there and a stale `TANKOVAULT_*` variable is sitting on top of it. Keys and
layers only. No value is ever logged.

These have no working default, and the stack refuses to boot rather than starting insecure:

- `TANKOVAULT_AUTH__JWT_SECRET` signs the API access tokens.
- `TANKOVAULT_AUTH__PASSWORD_PEPPER` is mixed into every argon2id hash. It is optional, but once
  set it has to stay stable and has to match across the `api` service and the seeding step.
- `TANKOVAULT_ANILIST__CLIENT_ID`, `__CLIENT_SECRET` and `__REDIRECT_URI` are the AniList OAuth
  application.
- `TANKOVAULT_ANILIST__TOKEN_ENCRYPTION_KEY` is a base64 32-byte key for those tokens at rest.

[docs/CONFIGURATION.md](docs/CONFIGURATION.md) is the full reference: every key, its default,
which services read it, and the failure modes worth knowing. Several of those are silent.

The nine documents under [docs/contracts/](docs/contracts/) are the machine-readable half of the
same surface. Each is generated from the config root one image's binary deserialises, and is
published on that image's digest.

## Operations

### Probes

| Endpoint | Meaning |
| --- | --- |
| `GET /health` | Liveness. Consults no dependency, so a failing Postgres cannot start a restart loop. |
| `GET /ready` | Readiness, with per-dependency detail. Each check gets two seconds before it counts as failed. |
| `GET /metrics` | The Prometheus scrape, on a listener of its own where one is configured. |

`/health` and `/ready` are the whole credential-free surface. The scrape joins them only while it
has no listener of its own, so giving it one keeps it off the port an orchestrator probes.

### The internal tier

Services authenticate each other with per-caller tokens or with mutual TLS, selected by
`TANKOVAULT_INTERNAL__IDENTITY`. Each callee lists the callers it accepts, so one service's
credential opens the routes that service is meant to reach and no others. Under mutual TLS a
kubelet probe presents no client certificate, so `/health` and `/ready` move to the plaintext
probe listener and every probe has to be pointed at it.

### Storage and messaging

PostgreSQL is the system of record, and `pgvector` has been a hard dependency of it since
migration 0027. NATS JetStream carries tasks and domain events. Redis backs caching, rate-limit
state, and the control-plane's leader election, which is what makes a second replica of it safe.

Migration is a discrete one-shot step in the `bootstrap` image rather than something a service
does at startup. Resetting a schema exists only in `xtask`, from a checkout, behind an explicit
confirmation variable, so nothing published carries a destructive command.

## Compatibility

| | Supported |
| --- | --- |
| Rust | 1.94 minimum, edition 2024; built with 1.94.0 |
| PostgreSQL | 18, with `pgvector` |
| Redis | 8 |
| NATS | JetStream |
| Platforms | `linux/amd64`, `linux/arm64` |

The minimum is checked by its own CI job rather than asserted. It and the pinned channel are
separate claims that happen to agree today.

## Documentation

| Document | Purpose |
| --- | --- |
| [docs/design.md](docs/design.md) | The architecture and build specification other documents cite by section |
| [docs/ENGINEERING_GUIDE.md](docs/ENGINEERING_GUIDE.md) | The rules the code is written to, and what enforces each one. Read before changing anything |
| [docs/OPERATIONS.md](docs/OPERATIONS.md) | Running the fleet: scaling, failure modes, runbooks |
| [docs/CONFIGURATION.md](docs/CONFIGURATION.md) | Every `TANKOVAULT_*` key, its default, and which services read it |
| [docs/OBSERVABILITY.md](docs/OBSERVABILITY.md) | The metric catalogue and what is worth alerting on |
| [docs/PROVIDERS.md](docs/PROVIDERS.md) | Provider adapters: the trait, the config-driven case, and onboarding one |
| [docs/CHALLENGE_HANDLING.md](docs/CHALLENGE_HANDLING.md) | Interstitial detection and the solver tier behind it |
| [docs/CHAPTER_STORAGE.md](docs/CHAPTER_STORAGE.md) | How chapter links are stored and resolved |
| [docs/READING_PROGRESS_AND_SYNC.md](docs/READING_PROGRESS_AND_SYNC.md) | Read progress and the AniList push and pull |
| [docs/RECOMMENDATIONS.md](docs/RECOMMENDATIONS.md) | The recommendation model, feature by feature |
| [docs/DESIGN_SPEC.md](docs/DESIGN_SPEC.md) | The SPA's design system; frontend source files cite it by section |
| [docs/RELEASING.md](docs/RELEASING.md) | How a release is cut, signed and published |
| [docs/contracts/](docs/contracts/) | The nine published configuration contracts, one per image |
| [deploy/README.md](deploy/README.md) | The Dockerfile's stages, the compose stack, and the one-shot install steps |
| [web/frontend/README.md](web/frontend/README.md) | The SPA: design system, generated API client, i18n rules |
| [openapi.json](openapi.json) | The REST API specification, generated from the handlers |

## Contributing

Issues and pull requests are welcome. [CONTRIBUTING.md](CONTRIBUTING.md) covers the commit
convention, the gates and the inbound licence terms; read it before opening one. Several files
here are generated and say so in their first lines, and CI reverts an edit made to the output
instead of to its source.

## Security

Do not open a public issue for a vulnerability. [SECURITY.md](SECURITY.md) has the reporting
instructions and the supported versions.

## License

[PolyForm-Noncommercial-1.0.0](LICENSE). Source available, not open source. Use, modify and redistribute it for
any noncommercial purpose; commercial use needs a separate licence from the copyright holder.
Charities, schools, public research bodies and government institutions count as noncommercial
however they are funded.

Where the line falls, in plain terms. Running your own instance for yourself, your household or
your friends is fine, including when donations cover the hosting bill, and so is modifying it and
publishing your fork under the same terms. Charging for access, running ads against it, or
offering it to customers as a hosted service needs a licence from me. Open an issue and ask if
you are unsure.

The published images carry the same terms and ship them at `/LICENSE`. A registry page does not
say so, but pulling one to run a paid service is unlicensed.

The dependencies are licensed separately, overwhelmingly MIT and Apache-2.0, and most of them
require their licence text to accompany a binary distribution, which an image and a WASM bundle
both are. [THIRD-PARTY-NOTICES](THIRD-PARTY-NOTICES) is that text for both dependency graphs,
generated from the lockfiles by `cargo run -p xtask -- notices`. Every image ships it at
`/THIRD-PARTY-NOTICES`, and a running instance serves it at `/third-party-notices`. The app's
`/licenses` screen renders the same notices grouped by licence — each distinct text once, naming
the dependencies that ship it — so the person whose browser ran the code can read the terms it
came under without scrolling half a megabyte of plain text.
