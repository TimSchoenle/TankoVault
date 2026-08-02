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
        └─ build (18 legs: 9 images x amd64/arm64, native runners)
              └─ manifest (9 jobs: manifest list per image, cosign sign, SBOM attest)
```

## What is published

Nine images, each a `linux/amd64` + `linux/arm64` manifest list, to **both** registries:

| Docker Hub | GHCR |
| --- | --- |
| `timschoenle/tankovault-<bin>` | `ghcr.io/<owner>/<repo>/<bin>` |

for `<bin>` in `api`, `worker`, `control-plane`, `notifier`, `sync`, `challenge-solver`,
`render`, `xtask`, `frontend`.

Tags per release: `vX.Y.Z`, `X.Y` and `latest`. Every published manifest-list digest is signed
with cosign keyless and carries an SPDX SBOM attestation.

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
| `RELEASE_BOT_APP_ID`, `RELEASE_BOT_PRIVATE_KEY` | `release` environment secret | release-please, update-lockfile |
| `ACTIONS_MAINTENANCE_APP_ID`, `ACTIONS_MAINTENANCE_PRIVATE_KEY` | repository secret | auto-merge |
| `DOCKERHUB_USERNAME`, `DOCKERHUB_TOKEN` | `release` environment secret | image publish |

Every job that reads one of these names an `environment: release` with `deployment: false` —
`release-please`, `update-lockfile`, and both publish jobs, `build` and `manifest`. That is not
optional and it fails quietly: a job that omits the environment reads the secret as an empty
string, so `docker/login-action` reports "Username and password required" and
`create-github-app-token` reports an empty `app-id`, neither of which names the environment as
the cause. `deployment: false` keeps a secret scope from writing a deployment record per job.

GHCR needs no secret at all: both the `build` and `manifest` jobs log in with the run's own
`GITHUB_TOKEN`, which their `packages: write` permission covers. Only Docker Hub, which is
outside GitHub's trust boundary, requires stored credentials.

The App token is not a stylistic preference: a push made with `GITHUB_TOKEN` does not trigger
workflows, so a release PR created or corrected by it would never run CI, and the required `ci`
check could never pass.

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
