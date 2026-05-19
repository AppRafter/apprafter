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

FROM rust:alpine AS chef
RUN apk add --no-cache musl-dev \
 && cargo install cargo-chef --locked
WORKDIR /workspace

FROM chef AS planner
COPY . .
RUN cargo chef prepare --recipe-path recipe.json

FROM chef AS builder
COPY --from=planner /workspace/recipe.json recipe.json
RUN cargo chef cook --release --recipe-path recipe.json \
        --target x86_64-unknown-linux-musl \
        --bin admission-webhook
COPY . .
RUN cargo build --release --bin admission-webhook \
        --target x86_64-unknown-linux-musl

FROM gcr.io/distroless/static-debian12:nonroot

COPY --from=builder \
    /workspace/target/x86_64-unknown-linux-musl/release/admission-webhook \
    /admission-webhook

USER nonroot
EXPOSE 8443
ENTRYPOINT ["/admission-webhook"]
