# SPDX-License-Identifier: MIT
# Multi-stage build for the AppRafter admission-webhook binary,
# using `cargo-chef` so the dependency-build layer stays
# cacheable independently of source changes. See
# `operator/apprafter-operator/Dockerfile` for the full
# rationale — the strategy is identical, only the `--bin`
# target differs.
#
# Layer plan:
#
#   1. `chef`    — Rust + musl toolchain + cargo-chef.
#   2. `planner` — `cargo chef prepare` emits `recipe.json`,
#                  source-free dep description.
#   3. `builder` — `cargo chef cook --bin admission-webhook`
#                  builds dependencies as a separate cacheable
#                  layer; then full-source `cargo build`
#                  re-compiles only the changed crates.
#
# With the workflow's `cache-from / cache-to: type=gha,
# scope=admission-webhook`, a typical source-only push drops
# this job from ~6 min to ~1-2 min.
#
# Runtime image: distroless/static-debian12 with the `nonroot`
# user (uid 65532). The musl-built binary is fully static so it
# needs no libc / dynamic-linker support in the runtime layer.

# Base image pinned + cargo-chef installed as pre-built binary,
# registry fully qualified to avoid podman's short-name prompt
# — see `operator/apprafter-operator/Dockerfile` for full
# rationale.
FROM docker.io/library/rust:1.98-alpine3.24 AS chef
ARG CARGO_CHEF_VERSION=0.1.78
# cargo-chef switched its release tooling to cargo-dist at 0.1.75, which
# changed BOTH the asset name (.tar.gz -> .tar.xz) and the archive layout
# (the binary is now nested in a per-target directory). Hence the `xz`
# package and `--strip-components=1`; without either, the build fails at
# image-build time rather than anywhere a test would see it.
RUN apk add --no-cache musl-dev curl tar xz \
 && curl --proto '=https' --tlsv1.2 -fsSL \
      "https://github.com/LukeMathWalker/cargo-chef/releases/download/v${CARGO_CHEF_VERSION}/cargo-chef-x86_64-unknown-linux-musl.tar.xz" \
    | tar -xJ --strip-components=1 -C /usr/local/bin/ \
        cargo-chef-x86_64-unknown-linux-musl/cargo-chef \
 && chmod +x /usr/local/bin/cargo-chef
WORKDIR /workspace

FROM chef AS planner
COPY . .
RUN cargo chef prepare --recipe-path recipe.json

# `--locked` on both cargo steps, for the reason given in
# `operator/apprafter-operator/Dockerfile`: the image builds exactly what
# operator/Cargo.lock says, or fails at the cook step.
FROM chef AS builder
COPY --from=planner /workspace/recipe.json recipe.json
RUN cargo chef cook --release --locked --recipe-path recipe.json \
        --target x86_64-unknown-linux-musl \
        --bin admission-webhook
COPY . .
RUN cargo build --release --locked --bin admission-webhook \
        --target x86_64-unknown-linux-musl

FROM gcr.io/distroless/static-debian12:nonroot

COPY --from=builder \
    /workspace/target/x86_64-unknown-linux-musl/release/admission-webhook \
    /admission-webhook

USER nonroot
EXPOSE 8443
ENTRYPOINT ["/admission-webhook"]
