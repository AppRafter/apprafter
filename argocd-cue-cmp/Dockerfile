# SPDX-License-Identifier: FSL-1.1-Apache-2.0
#
# argocd-cue-cmp — Argo CD Config Management Plugin sidecar
# for CUE compilation of user app repositories (ADR 0029).
# Mounted into the `argocd-repo-server` Deployment via
# `spec.repoServer.extraContainers` from the platform-stack
# Helm chart.
#
# Image strategy:
#   - Alpine base (~5 MB) keeps the sidecar memory floor low.
#     The CUE binary alone is ~25 MB; total runtime footprint
#     under 50 MB matches ADR 0029's overhead estimate.
#   - `cuelang/cue` upstream image is a viable alternative but
#     we want to control the entrypoint wrapper and plugin.yaml
#     packaging; building thin atop Alpine is simpler than
#     extending the upstream image.
#   - Pinned cue version via build arg so the image's tag and
#     the embedded cue version don't drift.

ARG CUE_VERSION=v0.10.0
ARG ALPINE_VERSION=3.20

# ---- stage 1: fetch cue binary --------------------------------
#
# Multi-stage so the final image doesn't carry curl + apk
# state. CUE distribution: official tarball from GitHub
# Releases. amd64 only — Argo CD's repo-server runs amd64 in
# practice; arm64 lands when tier-1 substrate flips
# (`Infrastructure.spec.nodes[].arch`, not in M1.5 scope).
FROM alpine:${ALPINE_VERSION} AS fetcher
ARG CUE_VERSION
RUN apk add --no-cache curl ca-certificates tar
WORKDIR /tmp
RUN curl -fsSL \
    "https://github.com/cue-lang/cue/releases/download/${CUE_VERSION}/cue_${CUE_VERSION}_linux_amd64.tar.gz" \
    -o cue.tgz \
 && tar -xzf cue.tgz \
 && chmod +x cue \
 && ./cue version

# ---- stage 2: final image -------------------------------------
#
# Argo CD's CMP sidecar contract:
#   - `/home/argocd/cmp-server/config/plugin.yaml` declares
#     the plugin's discovery + generate commands.
#   - The argocd-cmp-server binary is shipped by Argo CD; our
#     job is to provide a writable filesystem with the cue
#     binary and the plugin manifest.
#   - The repo-server runs each sidecar's discover + generate
#     by mounting the user's repo into the sidecar.
FROM alpine:${ALPINE_VERSION}

# Argo CD's CMP sidecar contract requires the sidecar to run
# as the same UID/GID as argocd-repo-server (999/999 upstream)
# so file ownership on the shared `/var/run/argocd` and
# `/home/argocd/cmp-server/plugins` volumes lines up across
# containers. Alpine 3.20's base image already occupies GID 999
# with the `ping` group, so we delete that first; `ping` is
# not used inside the sidecar (no network diagnostics in this
# image's scope).
RUN delgroup ping \
 && addgroup -S -g 999 argocd \
 && adduser -S -u 999 -G argocd -h /home/argocd argocd

# `ca-certificates` — CUE imports that resolve to HTTPS module
# registries (CUE registries land in M2+; harmless today,
# forward-compat).
# `bash` — entrypoint.sh shebang.
# `jq` — walk-fix #5 post-B.1.79a (cue-cmp v0.1.2):
# entrypoint.sh emits one k8s manifest per top-level value
# from `cue export --out json`; needs jq for enumerating keys
# and round-tripping each value individually back through `cue
# export -e <key> --out yaml` so Argo CD receives a clean
# YAML stream of unwrapped manifests instead of one nested
# JSON object.
RUN apk add --no-cache ca-certificates bash jq

# Drop the CUE binary on PATH.
COPY --from=fetcher /tmp/cue /usr/local/bin/cue

# CMP plugin manifest. Argo CD's repo-server reads this from
# `/home/argocd/cmp-server/config/plugin.yaml` per the upstream
# CMP convention.
RUN mkdir -p /home/argocd/cmp-server/config \
 && chown -R argocd:argocd /home/argocd
COPY --chown=argocd:argocd plugin.yaml /home/argocd/cmp-server/config/plugin.yaml

# Entrypoint wrapper called by the plugin's `generate` step.
COPY --chown=argocd:argocd entrypoint.sh /usr/local/bin/entrypoint.sh
RUN chmod +x /usr/local/bin/entrypoint.sh

# OCI metadata. The version label is set by CI from the
# `VERSION` file via `--build-arg IMAGE_VERSION=…`; the
# Dockerfile default `dev` distinguishes local builds.
ARG IMAGE_VERSION=dev
ARG IMAGE_REVISION=unknown
LABEL org.opencontainers.image.title="argocd-cue-cmp"
LABEL org.opencontainers.image.description="CUE compilation plugin for Argo CD (ADR 0029)"
LABEL org.opencontainers.image.source="https://github.com/apprafter/apprafter/tree/main/argocd-cue-cmp"
LABEL org.opencontainers.image.url="https://apprafter.io"
LABEL org.opencontainers.image.licenses="FSL-1.1-Apache-2.0"
LABEL org.opencontainers.image.version="${IMAGE_VERSION}"
LABEL org.opencontainers.image.revision="${IMAGE_REVISION}"

USER 999:999
WORKDIR /home/argocd

# The argocd-repo-server image launches our sidecar via
# `argocd-cmp-server` (provided by the upstream Argo CD
# image, not by us). Setting a default ENTRYPOINT here is
# unnecessary — the sidecar's `command:` in the
# extraContainers spec overrides anyway. We leave it as
# `sh` so the image is still useful for ad-hoc inspection:
# `docker run --rm -it ghcr.io/apprafter/argocd-cue-cmp sh`.
ENTRYPOINT ["/bin/sh"]
