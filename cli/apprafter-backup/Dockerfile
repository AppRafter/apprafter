# SPDX-License-Identifier: FSL-1.1-Apache-2.0
# Multi-stage build for the apprafter-backup runner binary, using
# `cargo-chef` so the dependency-build layer stays cacheable
# independently of source changes.
#
# Layer plan:
#
#   1. `chef`    — Rust + musl toolchain + cargo-chef (compiled from source).
#                  Re-built only when the base image moves.
#   2. `planner` — copies cli/ workspace, runs `cargo chef prepare`
#                  to emit recipe.json (source-free dep description).
#   3. `builder` — cooks deps from recipe.json (cache-stable), then
#                  builds ONLY `-p apprafter-backup` with musl target.
#   4. runtime   — alpine:3.21 with restic + ca-certificates.
#                  pg_dump / tar run in separate ephemeral helper pods
#                  (NOT this image), so no postgres-client needed here.
#
# Build context is the repo root (the cli/ workspace has no
# cross-workspace path deps into operator/).
#
# Registry is fully qualified (`docker.io/library/...`) to avoid
# interactive registry-selection prompts from podman with multiple
# `unqualified-search-registries` configured (hangs local e2e builds).
#
# ring v0.17 (pulled transitively by kube's rustls-tls feature) uses
# the `cc` crate — no perl or make needed for ring itself; `perl` and
# `make` are included because `cargo install cargo-chef --locked`
# compiles cargo-chef from source and some of its transitive C deps
# require them on musl Alpine.

FROM docker.io/library/rust:1.88-alpine3.21 AS chef
RUN apk add --no-cache musl-dev perl make && cargo install cargo-chef --locked
WORKDIR /workspace/cli

FROM chef AS planner
COPY cli/ /workspace/cli/
RUN cargo chef prepare --recipe-path recipe.json

FROM chef AS builder
COPY --from=planner /workspace/cli/recipe.json recipe.json
RUN cargo chef cook --release -p apprafter-backup --recipe-path recipe.json \
        --target x86_64-unknown-linux-musl
COPY cli/ /workspace/cli/
RUN cargo build --release -p apprafter-backup \
        --target x86_64-unknown-linux-musl

FROM alpine:3.21
RUN apk add --no-cache restic ca-certificates
COPY --from=builder \
    /workspace/cli/target/x86_64-unknown-linux-musl/release/apprafter-backup \
    /usr/local/bin/apprafter-backup
USER 65532:65532
ENTRYPOINT ["/usr/local/bin/apprafter-backup"]
