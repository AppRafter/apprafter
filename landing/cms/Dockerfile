# SPDX-License-Identifier: FSL-1.1-Apache-2.0
#
# landing-cms image — Payload 3 + Next 15 in standalone output
# mode. Runtime is Node 22 Alpine; Sharp's musl native binary
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

# Next standalone output writes to cms/.next/standalone with the
# workspace root as the tracing base (set in next.config.mjs).
RUN cd cms && bun run build

# -----------------------------------------------------------------
# Stage 2: minimal Node runtime
# -----------------------------------------------------------------
FROM node:22-alpine AS runtime
WORKDIR /app

ENV NODE_ENV=production
# Default port — overridden by LANDING_CMS_PORT at runtime when set.
ENV PORT=3000
ENV HOSTNAME=0.0.0.0

# Sharp's musl runtime dependency.
RUN apk add --no-cache libc6-compat

# Next standalone with outputFileTracingRoot=landing/ emits the
# server tree under landing/cms/. Flatten into /app so the
# Dockerfile doesn't need to know the depth.
COPY --from=builder /landing/cms/.next/standalone/cms /app
COPY --from=builder /landing/cms/.next/standalone/node_modules /app/node_modules
COPY --from=builder /landing/cms/.next/static /app/.next/static
COPY --from=builder /landing/cms/public /app/public

EXPOSE 3000

HEALTHCHECK --interval=30s --timeout=5s --start-period=20s --retries=3 \
  CMD wget -q -O /dev/null http://localhost:${PORT}/admin || exit 1

CMD ["node", "server.js"]
