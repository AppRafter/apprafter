# SPDX-License-Identifier: MIT
# Multi-stage build for the AppRafter admission webhook.
#
# Builder uses rust:1.83-alpine + musl for a fully-static binary so
# the runtime can be distroless/static (no libc, no shell). Final
# image runs as the unprivileged `nonroot` user (uid 65532).

FROM rust:1.83-alpine AS builder

RUN apk add --no-cache musl-dev

WORKDIR /workspace

# Copy the workspace manifests first so dependency compilation is
# cached when only source changes.
COPY Cargo.toml ./
COPY admission-webhook/Cargo.toml admission-webhook/Cargo.toml
RUN mkdir -p admission-webhook/src \
 && echo 'fn main() {}' > admission-webhook/src/main.rs \
 && echo '' > admission-webhook/src/lib.rs \
 && cargo build --release --bin admission-webhook --target x86_64-unknown-linux-musl \
 && rm -rf admission-webhook/src

# Real source.
COPY admission-webhook/src admission-webhook/src
COPY admission-webhook/tests admission-webhook/tests

RUN cargo build --release --bin admission-webhook --target x86_64-unknown-linux-musl

FROM gcr.io/distroless/static-debian12:nonroot

COPY --from=builder /workspace/target/x86_64-unknown-linux-musl/release/admission-webhook /admission-webhook

USER nonroot
EXPOSE 8443
ENTRYPOINT ["/admission-webhook"]
