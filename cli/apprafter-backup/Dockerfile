# SPDX-License-Identifier: FSL-1.1-Apache-2.0
# Multi-stage build for the apprafter-backup runner binary, using
# `cargo-chef` so the dependency-build layer stays cacheable
# independently of source changes.
#
# Layer plan:
#
#   1. `chef`    — Rust + musl toolchain + cargo-chef (compiled from source).
#                  Re-built only when the base image moves. Fixes the
#                  compiler for every later stage to the image's own.
#   2. `planner` — copies cli/ workspace, checks the image's rustc against
#                  cli/rust-toolchain.toml, runs `cargo chef prepare`
#                  to emit recipe.json (source-free dep description).
#   3. `builder` — cooks deps from recipe.json (cache-stable), then
#                  builds ONLY `-p apprafter-backup` with musl target.
#   4. runtime   — alpine:3.24 with restic + ca-certificates.
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
# ONE compiler for every stage: the one this image ships.
#
# cli/rust-toolchain.toml says channel "1.98", but the image names its
# toolchain by the full version (`1.98.1-x86_64-unknown-linux-musl`), so to
# rustup "1.98" is a toolchain that is not installed — and rustup's default is
# to download it on first use. Every cargo run inside cli/ did exactly that:
# the planner, and `cargo chef cook` too, because cargo-chef copies the
# toolchain file into recipe.json and writes it back before cooking. The
# runner was built by a second compiler fetched from static.rust-lang.org at
# build time, whose patch was whatever the channel said that day rather than
# what this pinned image carries, and the build worked only because rustup
# happens to auto-install by default.
#
# RUSTUP_TOOLCHAIN outranks the toolchain file, and RUST_VERSION is the
# image's own `1.98.1`, so cook and the final build run the image's compiler
# with nothing to download. RUSTUP_AUTO_INSTALL=0 turns any toolchain that
# still resolves elsewhere into a build failure instead of a silent download.
# The file keeps the say over the MINOR: the planner refuses to build when the
# image's rustc is not the minor cli/rust-toolchain.toml names.
ENV RUSTUP_TOOLCHAIN=${RUST_VERSION} \
    RUSTUP_AUTO_INSTALL=0
# Same cargo-chef as the operator images (scripts/upstream-pins.json
# `image-cargo-chef` reads all three ARGs). Without `--version`, `cargo install`
# resolves whatever crates.io calls newest on the day of the build.
ARG CARGO_CHEF_VERSION=0.1.78
RUN apk add --no-cache musl-dev perl make \
 && cargo install cargo-chef --locked --version "${CARGO_CHEF_VERSION}"
WORKDIR /workspace/cli

FROM chef AS planner
COPY cli/ /workspace/cli/
# The builder image and cli/rust-toolchain.toml are two copies of one pin
# (scripts/upstream-pins.json `rust-toolchain-minor`). RUSTUP_TOOLCHAIN above
# means the file no longer picks the compiler in here, so check it instead:
# a FROM line bumped without the file (or the reverse) fails the build rather
# than shipping a runner built by a rustc the file does not name.
RUN channel="$(sed -n 's/^channel[[:space:]]*=[[:space:]]*"\([^"]*\)".*/\1/p' rust-toolchain.toml)" \
 && case "${RUST_VERSION}" in \
      "${channel}"|"${channel}".*) \
        echo "rustc ${RUST_VERSION} from the builder image (cli/rust-toolchain.toml: ${channel})" ;; \
      *) echo "FATAL: the builder image ships rustc ${RUST_VERSION}, but" >&2; \
         echo "cli/rust-toolchain.toml pins channel '${channel}'. Move the" >&2; \
         echo "rust:<minor>-alpine FROM line and the toolchain file together." >&2; \
         exit 1 ;; \
    esac \
 && cargo chef prepare --recipe-path recipe.json

# `--locked` on BOTH cargo steps, as release-cli.yml builds the CLI: the image
# resolves exactly what cli/Cargo.lock says, or fails. Without it a Cargo.lock
# that lags a Cargo.toml edit is silently re-resolved inside the image, so the
# runner ships dependency versions no test in the workspace ever ran.
#
# `rustup show active-toolchain` puts the compiler of each step in the build
# log; cook and build must print the same line.
FROM chef AS builder
COPY --from=planner /workspace/cli/recipe.json recipe.json
RUN rustup show active-toolchain \
 && cargo chef cook --release --locked -p apprafter-backup --recipe-path recipe.json \
        --target x86_64-unknown-linux-musl
COPY cli/ /workspace/cli/
RUN rustup show active-toolchain \
 && cargo build --release --locked -p apprafter-backup \
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
