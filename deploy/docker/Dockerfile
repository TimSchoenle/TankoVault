# syntax=docker/dockerfile:1@sha256:87999aa3d42bdc6bea60565083ee17e86d1f3339802f543c0d03998580f9cb89
#
# Every TankoVault image, from one file. Select which with `--target` and `--build-arg BIN`:
#
#   --target runtime          --build-arg BIN=<name>   scratch + a musl service binary (default)
#   --target runtime-browser  --build-arg BIN=render   Debian + Chromium, for the render tier
#   --target frontend                                  scratch + the axum SPA server + WASM bundle
#
# `BIN` is one of: api · worker · control-plane · notifier · sync · challenge-solver · render ·
# xtask. (`frontend` is also a workspace binary, but its image needs the WASM bundle alongside
# it, so it has a stage of its own rather than a `BIN` value.)
#
# Runtime images
# --------------
# Every non-browser binary is a **musl binary** shipped on a bare `scratch` image: no shell,
# no package manager, no OS — just the binary, the musl loader and `libgcc_s` it resolves,
# and a CA trust store. The `render` tier drives a real headless Chromium (chromiumoxide,
# design §9), which `scratch` cannot host, so it uses the dedicated Debian `runtime-browser`
# stage.
#
# The builder is Alpine and the build is native rather than cross, because the provider fetch
# stack links BoringSSL (`wreq`, browser TLS/HTTP2 emulation) — see the `chef` and `cook`
# stages for why that also costs the previously fully-static link.
#
# One compile, nine binaries
# --------------------------
# **The `builder` stage below compiles every binary in a single `cargo` invocation, and no stage
# in this file depends on `BIN` until the runtime stages, which are `COPY`s.** That is the whole
# performance design of this file, and it replaced a `builder` that took `ARG BIN` and compiled
# one binary per image.
#
# The old shape had two compounding costs. First, `docker compose build` produced nine distinct
# `builder` vertices, each running its own `cargo build --bin X`; because they all shared one
# `target/` cache mount and cargo takes an *exclusive* lock on a target directory, BuildKit
# serialised them. Second, and worse, nine separate invocations resolve features nine separate
# times: cargo unifies features only across the packages selected in *one* invocation, so a
# dependency that `api` and `worker` wanted with different feature sets was recompiled on the
# way past — the shared cache mount turned that into thrash rather than reuse. A local
# `docker compose build` took over twenty minutes, and the image timestamps showed the nine
# finishing two to four minutes apart, in sequence.
#
# One invocation fixes both: the workspace crates compile once, the nine leaf binaries compile
# concurrently across whatever cores the daemon has, and features are resolved once for the
# whole set. The consequence to know about is that last point — each binary is now built with
# the *union* of the feature flags its siblings ask for on shared dependencies. Features are
# additive by convention so this is behaviour-preserving, and it does not change which *crates*
# a binary links (`api` still does not depend on `tankovault-adapters`, PERF-18), but a binary
# can be marginally larger than it was when built alone.
#
# The list of binaries is `SERVICE_BINS` in the `builder` stage. It is not derived from the
# workspace, so `xtask repo-lint` checks the two agree — a `[[bin]]` added without touching this
# file would otherwise produce an image that fails at the runtime `COPY`, and only for the one
# service nobody was building.
#
# Caching
# -------
# Three layers of cache, each covering what the one below it cannot:
#
#   1. The dependency graph is compiled once by cargo-chef (the `cook` stage) and reused across
#      source changes and across every service image; that stage is what CI exports/imports via
#      the GHA cache.
#   2. A BuildKit `type=cache` mount holds the whole of `CARGO_HOME` (crate registry, git
#      checkouts *and* cargo's own lock file) warm across local rebuilds — see the `cook` stage
#      for why the lock has to be inside the mount.
#   3. A second `type=cache` mount holds `target/` across rebuilds — see the `builder` stage.
#
# **Every cache mount carries an explicit `id=`.** The default id is the target path, and this
# file builds in `/app/target` twice: once for musl under Alpine, once for `wasm32` under Debian,
# with different rustcs and different `RUSTFLAGS`. Sharing one directory between those two would
# have them invalidate each other's fingerprints on every alternating build — the exact opposite
# of a cache. Keep the ids distinct.
#
# Reproducibility
# ---------------
# Base images are pinned by digest, cargo resolves against the committed `Cargo.lock`
# (`--locked`), and image layer timestamps are clamped to `SOURCE_DATE_EPOCH` when supplied
# (`--build-arg SOURCE_DATE_EPOCH=$(git log -1 --format=%ct)`). Together with the release
# profile (`strip`, `codegen-units=1`, `lto`) this yields byte-stable images.

ARG RUST_VERSION=1.94
# Passed through so BuildKit can clamp layer mtimes for reproducible images. Empty is fine.
ARG SOURCE_DATE_EPOCH

# --- Toolchain: cargo-chef on a musl-native base. -----------------------------------
# Pinned by digest for reproducible, tamper-evident builds. The `${RUST_VERSION}` tag is
# kept in the reference for readability; the digest is what is actually resolved. Refresh
# with: docker buildx imagetools inspect lukemathwalker/cargo-chef:latest-rust-1.94-alpine
#
# Alpine rather than Debian because `wreq` links **BoringSSL**, which its build script
# compiles from C/C++ source. Debian's `musl-tools` provides `musl-gcc` only — no musl C++
# compiler and no musl `libstdc++` — so BoringSSL cannot be built for the musl target there
# without hand-assembling a cross toolchain. Alpine *is* musl, so its stock `g++` and
# `libstdc++` are already the right target and the build is native rather than cross.
FROM lukemathwalker/cargo-chef:latest-rust-${RUST_VERSION}-alpine@sha256:5b2b5c6585c537a2795a477e93ebba85b4a2887e11ee9bddd34ad607e53ccec0 AS chef
WORKDIR /app
# `musl-dev` + `g++` for BoringSSL's C/C++ sources, `cmake`/`make` to drive its build,
# `nasm` for its x86-64 assembly, `perl` for its generated-source scripts, `git` because
# `btls-sys` applies its patch set to the vendored BoringSSL tree with `git apply` (and will
# `git init` the copied tree first if it has no `.git` — so `git` is needed even offline),
# `clang-dev` for the `libclang.so` that `bindgen` loads to generate the FFI bindings, and a
# CA bundle we later bake into the certificate-less `scratch` image.
RUN apk add --no-cache musl-dev g++ cmake make nasm perl git clang-dev ca-certificates
# `bindgen` dlopens libclang at build-script time and does not search Alpine's lib dir on
# its own.
ENV LIBCLANG_PATH=/usr/lib
# `cargo auditable build` is a drop-in for `cargo build` that embeds the resolved dependency
# list into the binary itself (a `.dep-v0` ELF section). Without it a running container cannot
# be mapped back to its dependency set at all — which matters far more here than in a normal
# image, because `scratch` ships no package manager for a conventional scanner to interrogate.
# The embedded manifest survives the release profile's `strip = true` (verified), so nothing
# about the profile changes. Read it back with
# `cargo audit bin /usr/local/bin/tankovault-service`.
#
# Pinned: an unpinned `cargo install` here is an unreviewed toolchain change on every cache
# miss, and a future release raising its MSRV above the pinned RUST_VERSION would break every
# image build at once.
RUN cargo install cargo-auditable --locked --version 0.7.5

# --- Plan: capture the dependency recipes. ------------------------------------------
# Two recipes, because `web/frontend` declares `[workspace]` and is excluded from the host
# workspace (see the comment on that table) — `cargo metadata` run at the root cannot see it,
# and vice versa. Both are produced here, on the Alpine toolchain, even though the web one is
# consumed by the Debian `cook-web` stage: `cargo chef prepare` reads manifests and resolves,
# it compiles nothing, so it is target- and libc-independent. One planner means one `COPY . .`
# for the whole file instead of two.
#
# The recipes are written to `/` rather than into the tree so the `COPY --from`s below name one
# stable path regardless of which directory each was produced in.
FROM chef AS planner
COPY . .
RUN cargo chef prepare --recipe-path /recipe-server.json \
 && cd web/frontend \
 && cargo chef prepare --recipe-path /recipe-web.json

# --- Cook: compile the musl dependency graph, into an image layer. ------------------
# Split out of `builder` so it is addressable on its own: the `builder` stage below mounts this
# stage's `target/` as the seed of its cache mount (`from=cook`), and CI builds this once and
# publishes it to the GHA cache (.github/workflows/ci.yml → `docker-deps`).
FROM chef AS cook
# The `tankovault-db` repositories use sqlx's compile-time-checked query macros. There is
# no database in the build sandbox, so the macros are resolved from the committed offline
# query cache (`.sqlx/`, generated by `cargo sqlx prepare`) instead of a live connection.
ENV SQLX_OFFLINE=true
# The build is **native** (no `--target`): the host is already `x86_64-unknown-linux-musl`.
# That is not a stylistic choice. `btls-sys` runs `bindgen`, which `dlopen`s `libclang` at
# build-script time, and musl defaults to `+crt-static` — a statically linked build script
# cannot dlopen anything. `RUSTFLAGS` is *not* applied to host build scripts when `--target`
# is passed, so a cross-style invocation has no way to relax that. Building natively puts
# build scripts and the final binary under the same `RUSTFLAGS`, where `-crt-static` below
# makes the dlopen work.
#
# The consequence is a dynamically linked musl binary. The `scratch` runtimes therefore also
# carry the loader and the two shared libraries BoringSSL's C++ pulls in — still no shell,
# no package manager, no OS (see the runtime stages).
ENV RUSTFLAGS="-C target-feature=-crt-static"
# Cargo already makes concurrent builds against one `CARGO_HOME` safe: it takes an exclusive
# lock on `$CARGO_HOME/.package-cache` while it unpacks crates into `registry/src`. Mounting
# only `registry/` and `git/` left that lock *outside* the shared mount — one private copy per
# builder — so concurrent cargos shredded each other's registry and the build died with
# `failed to unpack package ...: File exists (os error 17)` and missing `Cargo.toml`s.
# Relocating CARGO_HOME so the mount covers all of it, lock included, restores the mutual
# exclusion cargo implements, without serialising the builds the way `sharing=locked` would.
# `cargo`, the rustup proxies and the `cargo-auditable` installed above are found on
# PATH (`/usr/local/cargo/bin`), which CARGO_HOME does not affect.
ENV CARGO_HOME=/cargo
COPY --from=planner /recipe-server.json recipe.json
# Cook the dependency graph. The `--mount=type=cache` mount keeps the crate registry/git
# downloads warm across local rebuilds; the compiled artefacts still land in the image layer
# so cargo-chef's cache (and the CI GHA cache) keep working.
RUN --mount=type=cache,id=tankovault-backend-cargo,target=/cargo,sharing=shared \
    cargo chef cook --release --locked --recipe-path recipe.json

# --- Build: compile every workspace binary on top of the cooked dependencies. --------
# One invocation, no `ARG BIN`. See "One compile, nine binaries" in the header for why — this
# is the stage the twenty-minute `docker compose build` was coming from.
FROM cook AS builder
COPY . .
# The set of binaries this file ships. Kept explicit rather than derived (`--workspace --bins`)
# so the images an author is producing are readable here, and cross-checked against the
# workspace manifests by `xtask repo-lint` so the two cannot drift apart silently.
ARG SERVICE_BINS="api challenge-solver control-plane frontend notifier render sync worker xtask"
# `cargo auditable` rather than plain `cargo build`: see the chef stage. It wraps only
# *workspace* members (RUSTC_WORKSPACE_WRAPPER), so the cargo-chef dependency cache above is
# untouched and the layer ordering below still holds.
#
# `from=cook,source=/app/target` seeds the mount when it is empty. This is load-bearing, not an
# optimisation: a cache mount *shadows* the image layer beneath it, so without the seed the
# first build into a cold cache would find the cooked dependencies missing and rebuild all ~600
# of them. With it, a cold cache starts from exactly what `cook` produced — which is also why
# CI (fresh runner, always-empty mount) is unaffected by this mount either way.
#
# `sharing=locked` because cargo takes an exclusive lock on the target directory for the
# duration of a build. With one invocation there is nothing to contend for in a single build;
# the flag matters for two *concurrent* `docker build`s against the same daemon, where without
# it the second cargo fails on the lock rather than waiting for it.
#
# The binaries are copied out of the mount into `/out` inside the same `RUN`: a cache mount is
# not part of the layer, so anything left in `target/` is invisible to the runtime stages.
#
# `${flags}` is deliberately unquoted — word splitting is what turns the accumulated string into
# separate `--bin` arguments, and the values are bare crate names with no whitespace in them.
RUN --mount=type=cache,id=tankovault-backend-cargo,target=/cargo,sharing=shared \
    --mount=type=cache,id=tankovault-backend-target,target=/app/target,sharing=locked,from=cook,source=/app/target \
    set -eu; \
    flags=''; \
    for bin in ${SERVICE_BINS}; do flags="${flags} --bin ${bin}"; done; \
    cargo auditable build --release --locked ${flags}; \
    mkdir -p /out; \
    for bin in ${SERVICE_BINS}; do cp "target/release/${bin}" "/out/${bin}"; done

# The linkage contract. Every runtime stage below is `scratch` or a glibc image, so it ships an
# explicit, hand-listed set of shared libraries; nothing resolves a dependency for us. If a
# binary's `DT_NEEDED` set grows beyond what its runtime stage carries, the image still builds
# and the container dies at exec with `Error loading shared library …` and a wall of
# `symbol not found` — no build, test or structure-test sees it, because the only thing that
# can is a loader running the real binary in the real image.
#
# That is not hypothetical. `wreq` 6 swapped the BoringSSL binding `boring-sys2` for `btls-sys`,
# which links `libstdc++` where the old one needed only `libgcc_s`; every service image built
# fine and every one of them failed to start. This check is what makes that a build failure.
#
# There are three runtime stages and they do not ship the same set, so neither does this check.
# `render` (`runtime-browser`) and `frontend` (its own `scratch` stage) carry no `libstdc++`;
# only the shared `runtime` stage does. One combined list would mean either allowing
# `libstdc++` everywhere — losing the guarantee for the two stages that deliberately omit it —
# or banning it everywhere, which is the bug this exists to catch. Keep the `case` in step with
# what the stages actually `COPY`: a binary checked against the wrong list is a hole in exactly
# the shape of the failure this prevents.
RUN set -eu; \
    runtime_libs='libstdc++.so.6 libgcc_s.so.1 libc.musl-x86_64.so.1'; \
    no_cxx_libs='libgcc_s.so.1 libc.musl-x86_64.so.1'; \
    rc=0; \
    for f in /out/*; do \
      bin="$(basename "$f")"; \
      case "${bin}" in render|frontend) allowed="${no_cxx_libs}" ;; *) allowed="${runtime_libs}" ;; esac; \
      for need in $(readelf -d "$f" | sed -n 's/.*(NEEDED).*\[\(.*\)\]/\1/p'); do \
        echo " ${allowed} " | grep -q " ${need} " || { \
          echo "linkage contract: ${bin} needs ${need}, which its runtime stage does not ship" >&2; \
          echo "  allowed: ${allowed}" >&2; \
          rc=1; \
        }; \
      done; \
    done; \
    [ "$rc" -eq 0 ] || { echo "Add the library to the matching runtime stage, or drop the dependency." >&2; exit 1; }; \
    echo "linkage contract: ok"

# --- Toolchain (wasm): rustc + the `dx` CLI, on Debian. -----------------------------
# The SPA bundle needs `dx`, which drives `wasm-bindgen` and a bundled `wasm-opt`; both are
# glibc binaries, so this half stays on Debian rather than joining the Alpine lineage above.
# Pinned by digest. Refresh with: docker buildx imagetools inspect rust:1.94-bookworm
FROM rust:${RUST_VERSION}-bookworm@sha256:6ae102bdbf528294bc79ad6e1fae682f6f7c2a6e6621506ba959f9685b308a55 AS wasm-chef
WORKDIR /app
# `rust-toolchain.toml` first, on its own, and *before* `rustup target add`. The base image tag
# floats within the 1.94 series (it currently ships 1.94.1) while the toolchain file pins
# 1.94.0, so without this rustup would add the target to the image's toolchain and then switch
# to a freshly downloaded pinned one that has only the targets the file itself names — leaving
# `wasm32-unknown-unknown` missing at build time. Copied alone so this layer is invalidated by a
# toolchain bump, not by every source edit.
COPY rust-toolchain.toml ./
# The `dx` CLI version must match the `dioxus` library version (web/frontend/Cargo.toml → 0.7.9)
# so asset hashing/codegen line up. `cargo-chef` cooks the wasm dependency graph below.
#
# This image no longer installs `cargo-auditable` and no longer adds the musl target or
# `musl-tools`: the server binary it used to cross-compile here is now built by the Alpine
# `builder` above, which is a workspace member it was already compiling the dependencies for.
# That deleted an entire second musl dependency-graph compile from this file.
RUN --mount=type=cache,id=tankovault-wasm-cargo-registry,target=/usr/local/cargo/registry \
    --mount=type=cache,id=tankovault-wasm-cargo-git,target=/usr/local/cargo/git \
    rustup target add wasm32-unknown-unknown \
 && cargo install cargo-chef --locked --version 0.1.77 \
 && cargo install dioxus-cli@0.7.9 --locked

# --- Cook (wasm): the SPA's dependency graph, into an image layer. -------------------
# A cooked *layer*, not a cache mount, and that distinction is the whole reason this stage
# exists. A BuildKit `type=cache` mount lives in the local builder's state and is **not**
# carried by `cache-to: type=gha`; every GitHub runner is fresh, so mounts start empty on every
# run. Before this stage existed, CI recompiled the wasm graph from cold every time — at
# `lto = "fat"`, `codegen-units = 1`. An image layer is what `type=gha` actually stores.
#
# Consequently **this stage must not mount a cache over the target directory it is cooking
# into** — a cache mount shadows the layer, and the artefacts would land somewhere the layer
# does not keep. `wasm-builder` below re-mounts it as a cache (for fast local rebuilds) and
# seeds it from this layer with `from=`.
FROM wasm-chef AS cook-web
# `web/frontend`'s recipe contains exactly **one** manifest, its own. Its two path dependencies,
# `crates/api-client` and `crates/domain`, live outside that crate's workspace, so
# `cargo chef prepare` does not capture them and cooking the web graph on a bare toolchain fails
# to resolve them. Rehydrating the *host* recipe first leaves precisely the skeleton the web
# cook needs sitting at `/app` — all 24 member manifests, root `Cargo.toml` included, which is
# what makes their `workspace = true` inheritance resolve.
#
# `--no-build` is what keeps that cheap. This used to be a full `cargo chef cook` of the host
# recipe for musl (it had to be, because this image also built the server binary), and it cost a
# ~600-crate compile purely to leave the manifests on disk. `--no-build` writes the skeleton and
# stops.
COPY --from=planner /recipe-server.json /recipe-server.json
RUN cargo chef cook --recipe-path /recipe-server.json --no-build
WORKDIR /app/web/frontend
COPY --from=planner /recipe-web.json recipe.json
# The profile and `RUSTFLAGS` here are not free choices — they must reproduce byte-for-byte what
# `dx build --platform web --release` asks cargo for, because a cargo fingerprint covers the
# profile settings *and* the rustflags. Get either wrong and this stage still succeeds, still
# costs its full wall clock, and caches precisely nothing: `dx` simply rebuilds the whole graph
# under a different fingerprint. Both values below were read back out of a real `dx` build rather
# than guessed; see the re-derivation recipe at the end of this comment.
#
#   * `dx` does not build under `release`. It synthesises a per-platform profile — `wasm-release`
#     for web + `--release` — and injects it on the cargo command line as
#     `--config profile.wasm-release.inherits="release"` and
#     `--config profile.wasm-release.strip=false` (it needs the symbols that
#     `[profile.release] strip = "symbols"` would remove, and strips the bundle itself later).
#   * `--cfg getrandom_backend="wasm_js"` is the `RUSTFLAGS` `dx` sets for wasm targets. `uuid`'s
#     `v7` feature is switched on transitively and demands an explicit randomness source on
#     `wasm32-unknown-unknown`; without the cfg the graph does not compile at all.
#
# `cargo chef cook` has no `--config` passthrough, so the profile is supplied through cargo's
# environment-variable config layer instead. **It must be supplied that way and not by declaring
# `[profile.wasm-release]` in `web/frontend/Cargo.toml`, even though that is tidier and produces
# a provably identical cargo fingerprint.** `dx` parses manifest profiles itself — that is how it
# emits "wasm-split requires LTO to be enabled in the profile" — and takes a different code path
# when it finds one it expected to inject: with the profile declared in the manifest, `wasm-opt`
# aborted (`0xc0000409`) and `dx` shipped an unoptimised bundle while still reporting success,
# which is the worst possible failure mode. Env config is invisible to that parser.
#
# The values are set inline rather than with `ENV` so they cannot leak into any stage that
# inherits this one.
#
# No `--locked`, and it is the one cook that cannot have it. `cargo chef` rewrites the version of
# every *local* package in a skeleton to `0.0.1`, so that a version bump does not invalidate a
# recipe — which means the rehydration above left `crates/api-client` and `crates/domain` on disk
# claiming `0.0.1`, while this recipe's `Cargo.lock` still records their real versions (the web
# recipe never saw those two manifests; that is the same cross-workspace gap the rehydration
# fixes). Cargo then has to reconcile exactly those two entries, and `--locked` forbids it.
#
# Dropping the flag costs nothing that matters: every *registry* dependency stays pinned by the
# recipe's lock file, cargo only rewrites what it must, and this stage compiles nothing that
# ships — it warms a cache. The build that produces the artefact still resolves against the
# committed lock, because `wasm-builder` below copies the real tree back over this skeleton and
# `dx` reads that lock.
#
# To re-derive both after a `dx` upgrade, build once and read what cargo actually recorded:
#   dx build --platform web --release
#   cat web/frontend/target/wasm32-unknown-unknown/wasm-release/.fingerprint/*/*.json | grep rustflags
# and check `target/wasm32-unknown-unknown/` for the profile directory name. The check that the
# match still holds is that a `cargo build --profile wasm-release --target wasm32-unknown-unknown`
# run straight after a `dx build --release` recompiles almost nothing.
RUN --mount=type=cache,id=tankovault-wasm-cargo-registry,target=/usr/local/cargo/registry \
    --mount=type=cache,id=tankovault-wasm-cargo-git,target=/usr/local/cargo/git \
    CARGO_PROFILE_WASM_RELEASE_INHERITS=release \
    CARGO_PROFILE_WASM_RELEASE_STRIP=false \
    RUSTFLAGS='--cfg getrandom_backend="wasm_js"' \
    cargo chef cook --recipe-path recipe.json \
      --target wasm32-unknown-unknown \
      --profile wasm-release

# --- Build (wasm): the SPA bundle. --------------------------------------------------
# The frontend crate is excluded from the host workspace but takes path dependencies on
# workspace crates, so this needs the whole tree on disk. Copy the full context; this overwrites
# the skeleton sources `cargo chef cook` left behind and leaves the cooked artefacts in place.
FROM cook-web AS wasm-builder
WORKDIR /app
COPY . .
# `dx build` emits the web bundle under target/dx/<app>/release/web/public. Locate it by path
# rather than hardcoding the app name, require exactly one `release` hit (a stale `debug` bundle
# slipping through once shipped the wrong assets), then flatten it into /dist.
#
# `from=cook-web` seeds the cache mount when it is empty, which is load-bearing rather than an
# optimisation: the mount *shadows* the cooked layer underneath, so without the seed a cold
# cache would find the dependencies missing and rebuild the entire graph. CI's mount is always
# cold, so this is the line that makes the cook stage pay off there at all.
RUN --mount=type=cache,id=tankovault-wasm-cargo-registry,target=/usr/local/cargo/registry \
    --mount=type=cache,id=tankovault-wasm-cargo-git,target=/usr/local/cargo/git \
    --mount=type=cache,id=tankovault-wasm-target,target=/app/web/frontend/target,from=cook-web,source=/app/web/frontend/target \
    set -eu; \
    cd web/frontend; \
    dx build --platform web --release; \
    matches="$(find target/dx -type d -path '*/release/web/public')"; \
    count="$(printf '%s\n' "$matches" | grep -c . || true)"; \
    if [ "$count" -ne 1 ]; then \
      echo "expected exactly one release bundle under target/dx, found ${count}:" >&2; \
      printf '%s\n' "$matches" >&2; \
      exit 1; \
    fi; \
    mkdir -p /dist; \
    cp -r "${matches}/." /dist/; \
    test -f /dist/index.html || { echo "bundle missing index.html" >&2; exit 1; }

# --- Runtime (browser): Debian slim + Chromium for the optional `render` service. ---
# The render tier drives a real headless browser (chromiumoxide, design §9), which the
# `scratch` image cannot provide; build it with `--target runtime-browser`. The service
# binary is musl-linked and runs on Debian once the musl loader is copied in below.
FROM debian:13-slim@sha256:020c0d20b9880058cbe785a9db107156c3c75c2ac944a6aa7ab59f2add76a7bd AS runtime-browser
RUN apt-get update \
 && apt-get install -y --no-install-recommends \
      chromium ca-certificates fonts-liberation \
 && rm -rf /var/lib/apt/lists/*
# Dedicated unprivileged account (UID:GID 1001:1001) so the browser tier does not run as
# root; matches the numeric nonroot user baked into the `scratch` runtime below. `--create-home`
# gives Chromium a writable HOME for its profile/cache.
RUN groupadd --gid 1001 nonroot \
 && useradd --uid 1001 --gid 1001 --no-log-init --create-home nonroot
# The service binary is musl-linked, so this glibc image needs the musl loader too. Its
# `libgcc_s` goes in a private directory named by `/etc/ld-musl-x86_64.path` rather than into
# `/usr/lib`, so it cannot shadow the glibc `libgcc_s` that Chromium loads.
#
# Deliberately **no `libstdc++`** here, unlike the `scratch` `runtime` stage. `render` is the
# only binary this stage ever carries and it does not link the C++ standard library — verified
# from `readelf -d` on the builder's output, not assumed. Keeping it out matters beyond size:
# a musl-built `libstdc++` anywhere Chromium could find it is an ABI mismatch rather than a
# near-miss. If `render` ever does need it, the `linkage contract` check in the builder stage
# fails the build and this comment is where the answer is.
COPY --from=builder /lib/ld-musl-x86_64.so.1 /lib/ld-musl-x86_64.so.1
COPY --from=builder /usr/lib/libgcc_s.so.1 /usr/lib/musl/libgcc_s.so.1
RUN echo /usr/lib/musl > /etc/ld-musl-x86_64.path
# Declared after the layers above so selecting a different binary reuses all of them. In
# practice this stage only ever carries `render`; the arg is kept so the two runtime stages
# take the same input.
ARG BIN=render
COPY --from=builder /out/${BIN} /usr/local/bin/tankovault-service
# Point chromiumoxide at the distro Chromium and run it without the sandbox (no user
# namespaces inside the container). Consumed by the render service's config.
ENV TANKOVAULT_RENDER__CHROME_PATH=/usr/bin/chromium \
    TANKOVAULT_RENDER__NO_SANDBOX=true
USER 1001:1001
ENTRYPOINT ["/usr/local/bin/tankovault-service"]

# --- Runtime (frontend): scratch + the axum SPA server + the built bundle. ----------
# The `frontend` server (services/frontend) serves the SPA and reverse-proxies `/v1/*` (REST +
# SSE) to the `api` service from one origin — the same job the retired nginx image did — so the
# SPA's same-origin API calls resolve without CORS and Server-Sent Events (`/v1/me/stream`,
# `/v1/admin/scans/stream`) stream through unbuffered.
#
# The binary comes from the Alpine `builder` like every other service, so unlike the fully
# static cross-build this replaced it is dynamically linked against musl and needs the loader
# and `libgcc_s` alongside it (see the `cook` stage for why the static link is not available).
# The CA trust store is so the proxy can reach an `https://` upstream if one is configured.
FROM scratch AS frontend
COPY --from=builder /lib/ld-musl-x86_64.so.1 /lib/ld-musl-x86_64.so.1
COPY --from=builder /usr/lib/libgcc_s.so.1 /usr/lib/libgcc_s.so.1
COPY --from=builder /etc/ssl/certs/ca-certificates.crt /etc/ssl/certs/ca-certificates.crt
COPY --from=builder /out/frontend /usr/local/bin/tankovault-frontend
COPY --from=wasm-builder /dist /srv/www
ENV TANKOVAULT_FRONTEND__STATIC_DIR=/srv/www
# Numeric nonroot UID:GID (scratch ships no /etc/passwd); shared across every TankoVault image.
# This is why the server binds 3000 rather than the privileged port 80 the nginx image used.
USER 1001:1001
EXPOSE 3000
ENTRYPOINT ["/usr/local/bin/tankovault-frontend"]

# --- Runtime: scratch (musl binary + its loader), nonroot. --------------------------
# This is the DEFAULT target for every non-browser service; keep it last. `scratch` has no
# filesystem at all, so we bake in exactly what the binary resolves at load time and nothing
# else: still no shell, no package manager, no OS.
#
# The binary is dynamically linked against musl (see the cook stage for why BoringSSL
# forces a native, non-`crt-static` build), so `ldd` resolves exactly four things — the musl
# loader, `libc` (the same file), `libgcc_s` for the unwinder BoringSSL's C++ objects
# reference, and `libstdc++` for the C++ runtime itself. All live on musl's default search path
# (`/lib:/usr/local/lib:/usr/lib`), so no loader configuration is needed. The CA trust store is
# for the rustls/reqwest/lettre clients validating outbound TLS to AniList and SMTP; `wreq`
# uses its own bundled webpki roots.
#
# `libstdc++` was **not** needed until 2026-08-01 and its absence was documented as deliberate.
# `wreq` 6 replaced the `boring-sys2` BoringSSL binding with `btls-sys`, which links the C++
# standard library where the old one resolved to `libgcc_s` alone. Nothing in the build catches
# that: the image builds, and every service dies at exec with `Error loading shared library
# libstdc++.so.6` followed by a wall of `_ZTVN10__cxxabiv1…: symbol not found`. Verified with
# `readelf -d` on the shipped binary rather than by reasoning about it — that is the only way to
# know, and it is worth redoing after any change to the TLS stack.
FROM scratch AS runtime
COPY --from=builder /lib/ld-musl-x86_64.so.1 /lib/ld-musl-x86_64.so.1
COPY --from=builder /usr/lib/libgcc_s.so.1 /usr/lib/libgcc_s.so.1
COPY --from=builder /usr/lib/libstdc++.so.6 /usr/lib/libstdc++.so.6
COPY --from=builder /etc/ssl/certs/ca-certificates.crt /etc/ssl/certs/ca-certificates.crt
# Declared last so every layer above is shared across all eight service images and BuildKit
# builds them once. `BIN` is the only thing that distinguishes them.
ARG BIN
COPY --from=builder /out/${BIN} /usr/local/bin/tankovault-service
# Numeric nonroot UID:GID (scratch ships no /etc/passwd); shared across every TankoVault image.
USER 1001:1001
ENTRYPOINT ["/usr/local/bin/tankovault-service"]
