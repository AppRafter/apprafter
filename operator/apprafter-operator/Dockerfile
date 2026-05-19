# SPDX-License-Identifier: FSL-1.1-Apache-2.0
# Multi-stage build for the AppRafter operator binary, using
# `cargo-chef` so the dependency-build layer stays cacheable
# independently of source changes.
#
# Layer plan (each stage is independently cacheable by Docker):
#
#   1. `chef`    — Rust + musl toolchain + cargo-chef installed.
#                  Re-built only when the base image moves.
#   2. `planner` — copies the workspace and runs
#                  `cargo chef prepare`, which emits
#                  `recipe.json`: a deterministic, source-free
#                  description of every dep + version + feature
#                  set. The `--bin` source files are NOT
#                  compiled here.
#   3. `builder` — copies `recipe.json` from `planner`, runs
#                  `cargo chef cook --bin apprafter-operator`
#                  to build EVERY dependency for the requested
#                  binary against the musl target. This is the
#                  expensive step (~5 min uncached) and the
#                  one that benefits most from Docker layer
#                  cache: it invalidates only when
#                  `Cargo.lock` / `Cargo.toml` / target triple
#                  change. A workspace source-only change
#                  → cache hit on this layer.
#                  Then the full source is copied and the
#                  final `cargo build` runs; the compiler only
#                  re-builds the changed crates (~30-60s).
#
# The release-operator.yml workflow's
# `cache-from / cache-to: type=gha,scope=operator` keys each
# layer to GitHub Actions' managed cache. First run on a fresh
# runner stays slow; every subsequent run reuses the chef-cook
# layer until `Cargo.lock` changes — typical CLI / chart-only
# pushes drop the operator-build job from ~6 min to ~1-2 min.
#
# Why the prior dep-prebuild attempt (v0.1.52 / v0.1.55) failed:
# it hand-rolled a fake workspace by copying every member's
# Cargo.toml + stubbing src/lib.rs files, which crashed in
# buildx with cargo exit 101 and a stderr truncated by the CI
# log writer. cargo-chef replaces all of that with a tool
# that's specifically designed to handle workspaces and emits a
# stable recipe regardless of member topology.
#
# Build context is `operator/` (set by the workflow's
# `context: operator`); .dockerignore (sibling file) keeps
# `target/` and `.git/` out of the upload.
#
# Runtime image: distroless/static-debian12 with the `nonroot`
# user (uid 65532). The musl-built binary is fully static so it
# needs no libc / dynamic-linker support in the runtime layer.

# Base image pinned: floating `rust:alpine` updates on every
# upstream nightly tag refresh, which invalidates the entire
# layer chain and forces a full rebuild of the cargo-chef
# `chef` layer (~1-2 min) plus every downstream cook /
# build step. Pin to a digest-aliased version tag for
# stability; bump intentionally as part of an MSRV move.
#
# cargo-chef installed as a pre-built binary, not via
# `cargo install --locked`. The latter compiles cargo-chef
# from source (~1-2 min); the binary is ~10s.
FROM rust:1.88-alpine3.21 AS chef
ARG CARGO_CHEF_VERSION=0.1.71
RUN apk add --no-cache musl-dev curl tar \
 && curl --proto '=https' --tlsv1.2 -fsSL \
      "https://github.com/LukeMathWalker/cargo-chef/releases/download/v${CARGO_CHEF_VERSION}/cargo-chef-x86_64-unknown-linux-musl.tar.gz" \
    | tar -xz -C /usr/local/bin/ cargo-chef \
 && chmod +x /usr/local/bin/cargo-chef
WORKDIR /workspace

FROM chef AS planner
COPY . .
RUN cargo chef prepare --recipe-path recipe.json

FROM chef AS builder
COPY --from=planner /workspace/recipe.json recipe.json
RUN cargo chef cook --release --recipe-path recipe.json \
        --target x86_64-unknown-linux-musl \
        --bin apprafter-operator
COPY . .
RUN cargo build --release --bin apprafter-operator \
        --target x86_64-unknown-linux-musl

FROM gcr.io/distroless/static-debian12:nonroot

COPY --from=builder \
    /workspace/target/x86_64-unknown-linux-musl/release/apprafter-operator \
    /apprafter-operator

USER nonroot
# /healthz, /readyz, /metrics — see operator-core::server.
EXPOSE 8080
ENTRYPOINT ["/apprafter-operator"]
