# Releasing

TankoVault releases are automated end to end by [release-please]. Nothing here is run by hand:
you write conventional commits, release-please maintains a release pull request, and merging
that pull request tags the repository and publishes nine multi-architecture images.

This document covers the parts that are not obvious from reading the workflow, and the two
decisions a human still has to make.

## The flow

```
commit to main (conventional commits)
  └─ release-please.yaml → opens/updates the release PR
        ├─ bumps [workspace.package] version in Cargo.toml
        └─ writes CHANGELOG.md
     update-lockfile.yaml → syncs Cargo.lock on that PR, commits it
     ci.yml               → the required `ci` check runs against the bumped tree
     auto-merge-release-please.yml → approves + merges after the delay
  └─ merge → release-please tags vX.Y.Z and creates the GitHub release
        └─ release-deps (2 legs: one `builder` compile per architecture)
              └─ build (18 legs: 9 images x amd64/arm64, native runners)
                    └─ manifest (9 jobs: manifest list per image, cosign sign, SBOM attest)
                          └─ helm-release → chart bump PR against TimSchoenle/helm-charts
```

## What is published

Nine images, each a `linux/amd64` + `linux/arm64` manifest list, to **both** registries:

| Docker Hub | GHCR |
| --- | --- |
| `timschoenle/tankovault-<bin>` | `ghcr.io/<owner>/<repo>/<bin>`, case-folded |

for `<bin>` in `api`, `bootstrap`, `worker`, `control-plane`, `notifier`, `sync`,
`challenge-solver`, `render`, `frontend`. `xtask` is **not** among them — the deploy blacklist
(`[workspace.metadata.deploy.exclude]`, root `Cargo.toml`) keeps it out of every registry, and
`xtask repo-lint` fails if it appears in either matrix.

Tags per release: `vX.Y.Z`, `X.Y` and `latest`. Every published manifest-list digest is signed
with cosign keyless and carries an SPDX SBOM attestation.

## The chart hand-off

Publishing an image deploys nothing. The deployable chart lives in
[`TimSchoenle/helm-charts`](https://github.com/TimSchoenle/helm-charts/tree/main/charts/tankovault),
and `helm-release` is what connects the two: it pins all nine services to the digests this run
published and opens a pull request there. A pull request and not a push — the chart repository
gates its own releases, so this workflow proposes and that one decides.

It runs after `manifest`, not after `build`, because what a chart pins has to be a digest that is
signed and SBOM-attested, and `manifest` is where both happen. Each `manifest` leg exports its
Docker Hub manifest-list digest as an artifact; `helm-release` collects the nine. Artifacts rather
than job outputs because `manifest` is a matrix and a matrix job's `outputs` are last-writer-wins.

The Docker Hub digest is the one used because the chart's `repository` values name Docker Hub.
Both registries hold the same list digest — a manifest list is content-addressed, and both were
assembled from the same two per-architecture digests — but a chart should pin the registry it
actually pulls from.

What the pull request changes:

| Chart field | Value |
| --- | --- |
| `services.<camelCase>.image.tag`, `bootstrap.image.tag` | `vX.Y.Z@sha256:…` |
| `appVersion` | `X.Y.Z` — **no** `v`; that is why `helm-release` reads release-please's `version` output and not `tag_name` |
| `version` | the chart's own version, bumped by a patch |

The chart's version is bumped by a patch regardless of how the application version moved: an
application release does not change the chart's templates. A chart change is the chart
repository's own release.

The values keys are *derived* from the binary names (`control-plane` → `services.controlPlane`;
`bootstrap` is a one-shot job and sits at the top level) rather than read from a table in the
workflow. `xtask repo-lint` holds the two image matrices to the deploy blacklist, but it only
recognises `bin: [...]` and `images=[...]` literals — a third hand-maintained list of the nine
services would sit outside everything that keeps them in step. A key that does not already exist
in the chart's `values.yaml` is an error inside the action, so a rename fails loudly there rather
than silently skipping a service.

## Build caching

`cook` and `builder` are the entire wall clock of an image build, and `builder` takes no
`ARG BIN` — so one compile per architecture serves all nine images. Three arch-qualified BuildKit
cache scopes carry it (a cache does not cross architectures; the `chef` base resolves to a
different digest per platform, so every key downstream of it differs):

| Scope | Written by | Read by |
| --- | --- | --- |
| `backend-deps-<arch>` | `ci.yml` `docker-deps`, off `main` only | every image build in both workflows |
| `frontend-deps-<arch>` | `ci.yml` `docker-wasm-deps`, off `main` only | the `frontend` legs |
| `release-deps-<arch>` | `release-please.yaml` `release-deps` | the 18 release legs |

`release-deps` exists because both workflows fire on the push that merges the release PR, so the
CI scopes are still at the previous commit when the release legs start — and the release commit
changes `Cargo.toml`, `Cargo.lock` and `CHANGELOG.md`, which misses `builder`'s `COPY . .`.
Without the warm-up all eighteen legs recompiled the workspace independently.

Two rules keep the 10 GB per-repository LRU budget from evicting the one layer that matters:
pull requests import but never export (a PR-scoped cache is unreadable from anywhere else and
still consumes the budget), and no image leg ever sets `cache-to` — `mode=max` re-exports
imported layers, so each leg would upload its own copy of the multi-gigabyte `cook` layer.

## Licensing of what is published

TankoVault is **PolyForm Noncommercial 1.0.0** (root `LICENSE`, `license` under
`[workspace.package]`). Every runtime stage in `deploy/docker/Dockerfile` copies the terms to
`/LICENSE`, and each contract under `deploy/docker/cst/` asserts they are there — a published
image is a copy of the software, and the Notices clause requires the terms to travel with it.
Pulling an image does not grant commercial use.

This is what the two former gates were waiting on:

- **`ALLOW_IMAGE_PUBLISH`** blocked every push, sign and attest step while the repository was
  unlicensed (`OPS-10.4`). The variable and the `gate` job that read it are gone; merging a
  release pull request publishes. The structure tests that used to run only on the
  non-publishing branch now run on the publishing path, before the push.
- **`ENABLE_ARM64_CI`** kept `ci.yml`'s `docker-arm64` job off, which meant a release was the
  first time the arm64 path ran — how that build stayed broken for months. That job is
  unconditional now. Both variables can be deleted from the repository settings.

`OP-6` (GPL-3.0 `wreq-util`) was resolved separately, on 2026-08-01, by upgrading to
`wreq-util` 3.x under Apache-2.0.

## Required secrets and variables

| Name | Kind | Used by |
| --- | --- | --- |
| `RELEASE_BOT_APP_ID`, `RELEASE_BOT_PRIVATE_KEY` | `release` environment secret | release-please, update-lockfile, helm-release |
| `ACTIONS_MAINTENANCE_APP_ID`, `ACTIONS_MAINTENANCE_PRIVATE_KEY` | repository secret | auto-merge |
| `DOCKERHUB_USERNAME`, `DOCKERHUB_TOKEN` | `release` environment secret | image publish |

Every job that reads one of these names an `environment: release` with `deployment: false` —
`release-please`, `update-lockfile`, `helm-release`, and both publish jobs, `build` and
`manifest`. That is not optional and it fails quietly: a job that omits the environment reads the
secret as an empty
string, so `docker/login-action` reports "Username and password required" and
`create-github-app-token` reports an empty `app-id`, neither of which names the environment as
the cause. `deployment: false` keeps a secret scope from writing a deployment record per job.

GHCR needs no secret at all: both the `build` and `manifest` jobs log in with the run's own
`GITHUB_TOKEN`, which their `packages: write` permission covers. Only Docker Hub, which is
outside GitHub's trust boundary, requires stored credentials.

The App token is not a stylistic preference: a push made with `GITHUB_TOKEN` does not trigger
workflows, so a release PR created or corrected by it would never run CI, and the required `ci`
check could never pass.

The release bot App also has to be **installed on `TimSchoenle/helm-charts`** with
`contents: write` and `pull-requests: write`, or `helm-release` fails at token minting. That is
an installation setting on the App, not a secret in this repository — nothing here can grant it.

## Why `release-type: simple` and not `rust`

This is the configuration most likely to be "fixed" by someone who knows release-please, so:
JSON cannot carry a comment, and this is where the reasoning lives.

release-please's `rust` strategy rewrites `[package] version` in the root manifest **and every
workspace member**. TankoVault's root is a *virtual* manifest with no `[package]` section, and
all 26 members inherit `version.workspace = true` from `[workspace.package]` — which the Rust
updater does not touch. Using `rust` would therefore rewrite 26 members'
`version.workspace = true` into literal version strings while leaving the real source of truth
at `0.1.0`.

`simple` touches only `CHANGELOG.md` — its `version.txt` updater is `createIfMissing: false`
and there is no `version.txt` here, so that half is a no-op — and one `extra-files` entry bumps
`$.workspace.package.version` explicitly. One version, one place, no churn across 26 files.

The cost is that `Cargo.lock` goes stale on the release PR, because every member's recorded
version changes and `simple` does not update lockfiles. `update-lockfile.yaml` closes that, and
it is load-bearing: without it the release PR carries a lockfile that disagrees with the
manifests, every `--locked` build fails (`xtask ci`, `msrv`, `supply-chain`, and the
Dockerfile's `cargo auditable build --release --locked`), and the PR can never go green. That is
a deadlock, not a flaky failure.

## Commit types that appear in the changelog

`feat`, `fix`, `perf`, `revert`, `docs`, `refactor`, `test`, `style`, `ci`, `build`, `chore`,
`deps` — all visible, matching the other repositories in this account. A `feat!` or a
`BREAKING CHANGE:` footer drives the major bump.

## Verifying a published image

```sh
cosign verify \
  --certificate-identity-regexp 'https://github\.com/<owner>/<repo>/\.github/workflows/release-please\.yaml@.*' \
  --certificate-oidc-issuer https://token.actions.githubusercontent.com \
  timschoenle/tankovault-api:vX.Y.Z
```

## What was replaced

The previous `release.yml` — tag-triggered, GHCR-only, amd64-only — is gone. Keeping both would
have double-built every release, since release-please pushes the tag that workflow triggered on.

[release-please]: https://github.com/googleapis/release-please
