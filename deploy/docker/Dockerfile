# syntax=docker/dockerfile:1
#
# Multi-stage, cargo-chef-cached build for any TankoVault service binary. Select which one
# with `--build-arg BIN=<name>`:
#   api · worker · control-plane · notifier · sync · challenge-solver · render · xtask
#
# The dependency graph is compiled once (the `cook` layer) and reused across source
# changes and across every service image.

ARG RUST_VERSION=1.94

# Build on the Debian 12 (bookworm) toolchain so the produced glibc-linked binaries stay
# compatible with the runtime images below (distroless/cc-debian12 and debian:12-slim, both
# glibc 2.36). The default `latest-rust-*` tag tracks Debian 13/trixie (glibc 2.38+), which
# makes the binaries fail at startup with `version GLIBC_2.38 not found` on debian12.
FROM lukemathwalker/cargo-chef:latest-rust-${RUST_VERSION}-bookworm AS chef
WORKDIR /app

# --- Plan: capture the dependency recipe from the whole workspace. -----------------
FROM chef AS planner
COPY . .
RUN cargo chef prepare --recipe-path recipe.json

# --- Build: cook deps (cached), then compile the requested binary. -----------------
FROM chef AS builder
COPY --from=planner /app/recipe.json recipe.json
RUN cargo chef cook --release --recipe-path recipe.json
COPY . .
# `BIN` is declared here (after cook) so changing it does not bust the dependency cache.
ARG BIN
RUN cargo build --release --bin "${BIN}" \
 && cp "target/release/${BIN}" /app/service

# --- Runtime (browser): Debian slim + Chromium for the optional `render` service. ---
# The render tier drives a real headless browser (chromiumoxide, design §9), which the
# distroless image cannot provide; build it with `--target runtime-browser`.
FROM debian:12-slim AS runtime-browser
RUN apt-get update \
 && apt-get install -y --no-install-recommends \
      chromium ca-certificates fonts-liberation \
 && rm -rf /var/lib/apt/lists/*
COPY --from=builder /app/service /usr/local/bin/tankovault-service
# Point chromiumoxide at the distro Chromium and run it without the sandbox (no user
# namespaces inside the container). Consumed by the render service's config.
ENV TANKOVAULT_RENDER__CHROME_PATH=/usr/bin/chromium \
    TANKOVAULT_RENDER__NO_SANDBOX=true
ENTRYPOINT ["/usr/local/bin/tankovault-service"]

# --- Runtime: distroless (glibc + libgcc for the rustls/reqwest binaries), nonroot. -
# This is the DEFAULT target for every non-browser service; keep it last.
FROM gcr.io/distroless/cc-debian12:nonroot AS runtime
COPY --from=builder /app/service /usr/local/bin/tankovault-service
ENTRYPOINT ["/usr/local/bin/tankovault-service"]
