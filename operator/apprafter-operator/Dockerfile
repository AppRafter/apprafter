# SPDX-License-Identifier: FSL-1.1-MIT
# Multi-stage build for the AppRafter operator binary.
#
# Build context is `operator/` (set by the
# release-operator.yml workflow's `context: operator`). The
# v0.1.52/v0.1.55 attempt at a layered dep-prebuild stage
# (copying every workspace member's Cargo.toml + stubbing
# src/lib.rs files, then building, then COPY-ing real sources)
# failed in buildx with cargo exit 101 and the actual stderr
# truncated by the CI log writer. Simplified here to a single
# COPY + build — slower per CI run because the dep layer
# invalidates on every source change, but reliable. cargo-chef
# (the industry-standard layered cache for cargo in Docker) is
# the right next step and lands as a separate v0.1.x patch.
#
# Runtime image: distroless/static-debian12 with the `nonroot`
# user (uid 65532). The musl-built binary is fully static so it
# needs no libc / dynamic-linker support in the runtime layer.

FROM rust:1.83-alpine AS builder

RUN apk add --no-cache musl-dev

WORKDIR /workspace

# Whole workspace. .dockerignore (sibling file) keeps target/
# and .git/ out of the build context.
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
