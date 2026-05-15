#!/usr/bin/env bash
# SPDX-License-Identifier: FSL-1.1-MIT
#
# Verify that `platform-stack/cue/compatibility.cue` has an
# entry for the version about to be published. Wired into the
# `platform-stack-publish` workflow (sub-phase 1.68) as a
# fail-fast gate before any `helm push` / OCI write happens.
#
# Usage:
#
#   bash scripts/check-platform-stack-version.sh 0.2.0
#   bash scripts/check-platform-stack-version.sh 0.2.0-rc1
#
# Exit 0 = entry exists and is well-formed. Exit non-zero with
# a human-readable error pointing at the file to edit.

set -euo pipefail

if [[ $# -ne 1 ]]; then
  echo "usage: $0 <version>" >&2
  echo "  example: $0 0.2.0" >&2
  exit 2
fi

VERSION="$1"
REPO_ROOT="$(git rev-parse --show-toplevel)"
cd "$REPO_ROOT"

# Resolve the CUE binary the same way scripts/lint-cue.sh does:
# prefer local install, fall back to `nix run nixpkgs#cue --`.
if command -v cue >/dev/null 2>&1; then
  CUE_CMD=(cue)
elif command -v nix >/dev/null 2>&1; then
  CUE_CMD=(nix run nixpkgs#cue --)
else
  echo "ERROR: cue is not installed and nix is unavailable." >&2
  exit 2
fi

# `cue export -e compatibility[<version>]` exits non-zero when
# the path doesn't resolve to a concrete value. We capture both
# stdout (for the success case — surfaced into CI logs as a
# sanity check) and stderr (for the failure-case diagnostic).
if ! out=$("${CUE_CMD[@]}" export ./platform-stack/cue/... \
            -e "compatibility[\"${VERSION}\"]" \
            --out yaml 2>&1); then
  cat >&2 <<EOF
ERROR: platform-stack/cue/compatibility.cue has no entry for version "${VERSION}".

cue export said:
${out}

Open platform-stack/cue/compatibility.cue and add an entry:

    compatibility: "${VERSION}": {
        change:          "safe" | "caution" | "breaking"
        operatorVersion: "vX.Y.Z"
        notes:           "..."
        references:      [...]
    }

Then re-tag and re-run the workflow. The compatibility entry is
the contract \`PlatformController\` consumes to gate automated
upgrades — missing entries WILL surface as a stuck reconcile in
production.
EOF
  exit 1
fi

echo "compatibility.cue entry for ${VERSION}:"
echo "${out}"
