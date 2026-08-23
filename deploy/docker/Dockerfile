# syntax=docker/dockerfile:1@sha256:ecfaec9ed6d810b56388c508f4121597bfbba70d41a6dfeee4d8cad5f295fc32
# Every TankoVault image is built from this one file. Select the target service with `--target`
# (`runtime` default, `runtime-browser` for render, `frontend` for the SPA server) and
# `--build-arg BIN=<api|worker|control-plane|notifier|sync|challenge-solver|render|bootstrap>`.

ARG RUST_VERSION=1.94

# `SOURCE_DATE_EPOCH` is deliberately **not** declared here, and it must be a **constant**
# wherever it is set. The Dockerfile frontend consumes it as a built-in build arg, so its value is
# part of the cache key of every stage below — and there is no way to keep it out of one: buildx
# propagates it from the environment into the solve as `build-arg:SOURCE_DATE_EPOCH` whether it is
# passed as `--build-arg` or exported to the process. A per-commit epoch therefore makes every
# cross-run `cache-from` a total miss: `chef` re-runs `apk add` and rebuilds `cargo-auditable` on a
# digest-pinned base, and `cook` recompiles all 549 dependencies, on every run — 24 to 45 minutes
# a build. Both workflows pin it to `"0"` at workflow level for that reason, which is also what
# makes `rewrite-timestamp=true` on the publishing legs produce byte-identical images.
# release-please.yaml documents the same trap for `SERVICE_BINS`.

# Alpine, not Debian: BoringSSL (wreq's TLS backend) needs a musl C++ toolchain that Debian's
# musl-tools lacks, so the build is native rather than cross. Pinned by digest.
FROM lukemathwalker/cargo-chef:latest-rust-${RUST_VERSION}-alpine@sha256:5b2b5c6585c537a2795a477e93ebba85b4a2887e11ee9bddd34ad607e53ccec0 AS chef
WORKDIR /app
# Copied alone, first, exactly as `wasm-chef` does below and for a sharper version of the same
# reason: the base image's toolchain floats within 1.94 while this file pins an exact patch.
# Without it here, `cook` compiled against the image's rustc and `builder` — which receives this
# file via `COPY . .` — compiled against the pinned one. Cargo's fingerprint carries the rustc
# identity, so every one of the 523 dependency crates got a different metadata hash and `builder`
# recompiled the entire graph `cook` had just built, on every single build. Measured: 427 crates
# rebuilt with this file arriving late, 11 with the two stages agreeing.
COPY rust-toolchain.toml ./
# musl-dev/g++/cmake/nasm/perl build BoringSSL from source; git lets btls-sys patch the vendored
# tree; clang-dev is bindgen's libclang; ca-certificates goes into the certificate-less scratch image.
RUN apk add --no-cache musl-dev g++ cmake make nasm perl git clang-dev ca-certificates
# bindgen dlopens libclang; Alpine doesn't search this path by default.
ENV LIBCLANG_PATH=/usr/lib
# Embeds the resolved dependency list into the binary (.dep-v0 ELF section) — the only way to
# recover a dependency set from `scratch`, which has no package manager to scan. Pinned.
RUN cargo install cargo-auditable --locked --version 0.7.5

# Two recipes: web/frontend is its own workspace, invisible to `cargo metadata` at the root.
FROM chef AS planner
COPY . .
RUN cargo chef prepare --recipe-path /recipe-server.json \
 && cd web/frontend \
 && cargo chef prepare --recipe-path /recipe-web.json

# Split from `builder` so `builder` can seed its cache mount from this stage's `target/`, and CI
# publishes this layer to the GHA cache.
FROM chef AS cook
# No DB in the build sandbox: sqlx macros resolve from the committed offline cache (.sqlx/).
ENV SQLX_OFFLINE=true
# Native build (no --target): btls-sys's bindgen dlopens libclang at build-script time, and musl's
# default +crt-static forbids dlopen in build scripts; RUSTFLAGS doesn't reach host build scripts
# under --target, so native is the only way -crt-static below reaches them too.
#
# `--build-id` is what makes a Sentry stack trace symbolicable at all. The SDK's `debug-images`
# feature reports each loaded module by its GNU build-id note, and the server looks the debug file
# up by the same id — so a binary linked without the note reports frames as bare addresses no
# upload can ever repair. It is set explicitly rather than left to the linker's default because
# that default is a property of the *base image's* toolchain, and losing it would be silent: the
# build succeeds, the images ship, and only a production stack trace shows what happened.
# `sha1` is a hash of the linked output, so it stays a function of the input and does not disturb
# the byte-identical rebuilds `SOURCE_DATE_EPOCH` buys.
ENV RUSTFLAGS="-C target-feature=-crt-static -C link-arg=-Wl,--build-id=sha1"
# Debug information for that symbolication, kept out of the shipped binary by `builder`, which
# splits it into `/debug/` and strips what it copies to `/out/`. So the images do not grow.
#
# Environment variables rather than `[profile.release]` in the root `Cargo.toml`, on purpose: a
# workspace-wide `debug`/`strip` change would slow every local and CI `--release` build for the
# benefit of the nine binaries that are actually reported on, and `web/frontend` — its own
# workspace, its own profiles — would be untouched by it anyway. This is the same shape the wasm
# half uses below.
#
# They belong in **`cook`, not `builder`**: the debug level is part of cargo's fingerprint, so
# cooking the dependency graph without them and building with them recompiles all 523 crates.
# `line-tables-only`, not full DWARF: it carries the function names, files and line numbers a
# stack trace is read for, at a fraction of the object size and link time. Full DWARF is the knob
# to turn if a report ever needs locals or inlined-frame parameters.
ENV CARGO_PROFILE_RELEASE_DEBUG=line-tables-only \
    CARGO_PROFILE_RELEASE_STRIP=false
# CARGO_HOME must cover cargo's own package-cache lock, not just registry/git — otherwise
# concurrent builds shred each other's registry (`File exists (os error 17)`).
ENV CARGO_HOME=/cargo
COPY --from=planner /recipe-server.json recipe.json
RUN --mount=type=cache,id=tankovault-backend-cargo,target=/cargo,sharing=shared \
    cargo chef cook --release --locked --recipe-path recipe.json

FROM cook AS builder
COPY . .
# Explicit list, cross-checked against the workspace by `xtask repo-lint` so a missed `[[bin]]`
# fails here, not in a broken image. Built is not the same as published: `xtask` is here because
# the compose stack builds an image of it for `migrate`/`seed`, but the deploy blacklist
# (`[workspace.metadata.deploy.exclude]`, root `Cargo.toml`) keeps it out of every registry.
ARG SERVICE_BINS="api bootstrap challenge-solver config-contract control-plane frontend notifier render sync worker xtask"
# `from=cook,source=/app/target` seeds the mount so a cold cache (CI's is always cold) starts from
# `cook`'s output instead of rebuilding every dependency. `sharing=locked` lets concurrent builds
# queue on cargo's target-dir lock rather than race it.
# `${flags}` is unquoted so word splitting turns it into separate --bin args.
#
# **The seed above currently delivers nothing, and the package selection is not why.** Measured on
# CI, this stage recompiles *all 523* third-party crates `cook` had just compiled, every build —
# `cook`'s 11 minutes are pure waste and cargo-chef is buying nothing here. The `/cargo` mount is
# shared and works (zero re-downloads in this stage), so it is specifically the compiled artefacts
# in `/app/target` that never come back.
#
# Building the whole workspace here instead of this `--bin` list was tried, on the theory that
# feature unification over a different package set was invalidating the fingerprints. It is not:
# the redundant recompiles went 489-of-512 to 523-of-549 — the same ~95%, just more crates. It is
# reverted rather than kept because it compiled 26 extra crates for no gain. `cargo auditable` is
# not the cause either: it sets `RUSTC_WORKSPACE_WRAPPER`, so switching to it rebuilds workspace
# members only, verified locally at one crate rather than a graph.
#
# So the cause is still unknown, and the next person should start at whether the `from=cook`
# seeding populates the mount at all rather than re-testing either of the above.
RUN --mount=type=cache,id=tankovault-backend-cargo,target=/cargo,sharing=shared \
    --mount=type=cache,id=tankovault-backend-target,target=/app/target,sharing=locked,from=cook,source=/app/target \
    set -eu; \
    flags=''; \
    for bin in ${SERVICE_BINS}; do flags="${flags} --bin ${bin}"; done; \
    cargo auditable build --release --locked ${flags}; \
    mkdir -p /out; \
    for bin in ${SERVICE_BINS}; do \
      mkdir -p "/debug/${bin}"; \
      objcopy --only-keep-debug "target/release/${bin}" "/debug/${bin}/${bin}.debug"; \
      objcopy --strip-debug --strip-unneeded "target/release/${bin}" "/out/${bin}"; \
    done

# What `/out` ships and what `/debug` keeps have to stay tied to each other, and both ways of
# breaking that tie are silent.
#
# **The build-id is the tie.** Sentry matches a frame to a debug file by it and by nothing else:
# an upload whose id does not match the running binary is accepted, stored, and never consulted
# again. A binary with no note at all reports addresses forever. Neither shows up as an error
# anywhere — the release publishes, the symbolication just never happens — so it is checked here,
# on every image build, rather than discovered from a production issue.
#
# **`.dep-v0` is checked in the same pass** because it is the section a stripping pass is most
# likely to take with it: `cargo auditable` writes the resolved dependency list into a
# non-allocated section, and it is the only thing that makes a `scratch` image scannable — the
# SBOM `release-please.yaml` attests is read out of it and there is no package database to fall
# back to.
#
# `2>/dev/null` on the debug file alone: `--only-keep-debug` output has no allocated `.interp`,
# and readelf warns about the missing interpreter on every one of them. Not suppressed for the
# shipped binary, where the same warning would be a real finding.
RUN set -eu; \
    rc=0; \
    for bin in ${SERVICE_BINS}; do \
      id="$(readelf -n "/out/${bin}" | sed -n 's/.*Build ID: \([0-9a-f][0-9a-f]*\).*/\1/p')"; \
      debug_id="$(readelf -n "/debug/${bin}/${bin}.debug" 2>/dev/null | sed -n 's/.*Build ID: \([0-9a-f][0-9a-f]*\).*/\1/p')"; \
      if [ -z "${id}" ]; then \
        echo "symbol contract: ${bin} carries no GNU build-id, so no upload can symbolicate it" >&2; \
        rc=1; \
      elif [ "${id}" != "${debug_id}" ]; then \
        echo "symbol contract: ${bin} is build-id ${id} but its debug file is ${debug_id:-none}" >&2; \
        rc=1; \
      fi; \
      readelf -S "/out/${bin}" | grep -q '\.dep-v0' || { \
        echo "symbol contract: stripping removed ${bin}'s .dep-v0 section; the SBOM has nothing to read" >&2; \
        rc=1; \
      }; \
    done; \
    [ "$rc" -eq 0 ] || { echo "Check the RUSTFLAGS and the objcopy flags in the stage above." >&2; exit 1; }; \
    echo "symbol contract: ok"

# Every runtime stage ships a hand-picked set of shared libraries; a binary linking one more than
# its stage carries still builds and only fails at exec (`wreq` 6 → `btls-sys` added a `libstdc++`
# dependency and every image shipped broken). This check catches that here instead.
RUN set -eu; \
    arch="$(uname -m)"; \
    runtime_libs="libstdc++.so.6 libgcc_s.so.1 libc.musl-${arch}.so.1"; \
    no_cxx_libs="libgcc_s.so.1 libc.musl-${arch}.so.1"; \
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

# Stages loader/libgcc_s/libstdc++ into fixed, per-arch paths since `COPY` can't run a shell to
# resolve them — this previously hardcoded x86_64 and was silently wrong on arm64. Debian's `/lib`
# is a symlink to `/usr/lib`, which BuildKit refuses to copy a directory over, so the browser tree
# stages the loader under /usr/lib instead of /lib.
RUN set -eu; \
    arch="$(uname -m)"; \
    mkdir -p /sysroot/lib /sysroot/usr/lib \
             /sysroot-nocxx/lib /sysroot-nocxx/usr/lib \
             /sysroot-browser/usr/lib/musl /sysroot-browser/etc; \
    for root in /sysroot /sysroot-nocxx; do \
      cp "/lib/ld-musl-${arch}.so.1" "${root}/lib/ld-musl-${arch}.so.1"; \
    done; \
    cp "/lib/ld-musl-${arch}.so.1" "/sysroot-browser/usr/lib/ld-musl-${arch}.so.1"; \
    cp /usr/lib/libgcc_s.so.1 /sysroot/usr/lib/libgcc_s.so.1; \
    cp /usr/lib/libstdc++.so.6 /sysroot/usr/lib/libstdc++.so.6; \
    cp /usr/lib/libgcc_s.so.1 /sysroot-nocxx/usr/lib/libgcc_s.so.1; \
    cp /usr/lib/libgcc_s.so.1 /sysroot-browser/usr/lib/musl/libgcc_s.so.1; \
    echo /usr/lib/musl > "/sysroot-browser/etc/ld-musl-${arch}.path"

# The configuration contract each published image carries: every `TANKOVAULT_*` key the binary
# inside it actually reads, plus the JSON Schema a chart validates its rendered `config.toml`
# against. `TimSchoenle/helm-charts` fetches it by digest and gates its manifests on it.
#
# Generated from the binary `builder` has already compiled, by running `config-contract` out of
# `/out` — so this stage costs one process per service and no compilation at all. It is also why
# `config-contract` is in `SERVICE_BINS`: it needs the same link of the same config roots the
# services were built from.
#
# **The document and the labels come out of one run**, which is the whole point. The three
# `dev.terrace.config.*` values sit in a marked region in each runtime stage below, because a
# `LABEL` key can be interpolated from nothing; `just dockerfile-labels` rewrites those regions
# from this same generator, and what stops them drifting is that CI checks the *built image*
# against the `.labels` file this stage wrote. See `.github/workflows/ci.yml`.
#
# Deliberately **no** `--revision` / `--created`. Both are legitimate contract fields and both
# make the document differ between two builds of one commit — which would break the chain this
# design rests on: the committed copy under `docs/contracts/`, the copy exported to the host, the
# copy inside the image and the artifact attached to the digest are all the same bytes, and every
# one of those equalities is checked. `app.version` already says which release this is.
FROM builder AS contract-builder
RUN set -eu; \
    mkdir -p /contract; \
    for service in $(/out/config-contract --services); do \
      /out/config-contract --service "${service}" --format contract > "/contract/${service}.json"; \
      /out/config-contract --service "${service}" --format labels   > "/contract/${service}.labels"; \
    done; \
    test -s /contract/api.json || { echo "contract generation produced nothing" >&2; exit 1; }

# The documents alone, so `--output type=local` exports a handful of files rather than the
# builder's whole filesystem — the host needs them for `oras attach` and for the label check.
FROM scratch AS contract
COPY --from=contract-builder /contract/ /

# The split-out debug information, plus the source bundle that turns a symbolicated frame into an
# actual line of code in the Sentry UI. Built only by the `symbols` job in
# `.github/workflows/release-please.yaml`; no published image contains this stage or anything it
# produces.
#
# **The bundling has to happen in here**, which is the whole reason this stage exists rather than
# `sentry-cli debug-files upload --include-sources` on the runner. The paths DWARF records are
# this container's — `/app/services/api/src/...` for the workspace, `/cargo/registry/src/...` for
# the 523 dependencies — and neither exists on a GitHub runner, so `--include-sources` there
# resolves nothing and uploads a debug file with no source attached, successfully and silently.
# `bundle-sources` run next to the sources writes a `.src.zip` keyed by the same debug id, which
# uploads from anywhere.
#
# `/cargo` is remounted because the dependency sources live on it and not in `builder`'s layers.
#
# sentry-cli is pinned by version **and digest**, and deliberately carries no `renovate:` marker:
# its releases publish no checksum file, so the two values are one pair a bot cannot update
# together — a version bumped without its digest would fail this build with a checksum mismatch
# that names neither. Move them together, by hand, or not at all; a sentry-cli a few versions
# behind reads the same DWARF.
FROM builder AS debug-bundler
#
# The two digests are spelled `CLI_DIGEST_*` and not `SENTRY_CLI_SHA256_*` on purpose: gitleaks'
# `sentry-access-token` rule matches any 64-hex value within twenty characters of the word
# `sentry`, so the obvious name makes a published checksum read as a leaked credential and fails
# `secret scan` on every pull request. Renaming keeps that rule at full strength, which an inline
# `gitleaks:allow` would not.
ARG SENTRY_CLI_VERSION=3.6.2
ARG CLI_DIGEST_X86_64=3a4bbf2c0d06378d4e59b337647483751a0a2b1603db5fd4991847d0cfd6478c
ARG CLI_DIGEST_AARCH64=ff112ecf694b7d6b3629a6228ed4e3f7a0d51401bdf48a5051a79d8749dccd06
RUN --mount=type=cache,id=tankovault-backend-cargo,target=/cargo,sharing=shared \
    set -eu; \
    arch="$(uname -m)"; \
    case "${arch}" in \
      x86_64)  asset="sentry-cli-Linux-x86_64";  sha="${CLI_DIGEST_X86_64}" ;; \
      aarch64) asset="sentry-cli-Linux-aarch64"; sha="${CLI_DIGEST_AARCH64}" ;; \
      *) echo "no sentry-cli asset is pinned for ${arch}" >&2; exit 1 ;; \
    esac; \
    wget -q -O /tmp/sentry-cli \
      "https://github.com/getsentry/sentry-cli/releases/download/${SENTRY_CLI_VERSION}/${asset}"; \
    echo "${sha}  /tmp/sentry-cli" | sha256sum -c -; \
    chmod +x /tmp/sentry-cli; \
    for dir in /debug/*; do \
      bin="$(basename "${dir}")"; \
      /tmp/sentry-cli debug-files bundle-sources "${dir}/${bin}.debug"; \
      ls "${dir}"/*.src.zip >/dev/null 2>&1 || { \
        echo "no source bundle was produced for ${bin}; its debug file records no source paths" >&2; \
        exit 1; \
      }; \
    done; \
    rm -f /tmp/sentry-cli

# One directory per binary, so the uploading job can select the images this release actually
# published without having to map a debug id back to a service name.
FROM scratch AS debug-symbols
COPY --from=debug-bundler /debug/ /

# `dx` needs glibc (wasm-bindgen/wasm-opt), so this half stays on Debian rather than Alpine.
FROM rust:${RUST_VERSION}-bookworm@sha256:6ae102bdbf528294bc79ad6e1fae682f6f7c2a6e6621506ba959f9685b308a55 AS wasm-chef
WORKDIR /app
# Copied alone, before `rustup target add`: the base image's toolchain floats within 1.94 while
# this file pins an exact patch, so without this order wasm32-unknown-unknown ends up missing.
COPY rust-toolchain.toml ./
# `dx`'s version must match `dioxus` (web/frontend/Cargo.lock) or asset hashing/codegen mismatch —
# `dx bundle` refuses outright on a mismatch, and `dx build` is not guaranteed to. The desktop
# release job (`.github/workflows/release-please.yaml`) pins the same version; both track the
# lockfile.
RUN --mount=type=cache,id=tankovault-wasm-cargo-registry,target=/usr/local/cargo/registry \
    --mount=type=cache,id=tankovault-wasm-cargo-git,target=/usr/local/cargo/git \
    rustup target add wasm32-unknown-unknown \
 && cargo install cargo-chef --locked --version 0.1.77 \
 && cargo install dioxus-cli@0.7.10 --locked

# A cooked layer, not a cache mount: `type=cache` mounts aren't carried by `cache-to: type=gha`, so
# every fresh CI runner would start empty. Must not mount a cache over the target dir it cooks
# into, or the mount shadows the layer and nothing lands in it.
FROM wasm-chef AS cook-web
# Rehydrates the host recipe first (--no-build, skeleton only): web/frontend's own recipe omits its
# two path deps (crates/api-client, crates/domain), which live outside its workspace.
COPY --from=planner /recipe-server.json /recipe-server.json
RUN cargo chef cook --recipe-path /recipe-server.json --no-build
WORKDIR /app/web/frontend
COPY --from=planner /recipe-web.json recipe.json
# Profile/RUSTFLAGS must exactly match `dx build --release`'s own cargo invocation or this cache
# buys nothing (fingerprint mismatch). Env vars, not [profile.wasm-release] in Cargo.toml — `dx`
# parses manifest profiles itself and previously shipped an unoptimised bundle while reporting
# success when one was declared there. No --locked: the skeleton rewrites local packages to 0.0.1.
RUN --mount=type=cache,id=tankovault-wasm-cargo-registry,target=/usr/local/cargo/registry \
    --mount=type=cache,id=tankovault-wasm-cargo-git,target=/usr/local/cargo/git \
    CARGO_PROFILE_WASM_RELEASE_INHERITS=release \
    CARGO_PROFILE_WASM_RELEASE_STRIP=false \
    RUSTFLAGS='--cfg getrandom_backend="wasm_js"' \
    cargo chef cook --recipe-path recipe.json \
      --target wasm32-unknown-unknown \
      --profile wasm-release

FROM cook-web AS wasm-builder
WORKDIR /app
# The short commit the footer's meta line shows. Empty by default — `build_info::commit()`
# then omits the segment rather than printing "unknown", and a local build keeps a stable
# cache key for this stage, which is the expensive one. CI passes the real value.
ARG TANKOVAULT_COMMIT=""
COPY . .
# Located by path, not app name; requires exactly one `release` hit (a stale debug bundle once
# shipped). `from=cook-web` seeds the mount so CI's always-cold cache starts from the cooked layer.
# This id must stay distinct from the Alpine target mount above — different rustc/RUSTFLAGS, so
# sharing one id would invalidate each other's cache on every alternating build.
RUN --mount=type=cache,id=tankovault-wasm-cargo-registry,target=/usr/local/cargo/registry \
    --mount=type=cache,id=tankovault-wasm-cargo-git,target=/usr/local/cargo/git \
    --mount=type=cache,id=tankovault-wasm-target,target=/app/web/frontend/target,from=cook-web,source=/app/web/frontend/target \
    set -eu; \
    cd web/frontend; \
    TANKOVAULT_COMMIT="${TANKOVAULT_COMMIT}" dx build --platform web --release; \
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

# The warm-up target both workflows build. Nothing is published from it; it exists to be a cache.
#
# **A build that imports two registry cache lineages resolves one and drops the other**, and
# BuildKit then re-runs the dropped lineage's vertices from cold. `frontend` is the only image
# consuming both — the musl server binary from `builder`, the SPA bundle from `wasm-builder` — so
# while the wasm half lived in a cache tag of its own it was the only leg that paid, and it paid
# every run, on every architecture. Measured on the push that released v5.0.0: `ci.yml`'s
# `docker build (frontend)` took 43 minutes against under a minute for each of its nine siblings,
# and the release's two `frontend` legs took 39 and 29 against one minute each. All four logs show
# the same thing — `cook-web` a cache hit, `cook` and `builder` recompiling — against a tag a
# sibling leg had just read in full.
#
# Naming both stages here puts them in one solve, so one `mode=max` export carries both chains
# under one tag and every consumer imports exactly one. That is the whole fix; the copies below
# are only what forces the two stages into the graph.
#
# It is also strictly cheaper than the two warm-ups it replaces: `builder` and `wasm-builder`
# share no edge, so BuildKit runs them concurrently and the warm-up's wall clock is the longer of
# the two rather than their sum — and `dx build`, five minutes that used to sit on the `frontend`
# leg's critical path, becomes an import there.
FROM scratch AS build-cache
COPY --from=builder /out/ /out/
COPY --from=wasm-builder /dist/ /dist/

# Debian + Chromium for the render tier (chromiumoxide, design §9); `scratch` can't host a browser.
FROM debian:13-slim@sha256:3a39a0592364683e6bab97937b72cad5a8fa6dcbbee90edb3bb48c7f8e94f258 AS runtime-browser
RUN apt-get update \
 && apt-get install -y --no-install-recommends \
      chromium ca-certificates fonts-liberation \
 && rm -rf /var/lib/apt/lists/*
# Unprivileged 1001:1001, matching the scratch runtime's nonroot user.
RUN groupadd --gid 1001 nonroot \
 && useradd --uid 1001 --gid 1001 --no-log-init --create-home nonroot
# musl loader/libgcc_s go in a private dir, not /usr/lib, so they can't shadow the glibc copies
# Chromium loads. Deliberately no libstdc++: render doesn't link it, and a musl one anywhere
# Chromium could find it would be an ABI mismatch, not a near-miss.
COPY --from=builder /sysroot-browser/ /
# Asserts the merged-/usr layout the loader path depends on, so a base-image change fails the
# build instead of failing silently at exec.
RUN set -eu; \
    interp="/lib/ld-musl-$(uname -m).so.1"; \
    [ -f "${interp}" ] || { echo "merged-/usr assumption broken: ${interp} does not resolve" >&2; exit 1; }
# Only ever built with `render`; kept as an ARG so both runtime stages take the same input.
ARG BIN=render
COPY --from=builder /out/${BIN} /usr/local/bin/tankovault-service
# The offline copy of this image's configuration contract. Read by anything holding the image
# without a registry — `docker save`, an air-gapped mirror, a future initContainer — and by the
# chart repo's refresh when a registry does not implement the referrers API.
COPY --from=contract-builder /contract/${BIN}.json /config/contract.json
# PolyForm Noncommercial's Notices clause: the terms travel with every copy of the software, and
# a published image is a copy. `.dockerignore` exempts LICENSE from the docs exclusion for this.
COPY --from=builder /app/LICENSE /LICENSE
# The dependencies' terms, which are a separate obligation from the line above: an image is a
# binary distribution, and nearly every licence in `deny.toml`'s allow list requires its text to
# accompany one. Generated by `xtask notices`; `container-structure-test` asserts it is here.
COPY --from=builder /app/THIRD-PARTY-NOTICES /THIRD-PARTY-NOTICES
# No sandbox: no user namespaces inside the container.
ENV TANKOVAULT_RENDER__CHROME_PATH=/usr/bin/chromium \
    TANKOVAULT_RENDER__NO_SANDBOX=true
# What makes the document above discoverable without pulling a layer: a registry client reads
# these three off the image config blob in one request. All three are constants of *this
# deployment* — the envelope version, where the offline copy is, and the loader prefix every
# service shares — which is what lets one hand-written block serve nine images.
#
# `LABEL` takes no interpolation and `--build-arg` cannot reach a file produced inside a builder
# stage, so these are written from `config-contract --format dockerfile` — by hand, or by `just
# dockerfile-labels`, which is the same bytes without the copying — rather than derived.
# Two things hold them honest, and neither is a source diff: `xtask config-contract` compares
# every marked region in this file against the generator, and CI compares the **built image**
# against the `.labels` file the same generator run wrote. A LABEL block with nothing checking it
# is the failure mode two earlier drafts of this design shipped.
#
# The markers are `terrace-config`'s own, and they are what the check and the rewrite both cut on.
# Finding the `LABEL` line and following its continuations — what this repository did before —
# reads correctly right up until the block is written as one `LABEL` per line, and then compares
# one line of three and passes.
# terrace-config:labels:begin
LABEL dev.terrace.config.contract.version="1" \
      dev.terrace.config.contract.path="/config/contract.json" \
      dev.terrace.config.prefix="TANKOVAULT_"
# terrace-config:labels:end
USER 1001:1001
ENTRYPOINT ["/usr/local/bin/tankovault-service"]

# Serves the SPA and reverse-proxies /v1/* to `api` from one origin, so the client needs no CORS
# and SSE streams aren't buffered.
FROM scratch AS frontend
COPY --from=builder /sysroot-nocxx/ /
COPY --from=builder /etc/ssl/certs/ca-certificates.crt /etc/ssl/certs/ca-certificates.crt
COPY --from=builder /out/frontend /usr/local/bin/tankovault-frontend
# This image's own configuration contract; see the `runtime-browser` stage.
COPY --from=contract-builder /contract/frontend.json /config/contract.json
COPY --from=wasm-builder /dist /srv/www
# See the `runtime-browser` stage: the licence terms ship with the image, not just the repo.
COPY --from=builder /app/LICENSE /LICENSE
# This image owes them twice over: it is a binary distribution itself, and it hands the browser
# bundle — a second dependency graph, `web/frontend`'s — to every reader who loads the page. The
# server publishes this file at `/third-party-notices`, which is where the SPA's rail links.
COPY --from=builder /app/THIRD-PARTY-NOTICES /THIRD-PARTY-NOTICES
ENV TANKOVAULT_FRONTEND__STATIC_DIR=/srv/www
# Numeric nonroot UID (scratch has no /etc/passwd); binds 3000, not the privileged port 80.
# The contract labels; see the `runtime-browser` stage for what holds them honest.
# terrace-config:labels:begin
LABEL dev.terrace.config.contract.version="1" \
      dev.terrace.config.contract.path="/config/contract.json" \
      dev.terrace.config.prefix="TANKOVAULT_"
# terrace-config:labels:end
USER 1001:1001
EXPOSE 3000
ENTRYPOINT ["/usr/local/bin/tankovault-frontend"]

# Default target for every non-browser service. `scratch` bakes in only what the binary resolves
# at load time: no shell, no package manager, no OS. Needs libstdc++ alongside libgcc_s: wreq's
# BoringSSL binding (btls-sys) links the C++ runtime, and dropping it fails silently at exec.
FROM scratch AS runtime
COPY --from=builder /sysroot/ /
COPY --from=builder /etc/ssl/certs/ca-certificates.crt /etc/ssl/certs/ca-certificates.crt
ARG BIN
COPY --from=builder /out/${BIN} /usr/local/bin/tankovault-service
# This image's own configuration contract; see the `runtime-browser` stage.
COPY --from=contract-builder /contract/${BIN}.json /config/contract.json
# See the `runtime-browser` stage: the licence terms ship with the image, not just the repo.
COPY --from=builder /app/LICENSE /LICENSE
COPY --from=builder /app/THIRD-PARTY-NOTICES /THIRD-PARTY-NOTICES
# Numeric nonroot UID (scratch has no /etc/passwd).
# The contract labels; see the `runtime-browser` stage for what holds them honest.
# terrace-config:labels:begin
LABEL dev.terrace.config.contract.version="1" \
      dev.terrace.config.contract.path="/config/contract.json" \
      dev.terrace.config.prefix="TANKOVAULT_"
# terrace-config:labels:end
USER 1001:1001
ENTRYPOINT ["/usr/local/bin/tankovault-service"]
