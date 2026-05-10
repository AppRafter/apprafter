# SPDX-License-Identifier: MIT
# Multi-stage build for the AppRafter admission-webhook binary.
#
# Build context is `operator/` (set by the
# release-operator.yml workflow's `context: operator`). The
# pre-existing single-member dep-prebuild trick was broken
# (workspace member resolution failed because only the webhook's
# Cargo.toml was copied, not the other workspace members') and
# never validated since no nightly / CI build ever ran against
# it before 2026-05-10. Simplified here to a single COPY +
# build — slower per CI run because the dep layer invalidates
# on every source change, but reliable. cargo-chef
# (the industry-standard layered cache for cargo in Docker) is
# the right next step and lands as a separate v0.1.x patch.
#
# Runtime image: distroless/static-debian12 with the `nonroot`
# user (uid 65532). The musl-built binary is fully static so it
# needs no libc / dynamic-linker support in the runtime layer.

FROM rust:1.85-alpine AS builder

RUN apk add --no-cache musl-dev

WORKDIR /workspace

# Whole workspace. .dockerignore (sibling file) keeps target/
# and .git/ out of the build context.
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
