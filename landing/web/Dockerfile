# SPDX-License-Identifier: FSL-1.1-Apache-2.0
#
# landing-web image — Astro SSG output served by Caddy.
#
# Build context: the workspace root `landing/` (NOT this directory),
# so Bun's workspace install can see both child package.json files
# and the shared lockfile.
#
#   docker build -f landing/web/Dockerfile -t landing-web landing/
#
# The build is reproducible without a running Payload:
# LANDING_USE_FALLBACK=1 makes lib/cms.ts source content from the
# JSONs committed under web/src/data/fallback/ rather than fetching
# the live CMS. Operators who want CMS-driven static builds run a
# second pass inside the cluster after content edits — see
# landing/DEPLOY.md.

# -----------------------------------------------------------------
# Stage 1: build the static Astro output
# -----------------------------------------------------------------
FROM oven/bun:1-alpine AS builder
WORKDIR /landing

# Workspace lockfile + manifest first — keeps the install layer
# cached when only source files change.
COPY package.json bun.lock tsconfig.base.json ./
COPY web/package.json web/package.json
COPY cms/package.json cms/package.json

# All Bun workspace deps are hoisted into /landing/node_modules.
RUN bun install --frozen-lockfile

# Bring in the web sources only — cms/ is not needed for this image.
COPY web web

# Defaults to 1 → lib/cms.ts reads JSON fallbacks during SSG, no
# CMS connection required. The release workflow takes this path.
# The rebuild workflow overrides to 0 so the build fetches live
# content from LANDING_CMS_URL — that workflow then re-publishes
# `landing-web:edge` after each admin edit.
ARG LANDING_USE_FALLBACK=1
ENV LANDING_USE_FALLBACK=${LANDING_USE_FALLBACK}

# When LANDING_USE_FALLBACK=0 the build needs a reachable Payload.
# Default points at the public CMS host so the rebuild workflow
# Just Works; the release workflow ignores this since fallback is on.
ARG LANDING_CMS_URL=https://cms.apprafter.dev
ENV LANDING_CMS_URL=${LANDING_CMS_URL}

# PUBLIC_CMS_URL is what the browser fetches at runtime (waitlist
# form). Default points at the public deploy; override via
# `--build-arg PUBLIC_CMS_URL=...`.
ARG PUBLIC_CMS_URL=https://cms.apprafter.dev
ENV PUBLIC_CMS_URL=${PUBLIC_CMS_URL}

RUN cd web && bun run build

# -----------------------------------------------------------------
# Stage 2: minimal Caddy runtime
# -----------------------------------------------------------------
FROM caddy:2-alpine AS runtime

COPY --from=builder /landing/web/dist /usr/share/caddy
COPY web/Caddyfile /etc/caddy/Caddyfile

EXPOSE 80

# Healthcheck: hit / and expect HTTP 200.
HEALTHCHECK --interval=30s --timeout=3s --start-period=5s --retries=3 \
  CMD wget -q -O /dev/null http://localhost/ || exit 1
