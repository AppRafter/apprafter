# SPDX-License-Identifier: MIT
# Multi-stage build for the AppRafter bun-http-starter.
#
# Builder uses oven/bun:1-debian for npm-compatible install + the
# Bun bundler; runtime is the distroless nodejs image (Bun bundles
# to a self-contained CommonJS file the Node runtime can execute,
# avoiding the need to ship Bun in the runtime layer).

FROM oven/bun:1-debian AS builder

WORKDIR /workspace

# Lockfile + manifest first → layer-cached deps.
COPY package.json bun.lock ./
RUN bun install --frozen-lockfile --production

# Then sources.
COPY tsconfig.json ./
COPY src src

# Bundle to a self-contained Node-compatible file. Distroless
# nodejs runs the bundle without needing Bun at runtime.
RUN bun build src/index.ts \
        --target node \
        --outfile dist/index.cjs \
        --format cjs \
        --minify

FROM gcr.io/distroless/nodejs20-debian12:nonroot

COPY --from=builder /workspace/dist/index.cjs /app/index.cjs

USER nonroot
EXPOSE 3000
ENV PORT=3000

CMD ["/app/index.cjs"]
