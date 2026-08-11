# syntax=docker/dockerfile:1@sha256:87999aa3d42bdc6bea60565083ee17e86d1f3339802f543c0d03998580f9cb89
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
ENV RUSTFLAGS="-C target-feature=-crt-static"
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
ARG SERVICE_BINS="api bootstrap challenge-solver control-plane frontend notifier render sync worker xtask"
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
    for bin in ${SERVICE_BINS}; do cp "target/release/${bin}" "/out/${bin}"; done

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
USER 1001:1001
ENTRYPOINT ["/usr/local/bin/tankovault-service"]

# Serves the SPA and reverse-proxies /v1/* to `api` from one origin, so the client needs no CORS
# and SSE streams aren't buffered.
FROM scratch AS frontend
COPY --from=builder /sysroot-nocxx/ /
COPY --from=builder /etc/ssl/certs/ca-certificates.crt /etc/ssl/certs/ca-certificates.crt
COPY --from=builder /out/frontend /usr/local/bin/tankovault-frontend
COPY --from=wasm-builder /dist /srv/www
# See the `runtime-browser` stage: the licence terms ship with the image, not just the repo.
COPY --from=builder /app/LICENSE /LICENSE
# This image owes them twice over: it is a binary distribution itself, and it hands the browser
# bundle — a second dependency graph, `web/frontend`'s — to every reader who loads the page. The
# server publishes this file at `/third-party-notices`, which is where the SPA's rail links.
COPY --from=builder /app/THIRD-PARTY-NOTICES /THIRD-PARTY-NOTICES
ENV TANKOVAULT_FRONTEND__STATIC_DIR=/srv/www
# Numeric nonroot UID (scratch has no /etc/passwd); binds 3000, not the privileged port 80.
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
# See the `runtime-browser` stage: the licence terms ship with the image, not just the repo.
COPY --from=builder /app/LICENSE /LICENSE
COPY --from=builder /app/THIRD-PARTY-NOTICES /THIRD-PARTY-NOTICES
# Numeric nonroot UID (scratch has no /etc/passwd).
USER 1001:1001
ENTRYPOINT ["/usr/local/bin/tankovault-service"]
