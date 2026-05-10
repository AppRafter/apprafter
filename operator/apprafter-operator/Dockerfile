# SPDX-License-Identifier: FSL-1.1-MIT
# Multi-stage build for the AppRafter operator binary.
#
# Mirrors admission-webhook/Dockerfile: rust:1.83-alpine + musl
# for a fully-static binary, distroless/static runtime with a
# `nonroot` user. The operator workspace has path-deps
# (operator-core, operator-rendering, operator-controllers/application)
# so we copy every member's Cargo.toml + a stub source up-front for
# the dep-prebuild stage, then bring in the real sources.

FROM rust:1.83-alpine AS builder

RUN apk add --no-cache musl-dev

WORKDIR /workspace

# Workspace + every member's manifest. Listed individually rather
# than `COPY . .` so the dep-cache layer only invalidates when a
# Cargo.toml changes, not on every source edit.
COPY Cargo.toml ./
COPY apprafter-operator/Cargo.toml apprafter-operator/Cargo.toml
COPY operator-core/Cargo.toml operator-core/Cargo.toml
COPY operator-rendering/Cargo.toml operator-rendering/Cargo.toml
COPY operator-controllers/application/Cargo.toml operator-controllers/application/Cargo.toml
COPY admission-webhook/Cargo.toml admission-webhook/Cargo.toml

# Stub sources so cargo can resolve the workspace and prebuild
# external dependencies without seeing our real code yet.
RUN mkdir -p apprafter-operator/src \
        operator-core/src \
        operator-rendering/src \
        operator-controllers/application/src \
        admission-webhook/src \
 && echo 'fn main() {}' > apprafter-operator/src/main.rs \
 && echo '' > apprafter-operator/src/lib.rs \
 && echo '' > operator-core/src/lib.rs \
 && echo '' > operator-rendering/src/lib.rs \
 && echo '' > operator-controllers/application/src/lib.rs \
 && echo 'fn main() {}' > admission-webhook/src/main.rs \
 && echo '' > admission-webhook/src/lib.rs \
 && cargo build --release --bin apprafter-operator \
        --target x86_64-unknown-linux-musl \
 && rm -rf apprafter-operator/src \
        operator-core/src \
        operator-rendering/src \
        operator-controllers/application/src \
        admission-webhook/src

# Real source.
COPY apprafter-operator apprafter-operator
COPY operator-core operator-core
COPY operator-rendering operator-rendering
COPY operator-controllers operator-controllers
COPY admission-webhook admission-webhook

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
