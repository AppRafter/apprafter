# SPDX-License-Identifier: FSL-1.1-Apache-2.0
#
# landing-cms image — Payload 3 + Next 15 in standalone output
# mode. Runtime is Node 24 Alpine; Sharp's musl native binary
# ships with @img/sharp-libvips-linuxmusl-x64.
#
# Build context: the workspace root `landing/` (same as
# landing-web).
#
#   docker build -f landing/cms/Dockerfile -t landing-cms landing/
#
# DATABASE_URI + PAYLOAD_SECRET + SMTP_* are runtime-only — the
# build doesn't need them. payload.config.ts reads them lazily
# when getPayload() is called from an API route.

# -----------------------------------------------------------------
# Stage 1: build the Next.js + Payload bundle
# -----------------------------------------------------------------
FROM oven/bun:1-alpine AS builder
WORKDIR /landing

# Build deps for Sharp's prebuilt musl binaries.
RUN apk add --no-cache libc6-compat

COPY package.json bun.lock tsconfig.base.json ./
COPY web/package.json web/package.json
COPY cms/package.json cms/package.json

RUN bun install --frozen-lockfile

COPY cms cms

# The CMS bundles the shared phase registry at build time
# (waitlistPhaseOptions.ts and the Roadmap `num` validate import it
# across the web/ workspace). Only web/package.json is copied above, so
# the registry file itself must be present for the Next build to
# resolve + inline it — it stays the single source
# (landing/web/src/data/phases.json); nothing is kept at runtime.
COPY web/src/data/phases.json web/src/data/phases.json

# Next standalone output writes to cms/.next/standalone with the
# workspace root as the tracing base (set in next.config.mjs).
RUN cd cms && bun run build

# Bundle the content-seed script into one self-contained ESM file the
# runtime image runs with plain node (no bun, no payload CLI). ESM
# (not CJS) because Payload's lexical richtext uses top-level await.
# Native deps (sharp, pg) stay external — they resolve from the
# standalone node_modules at runtime. NODE_ENV=production is set so
# bun inlines it into the bundle (it inlines process.env.NODE_ENV at
# build time); otherwise the Payload adapter takes the dev branch and
# tries to `pushDevSchema` via drizzle-kit, which is absent at runtime.
# The post-deploy Job runs this once as `node cms/seed.mjs` (SEED_IF_EMPTY=1).
RUN cd cms && NODE_ENV=production bun build src/seed/seed.ts --target=node --format=esm \
      --external sharp --external pg --outfile /landing/seed.mjs

# Next's file-tracer (nft) does not reliably copy a handful of
# Payload's runtime deps into the standalone tree: they are required
# only from lazily-loaded Payload code paths (upload SSRF guard →
# undici, realtime → ws, schema validation → ajv) that nft cannot
# follow through Bun's symlinked `.bun` store. They survive a glibc
# build but are dropped on this Alpine/musl one, 500ing the admin
# with MODULE_NOT_FOUND. Stage each one — plus its dependency
# closure — as a fully-dereferenced (cp -L), self-contained tree
# straight from Bun's store; the runtime stage drops them onto the
# admin bundle's module-resolution path. Sourcing from the store by
# name keeps this version-agnostic and covers transitive-only deps
# (ws, ajv are not direct dependencies). `@builder.io/partytown` is
# intentionally skipped — it is an optional Next dep, never installed
# nor executed by the admin.
RUN set -eu; mkdir -p /landing/runtime-deps; \
    for p in undici ws ajv; do \
      cp -RL /landing/node_modules/.bun/"$p"@*/node_modules/. /landing/runtime-deps/; \
    done; \
    for p in undici ws ajv; do \
      test -d "/landing/runtime-deps/$p" \
        || { echo "FATAL: runtime-dep '$p' missing after staging"; exit 1; }; \
    done

# -----------------------------------------------------------------
# Stage 2: minimal Node runtime
# -----------------------------------------------------------------
FROM node:24-alpine AS runtime
WORKDIR /app

ENV NODE_ENV=production
# Default port — overridden by LANDING_CMS_PORT at runtime when set.
ENV PORT=3000
ENV HOSTNAME=0.0.0.0

# Sharp's musl runtime dependency.
RUN apk add --no-cache libc6-compat

# Next standalone with outputFileTracingRoot=landing/ emits the
# server tree under standalone/cms/ next to a sibling
# standalone/node_modules/ store. Each package symlink inside
# standalone/cms/node_modules points at `../../node_modules/.bun/…`
# (Bun's hoisted store), which only resolves while cms/ and
# node_modules/ remain siblings. So copy the WHOLE standalone tree
# verbatim and launch cms/server.js from the root — do NOT flatten
# cms/ into /app, or those `../../` symlinks overshoot to
# /node_modules and `require('next')` fails at runtime.
COPY --from=builder /landing/cms/.next/standalone /app
# Next standalone deliberately omits static/ and public/. server.js
# does process.chdir(__dirname), so they must sit under the app
# subdir. The public/ directory MUST exist in the source tree;
# landing/cms/public/robots.txt is the floor.
COPY --from=builder /landing/cms/.next/static /app/cms/.next/static
COPY --from=builder /landing/cms/public /app/cms/public
# Payload runtime deps nft drops from the Alpine standalone build
# (see builder stage), staged with their dependency closures so
# `require('undici'|'ws'|'ajv')` from the admin bundle resolves —
# /app/cms/.next/server/... walks up to /app/cms/node_modules. Merges
# alongside the standalone's own next/react/pg/graphql symlinks.
COPY --from=builder /landing/runtime-deps /app/cms/node_modules

# Content-seed artifact (see builder stage): the bundled seed script +
# the fallback JSONs it reads. The post-deploy Job runs it once:
#   node cms/seed.mjs        (SEED_IF_EMPTY=1 → never clobbers edits)
# SEED_FALLBACK_DIR is baked because the bundle can't resolve its own
# runtime path; the CMS server itself ignores this env.
ENV SEED_FALLBACK_DIR=/app/cms/seed-data
COPY --from=builder /landing/seed.mjs /app/cms/seed.mjs
COPY web/src/data/fallback /app/cms/seed-data

EXPOSE 3000

HEALTHCHECK --interval=30s --timeout=5s --start-period=20s --retries=3 \
  CMD wget -q -O /dev/null http://localhost:${PORT}/admin || exit 1

CMD ["node", "cms/server.js"]
