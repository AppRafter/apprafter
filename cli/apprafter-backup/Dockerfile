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

FROM docker.io/library/rust:1.98-alpine3.24 AS chef
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

FROM alpine:3.24

# restic is the tool this image EXISTS to run, and until 2026-09 its version
# was chosen silently by the Alpine tag above — 3.21 gave 0.17.3, 3.24 gives
# 0.18.1. Nothing declared it and nothing watched it, so a base-image bump
# moved the binary that writes and restores customer backups as a side effect.
#
# Same rule as everywhere else in this tree: pin the MINOR, let the patch
# float. apk cannot express that, so assert it instead — `apk add` still
# resolves the newest 0.18.x (security fixes keep arriving), and the build
# FAILS LOUDLY if a future Alpine bump would change the minor underneath us.
# An exact `restic=0.18.1-r0` pin would break on every Alpine rebuild of the
# same upstream version, which is churn without safety.
ARG RESTIC_MINOR=0.18
RUN apk add --no-cache restic ca-certificates \
 && installed="$(restic version | awk '{print $2}')" \
 && case "$installed" in \
      "${RESTIC_MINOR}".*) echo "restic ${installed} (pinned minor ${RESTIC_MINOR})" ;; \
      *) echo "FATAL: alpine:3.24 ships restic ${installed}, expected ${RESTIC_MINOR}.x." >&2; \
         echo "The backup runner's restic moved with the base image. Re-read the" >&2; \
         echo "restic changelog for wire/repository-format changes before bumping" >&2; \
         echo "RESTIC_MINOR -- 0.18.0 already made four S3 behaviours mandatory." >&2; \
         exit 1 ;; \
    esac
COPY --from=builder \
    /workspace/cli/target/x86_64-unknown-linux-musl/release/apprafter-backup \
    /usr/local/bin/apprafter-backup
USER 65532:65532
ENTRYPOINT ["/usr/local/bin/apprafter-backup"]
