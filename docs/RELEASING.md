# Releasing

TankoVault releases are automated end to end by [release-please]. Nothing here is run by hand:
you write conventional commits, release-please maintains a release pull request, and merging
that pull request tags the repository and publishes the multi-architecture images **that
changed**.

This document covers the parts that are not obvious from reading the workflow, and the two
decisions a human still has to make.

## The flow

```
commit to main (conventional commits)
  └─ release-please.yaml
        release-pr        → opens/updates the release PR
              ├─ bumps [workspace.package] version in Cargo.toml
              └─ writes CHANGELOG.md
     auto-fix.yaml        → syncs both Cargo.lock files, regenerates openapi.json and the
                            client at the new version, commits all of it once
     ci.yml               → the required `ci` check runs against the bumped tree
     auto-merge-release-please.yml → approves + merges after the delay
  └─ merge → release-please.yaml
        release-please    → creates the GitHub release for vX.Y.Z as a **draft**. No tag yet.
        ├─ plan (xtask release-plan: which images changed since each one's published tag)
        │     └─ release-deps (2 legs: one `build-cache` compile per architecture)
        │           └─ build (2 legs per planned image: amd64 + arm64, native runners)
        │                 └─ manifest (1 job per planned image: list, cosign sign, SBOM attest)
        │                       └─ helm-release → chart bump PR against TimSchoenle/helm-charts
        └─ desktop (installer matrix)
              └─ desktop-release → attaches them, publishes the draft. **This creates the tag.**
                    └─ release-pr → opens the *next* release PR, against the tag just created
```

A release in which no image changed — a documentation or CI release — stops after `plan`:
nothing is built, nothing is published, and no chart pull request is opened.

## Why release-please runs twice

`release-please.yaml` invokes the action in two jobs, `release-please`
(`skip-github-pull-request`) and `release-pr` (`skip-github-release`), with `desktop-release`
between them. That split is load-bearing, not tidiness.

The action tags a release and opens the pull request for the next one in a single process, and
the two halves disagree about when a tag exists. Because releases here are created as **drafts**
— see [the desktop update channel](#the-desktop-update-channel) for why — GitHub materialises no
tag until `desktop-release` publishes one, minutes later. The pull-request half resolves "the
previous release" by looking that tag up, so in one process it looks for a tag its own
predecessor has not created yet.

It does not fail when it cannot find one. It falls back to the version in
`.release-please-manifest.json` — so the version stays right — and takes the commit range from
the beginning of history. Release v3.1.0 logged

```
❯ looking for tagName: v3.1.0
✔ No latest release found for path: ., component: , but a previous version (3.1.0) was specified in the manifest.
✔ Considering: 259 commits
```

and opened [#138]: a 245-line changelog re-listing every commit ever merged, and a 4.0.0 major
bump from re-counting `!` commits released a dozen tags earlier — for a `main` that had nothing
on it since v3.1.0 at all. Nothing downstream would have caught it; the action succeeded, the
release published normally, and `auto-merge-release-please.yml` merges release pull requests on a
schedule. `repo-lint`'s `release-please-tags-before-it-proposes` is what holds the two halves
apart now.

If `desktop-release` fails, the release stays a draft, `vX.Y.Z` is never tagged and `release-pr`
is skipped rather than run against a base that does not exist — so no release pull request is
opened until the next push to `main`. The run summary says so. A `workflow_dispatch` of
`release-please.yaml` is the manual equivalent: with nothing left to tag it reaches `release-pr`
directly.

[#138]: https://github.com/TimSchoenle/TankoVault/pull/138

## What is published

The images that changed, each a `linux/amd64` + `linux/arm64` manifest list, to **both**
registries:

| Docker Hub | GHCR |
| --- | --- |
| `timschoenle/tankovault-<bin>` | `ghcr.io/<owner>/<repo>/<bin>`, case-folded |

The publishable set is `<bin>` in `api`, `bootstrap`, `worker`, `control-plane`, `notifier`,
`sync`, `challenge-solver`, `render`, `frontend`. It is not written down in the workflow: `xtask
release-plan` derives it from the Dockerfile's `SERVICE_BINS` minus the deploy blacklist
(`[workspace.metadata.deploy.exclude]`, root `Cargo.toml`), which is what keeps `xtask` — a task
runner with a `reset` command — out of every registry. `repo-lint` fails if `release-please.yaml`
starts naming its images literally again, because a literal would publish a hand-maintained list
while every other gate stayed green.

Tags per published release: `vX.Y.Z`, `X.Y` and `latest`. Every published manifest-list digest is
signed with cosign keyless and carries an SPDX SBOM attestation.

**A service that did not change gets no tag for that release.** `timschoenle/tankovault-api:v1.0.1`
does not exist if `api` was untouched in v1.0.1, and that service's `latest` and `X.Y` stay at its
last real build. This is deliberate in both directions: it is what makes a registry's tag set the
record of which releases changed each service — which is where the next release reads its diff
base from — and it means **the chart is the only complete, uniform reference surface**. Deploy
from the chart, not from a tag you assumed exists.

## The desktop update channel

Every release also carries the desktop client — Windows `.msi` and NSIS `.exe`, Linux `.deb` and
`.AppImage`, plus a portable archive per platform — and an installed client can update *itself*
from these releases (`web/frontend/src/update`). Because that means downloading an executable and
running it, the release publishes a document the client can check first:

| Asset | What it is |
| --- | --- |
| `desktop-manifest.json` | the version, and one entry per platform: file name, SHA-256, byte length |
| `desktop-manifest.json.minisig` | minisign signature over it. **This is the gate** the client enforces |
| `desktop-manifest.json.cosign.bundle` | keyless `cosign sign-blob` over the same bytes, for public verification |
| `sha256sums.txt` | covers every asset above, for someone downloading by hand |

The client reads **nothing** out of the manifest until the minisign signature verifies against a
public key compiled into the binary, and then holds every downloaded byte to the digest and length
that manifest gives. The installers themselves carry no code signature — there is no Authenticode
certificate and no repository signature — so this is the only thing that distinguishes "the release
we published" from "whatever answered the request".

`desktop-release` builds the manifest by **file extension**, not by name: `dx` derives installer
names from the product name and version and nothing here controls that string. A file matching none
of the four extensions fails the release, rather than shipping a manifest that silently omits a
platform.

### What the reader sees when one is applied

An update is *staged* while the app runs and *applied* at the next start, before there is a
window — so the whole of it happens with nothing of ours on screen. Three things exist so that is
not indistinguishable from the app failing to open:

- a system notification naming the version, raised **before** the hand-off and waited on, because
  a notification still queued on a thread when the process exits is never delivered;
- **the app reopens by itself.** Neither Windows installer can replace a running executable, so
  the process that hands over cannot also be the one that comes back: it copies itself into the
  staging directory and gives that copy the single job of outliving the installer and starting
  what it produced (`update::install::launch`). An `AppImage` needs none of this — it replaces
  its own image and `exec`s it;
- the version is recorded before the hand-off and read once by the run that follows, which
  confirms it in the settings sheet and in a second notification — but only if the version that
  came back is the one that was promised, so a cancelled or failed install says nothing rather
  than something false.

### Why minisign for the client, when the images use cosign

Verification has to run inside a reader's app, offline, in a process built with `panic = "abort"`.
`minisign-verify` is one ed25519 check with **zero dependencies**. Verifying a keyless signature
means a Fulcio certificate chain, a Rekor inclusion proof and a TUF trust root that has to be
embedded and then goes stale — in Rust, the pre-1.0 `sigstore` crate and a much larger
`THIRD-PARTY-NOTICES` for the desktop half. That cost is free on a runner, where `cosign` is a Go
binary already installed, and it is not free in a shipped client.

So both: minisign is the client's hot path, and the cosign bundle means a release is verifiable
with the same tooling the images are.

### The signing key

The public half is compiled into the client — `TRUSTED_KEYS` in
`web/frontend/src/update/discover.rs`, generated 2026-08-07:

```
RWRJbPWpabBZ+C+5MBbE04xjL6HFoNsBZLbqqWogP7sD5BedsiJDJ4Ve
```

> **The private half must be in the `release` environment before the next release.**
> `desktop-release` fails outright without `MINISIGN_SECRET_KEY`, and then verifies its own
> signature against the key above — so a secret that does not match it fails the release too. That
> is deliberate: a release published without a usable signature is immutable and can never be
> re-signed, and every installed client would refuse it.

`MINISIGN_SECRET_KEY` is the whole of `tankovault-release.key` (both lines) and `MINISIGN_PASSWORD`
is its passphrase. Both are **`release` environment** secrets; a repository secret is not
equivalent, because a job that omits `environment: release` reads it as an empty string.

```
gh secret set MINISIGN_SECRET_KEY --env release < tankovault-release.key
gh secret set MINISIGN_PASSWORD --env release
```

Keep `tankovault-release.key` offline as well as in the secret. If it is lost, rotation below is
the only way back — and that needs a release to carry the new public key *first*.

Generating a fresh pair, if one is ever needed:

```
minisign -G -s tankovault-release.key -p tankovault-release.pub
```

`sed -n 2p tankovault-release.pub` is the line that goes in `TRUSTED_KEYS` — the `RW…` base64, not
the `untrusted comment:` line above it. A malformed entry there is neither a compile error nor a
runtime error: it just never verifies anything, and the client reports every release as untrusted.
`every_trusted_key_is_a_well_formed_minisign_key` is what stops that reaching a release.

### Rotating it — a two-release move, in this order

A release is immutable once published, and shipped clients only trust what they were compiled
with. Switching the signer in one step therefore strands every installed client on the version it
has.

1. **Release N**: add the new public key to `TRUSTED_KEYS` *beside* the old one, keep signing with
   the old secret. Clients that take this release now trust both.
2. **Release N+1**: replace `MINISIGN_SECRET_KEY` with the new secret. Clients from N accept it;
   clients still on N−1 or earlier do not, and have to download once by hand.
3. **Later**: drop the old public key from `TRUSTED_KEYS`, once you are content to leave behind
   anyone who never took N.

`TRUSTED_KEYS` is a list for exactly this reason — never replace its single entry in place.

### Verifying a desktop release by hand

```
gh release download vX.Y.Z -p 'desktop-manifest.json*' -p 'sha256sums.txt'
minisign -Vm desktop-manifest.json -P '<the RWQ… public key>'
```

or, without the key, against the workflow identity that produced it:

```
cosign verify-blob desktop-manifest.json \
  --bundle desktop-manifest.json.cosign.bundle \
  --certificate-oidc-issuer https://token.actions.githubusercontent.com \
  --certificate-identity-regexp '^https://github.com/TimSchoenle/TankoVault/'
```

Then check the installer you downloaded against the `sha256` in the manifest — that is the same
comparison the client makes, in the same order.

## Which images a release rebuilds

`xtask release-plan` decides, and it is unit-tested rather than written as shell in the workflow.

Each image is diffed from *the tag it is currently published at*, resolved by asking Docker Hub
which release tags exist for that repository — not from the previous release. A `manifest` leg
that failed at v1.0.1 is therefore picked up at v1.0.2 instead of being skipped for good.

A changed path reaches an image if:

- it belongs to a workspace package that image is built from, following the `cargo metadata`
  dependency graph (dev-dependencies excluded — they are never compiled for `cargo build --bin`);
- or it is in the `RULES` table in [`xtask/src/release_plan.rs`](../xtask/src/release_plan.rs),
  which covers the build inputs that belong to no package: `deploy/docker/`, `LICENSE`,
  `THIRD-PARTY-NOTICES` and `.cargo/` reach every image; `migrations/` and `.sqlx/` belong to
  `crates/db` because it embeds them; `openapi.json` belongs to `crates/api-client`;
- or it is under `web/`, which reaches the `frontend` image alone. The SPA is a separate
  workspace, so its manifest is read directly to find the host-workspace crates it depends on —
  without that, a regenerated `api-client` would rebuild nothing at all.

Two things it deliberately ignores. Documents, because no runtime stage copies one. And the
workspace version: release-please rewrites `[workspace.package] version` in `Cargo.toml` and every
member's version in `Cargo.lock` **on the release commit itself**, so read literally, every release
changes every image and the whole mechanism becomes a no-op. Both files are compared with those
versions masked. That is sound because no binary reads `CARGO_PKG_VERSION` — the version reaches an
image only through the dependency list `cargo auditable` embeds, which is audit metadata, not
behaviour. The consequence is that a carried-forward image's `cargo audit bin` output names the
release it was built at, which is the truth about that binary.

Anything the planner does not recognise counts as a change to **every** image, and says so in the
log. `repo-lint`'s `build-inputs-are-classified` rule is what stops that from becoming the normal
case: a new top-level path has to be given a verdict in `RULES` before it can reach `main`.

To rebuild everything regardless — a toolchain change nothing in the tree records, a registry the
planner cannot read — set the repository variable `RELEASE_REBUILD_ALL` to `true` before merging
the release pull request.

## The chart hand-off

Publishing an image deploys nothing. The deployable chart lives in
[`TimSchoenle/helm-charts`](https://github.com/TimSchoenle/helm-charts/tree/main/charts/tankovault),
and `helm-release` is what connects the two: it pins the services this run published to their new
digests and opens **one** pull request there. A pull request and not a push — the chart repository
gates its own releases, so this workflow proposes and that one decides.

It runs after `manifest`, not after `build`, because what a chart pins has to be a digest that is
signed and SBOM-attested, and `manifest` is where both happen. Each `manifest` leg exports its
Docker Hub manifest-list digest as an artifact; `helm-release` collects them. Artifacts rather
than job outputs because `manifest` is a matrix and a matrix job's `outputs` are last-writer-wins.

The Docker Hub digest is the one used because the chart's `repository` values name Docker Hub.
Both registries hold the same list digest — a manifest list is content-addressed, and both were
assembled from the same two per-architecture digests — but a chart should pin the registry it
actually pulls from.

What the pull request changes:

| Chart field | Value |
| --- | --- |
| `services.<camelCase>.image.tag`, `bootstrap.image.tag` — **for the published services only** | `vX.Y.Z@sha256:…` |
| `appVersion` | `X.Y.Z` — **no** `v`; that is why `helm-release` reads release-please's `version` output and not `tag_name` |
| `version` | the chart's own version, bumped by a patch |

The chart's version is bumped by a patch regardless of how the application version moved: an
application release does not change the chart's templates. A chart change is the chart
repository's own release.

### Why a partial pin is the correct one

`update-chart-version` writes the keys it is given and leaves the rest of `values.yaml` byte-for-
byte alone. A service that did not change therefore keeps the exact `tag@digest` string it already
had, its container image reference in the rendered Deployment is unchanged, and **Kubernetes does
not roll it**. That is the whole point: a one-line fix in `notifier` restarts `notifier`.

So the chart carries a mix of versions on purpose — `api` at `v1.0.0` beside `notifier` at
`v1.0.1`, under `appVersion: 1.0.1`. Every entry is digest-pinned, so that mix is an accurate
record of when each service last changed, not drift. `appVersion` names the release train.

Two guards keep it from becoming drift for real:

- `helm-release` compares the number of digest artifacts against the number of images `plan`
  selected, and fails if they differ. Without it, `fail-fast: false` on `manifest` meant one
  failed leg could produce a chart bump claiming a release that was never fully built. The old
  "nine digests are present" reasoning no longer holds, so it was replaced with this.
- A key that does not already exist in the chart's `values.yaml` is an error inside the action,
  applied atomically across every key — so a rename fails loudly there rather than silently
  skipping a service.

The values keys are *derived* from the binary names (`control-plane` → `services.controlPlane`;
`bootstrap` is a one-shot job and sits at the top level) rather than read from a table in the
workflow. The image set itself comes from `plan`; a hand-maintained list of the services in either
place would sit outside everything that keeps them in step.

## Build caching

`cook` and `builder` are the entire wall clock of an image build, and `builder` takes no
`ARG BIN` — so one compile per architecture serves every image. Arch-qualified BuildKit cache tags
on the GHCR `buildcache` repository carry it (a cache does not cross architectures; the `chef` base
resolves to a different digest per platform, so every key downstream of it differs):

| Tag | Written by | Read by |
| --- | --- | --- |
| `backend-deps-<arch>` | `ci.yml` `docker-deps`, off `main` only | every image build in both workflows |
| `pr-<number>-deps-<arch>` | `ci.yml` `docker-deps`, off an ordinary pull request | that pull request's own image legs |
| `release-pr-deps-<arch>` | `ci.yml` `docker-deps`, off the release-please branch only | `release-please.yaml`'s warm-up |
| `release-deps-<arch>` | `release-please.yaml` `release-deps` | the release legs |

**Every one of them is written from the Dockerfile's `build-cache` target, and that is a contract
rather than a coincidence.** `build-cache` names `builder` and `wasm-builder` in one stage, so a
single `mode=max` export carries the musl chain and the wasm chain together and any one tag is a
complete lineage on its own.

The rule that makes it necessary: **a build that imports two registry cache lineages resolves one
and drops the other**, then recompiles the dropped one from cold. `frontend` is the only image
consuming both chains, so while the wasm half lived in its own `frontend-deps-<arch>` family it was
the only leg that paid — and it paid every run, on every architecture: 43 minutes in `ci.yml`'s
`docker build (frontend)` against under a minute for its nine siblings, 39 and 29 minutes in
release v5.0.0's two `frontend` legs against one minute each. The logs show `cook-web` hitting and
`cook` plus `builder` recompiling, against a tag a sibling leg had just read in full. So: one
lineage, and no leg imports a tag written from a narrower target.

Folding the two warm-ups into one costs nothing in wall clock — `builder` and `wasm-builder` share
no edge, so BuildKit solves them concurrently — and it takes `dx build` off the `frontend` leg's
critical path, since that leg now imports `wasm-builder` instead of running it.

`release-deps` exists because both workflows fire on the push that merges the release PR, so the
CI tags are still at the previous commit when the release legs start — and the release commit
changes `Cargo.toml`, `Cargo.lock` and `CHANGELOG.md`, which misses `builder`'s `COPY . .`.
Without the warm-up every leg recompiled the workspace independently.

`SERVICE_BINS` is left at the Dockerfile's default everywhere. Narrowing it to `plan`'s set saved a
thin-LTO link per unpublished image and cost far more than it saved: the value is part of
`builder`'s cache key, so a release warm-up cooked under a narrowed one could not read anything
`ci.yml` writes — including `release-pr-deps-<arch>`, which is the same tree already compiled. The
same rule governs `SOURCE_DATE_EPOCH` and `TANKOVAULT_COMMIT`: a build arg the warm-up does not
pass is a build arg no leg may pass either.

The `plan` job has **no** compilation cache, and that is the one place the rule below costs
something real: `xtask` reaches the API crate, so building it compiles most of the backend from
cold on every release — minutes, in front of everything else. A `Swatinem/rust-cache` would remove
that and would also make a writable cache an input to the job that decides what a signed release
publishes, in the one workflow holding `packages: write` and `id-token: write`. That is the
cache-poisoning class zizmor audits for, and it is not worth those minutes. The job drops debug
info and incremental compilation instead (`CARGO_PROFILE_DEV_DEBUG: none`, `CARGO_INCREMENTAL: 0`),
which is most of a dev-profile build's cost and none of its meaning for a program that prints three
lines and exits.

If that cold build ever becomes the release's long pole, the fix is to put `xtask`'s database and
OpenAPI dependencies behind a default-on feature — `ci`, `repo-lint`, `coverage-ratchet`,
`notices`, `config-docs` and `release-plan` need none of them — not to add a cache here.

Two rules keep the warm layer from being trampled: pull requests import but never export, and no
image leg ever sets `cache-to` — `mode=max` re-exports imported layers, so each leg would upload
its own copy of the multi-gigabyte `cook` layer into a tag CI also reads. A release build is the
one build that must not poison what CI reads.

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
| `RELEASE_BOT_APP_ID`, `RELEASE_BOT_PRIVATE_KEY` | `release` environment secret | release-please, helm-release |
| `ACTIONS_MAINTENANCE_APP_ID`, `ACTIONS_MAINTENANCE_PRIVATE_KEY` | repository secret | auto-merge, auto-fix |
| `DOCKERHUB_USERNAME`, `DOCKERHUB_TOKEN` | `release` environment secret | `plan` (reading the published tag set), image publish |
| `MINISIGN_SECRET_KEY`, `MINISIGN_PASSWORD` | `release` environment secret | `desktop-release` (signing the desktop manifest) |

Every job that reads a **`release` environment** secret names `environment: release` with
`deployment: false` — `release-please`, `helm-release`, `plan`, and both publish jobs, `build` and
`manifest`. That is not optional and it fails quietly: a job that omits the environment reads the
secret as an empty
string, so `docker/login-action` reports "Username and password required" and
`create-github-app-token` reports an empty `app-id`, neither of which names the environment as
the cause. `deployment: false` keeps a secret scope from writing a deployment record per job.

The maintenance pair is a **repository** secret and deliberately not an environment one. The one
workflow that commits to a pull request branch — `auto-fix` — runs on every pull request that
touches Rust, and a secret scope that exists for publishing has no business sitting behind a job
any pull request can start. The lockfile sync named `environment: release` until 2026-08-04, back
when it was a workflow of its own.

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
version changes and `simple` does not update lockfiles. `auto-fix.yaml`'s lockfile sync closes
that, and it is load-bearing: without it the release PR carries a lockfile that disagrees with the
manifests, every `--locked` build fails (`xtask ci`, `msrv`, `supply-chain`, and the
Dockerfile's `cargo auditable build --release --locked`), and the PR can never go green. That is
a deadlock, not a flaky failure.

Two things make that harder than it sounds, and 1.2.1 hit both.

**release-please rewrites the branch.** Every new commit on `main` while the release PR is open
makes it force-push a single fresh release commit, discarding the bot commits that had fixed the
branch. The fixes have to run again on the new head — and while they lived in separate workflows
they could not reliably, because those workflows shared one concurrency group and GitHub keeps
only one *pending* run per group. A run arriving at a group that already has one running and one
pending cancels the pending one rather than queueing behind it.

Three members cost release 1.2.1: the lockfile sync was the run dropped, and both lockfiles and
`openapi.json` merged still recording 1.2.0. Cutting the group to two members did not fix the kind
of bug, only its constant — on release 1.3.0 (#76) release-please force-pushed twice in 33
seconds, `auto-fix` was cancelled on both events, `auto-format` survived, and `test (workspace)`
failed with `openapi.json is out of date`. So there is now exactly **one** workflow that commits
mechanical fixes, doing all of them in one job and one commit, and `repo-lint`'s
`concurrency-groups-hold-one-workflow` fails if a second workflow ever joins its group.

**The gate only helps if it is waited for.** `lockfile integrity` reported the 1.2.1 drift
correctly, 100 seconds before the pull request was merged by hand. Merging a release PR before
the required `ci` check reports is what turns a red gate into a broken `main`.

## Why one version, and not a release-please component per service

Publishing only what changed is the job release-please's multi-package mode looks built for: nine
`packages` entries, `separate-pull-requests: false` for a single pull request, a version and a
changelog each. It is the wrong tool here, for one decisive reason and two expensive ones.

The decisive one: **release-please attributes a commit to a component by path prefix.** A change
under `crates/domain/` sits inside no service's directory, so it would release nothing — and every
image that depends on it would silently stay on old code. That is precisely the failure the deploy
blacklist and `repo-lint` exist to prevent, reintroduced at the level above. Deciding what changed
needs the Cargo dependency graph, which release-please has no view of; `xtask release-plan` does,
and it runs at publish time where that graph is available.

The expensive ones: nine independent versions cannot come from `[workspace.package] version`, so
every member would carry its own — undoing the reasoning in the section above — and `appVersion`
would lose any single value to hold.

So release-please keeps deciding *one* version for the repository, on its own unchanged job, and
the publish path decides which images that version needs.

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

`vX.Y.Z` here is a tag that service actually has. If it was not rebuilt for that release the tag
does not exist — list what does, or read the digest out of the chart:

```sh
docker buildx imagetools inspect timschoenle/tankovault-api:latest
```

## What was replaced

The previous `release.yml` — tag-triggered, GHCR-only, amd64-only — is gone. Keeping both would
have double-built every release, since release-please pushes the tag that workflow triggered on.

[release-please]: https://github.com/googleapis/release-please
