#!/usr/bin/env bash
# SPDX-License-Identifier: MIT
#
# Scaffold a Backstage app under $TARGET (default ./host-app) and
# drop the AppRafter Dockerfile + .dockerignore alongside it.
#
# Usage:
#   ./scripts/scaffold.sh [TARGET]
#
# Requires Node 20+ and a network connection to npm. Picks
# `@backstage/create-app@latest` via npx; we don't pin a version
# here on purpose — the chart pins are in the AppRafter Rust
# crates, not in this wrapper.

set -euo pipefail

# ---- args + env ------------------------------------------------
TARGET="${1:-host-app}"
SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd -P)"
HOST_DIR="$(cd -- "$SCRIPT_DIR/.." && pwd -P)"

# ---- preflight -------------------------------------------------
if ! command -v node >/dev/null 2>&1; then
    echo "scaffold: node not found on PATH; install Node 20+ first" >&2
    exit 1
fi

NODE_MAJOR="$(node --version | sed -E 's/^v([0-9]+).*/\1/')"
if [ "$NODE_MAJOR" -lt 20 ]; then
    echo "scaffold: node $NODE_MAJOR is too old; Backstage 1.x needs Node 20+" >&2
    exit 1
fi

if ! command -v npx >/dev/null 2>&1; then
    echo "scaffold: npx not found on PATH; install npm or yarn alongside node" >&2
    exit 1
fi

if [ -e "$TARGET" ] && [ "$(ls -A "$TARGET" 2>/dev/null || true)" ]; then
    echo "scaffold: $TARGET exists and is non-empty; refusing to overwrite" >&2
    echo "scaffold: rerun with a different TARGET or delete $TARGET first" >&2
    exit 1
fi

# ---- scaffold --------------------------------------------------
echo "scaffold: running @backstage/create-app@latest into $TARGET …"
npx --yes @backstage/create-app@latest \
    --path "$TARGET" \
    --skip-install

# ---- drop the Dockerfile + .dockerignore -----------------------
cp -- "$HOST_DIR/Dockerfile" "$TARGET/Dockerfile"
cp -- "$HOST_DIR/.dockerignore" "$TARGET/.dockerignore"

# ---- next steps ------------------------------------------------
cat <<NEXT
scaffold: done. Next:

  1) Pick your package manager and install:
       cd $TARGET && yarn install         # or: npm install

  2) Build the image:
       docker build -t ghcr.io/<org>/backstage:0.1.0 .

  3) Push to your registry:
       docker push ghcr.io/<org>/backstage:0.1.0

  4) Plug it into your Infrastructure manifest:
       spec.backstage.image: "ghcr.io/<org>/backstage:0.1.0"

  5) Run \`apprafter cluster-bootstrap\` against your cluster.
NEXT
