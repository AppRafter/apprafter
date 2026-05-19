#!/usr/bin/env bash
# SPDX-License-Identifier: FSL-1.1-Apache-2.0
#
# Verify that `platform-stack/cue/compatibility.cue` has an
# entry for a given chart version. Used both:
#
#   - by the `platform-stack-publish` workflow as a fail-fast
#     gate before any `helm push` / OCI write happens, and
#   - locally as the maintainer's pre-tag sanity check.
#
# Usage:
#
#   # Explicit version:
#   bash scripts/check-platform-stack-version.sh 0.1.0
#   bash scripts/check-platform-stack-version.sh 0.1.0-rc1
#
#   # Auto: read `currentVersion` from
#   # `platform-stack/cue/platform.cue` (the canonical
#   # source-of-truth declared during sub-phase 1.68's
#   # version-policy refactor).
#   bash scripts/check-platform-stack-version.sh
#
# Exit 0 = entry exists and is well-formed. Exit non-zero with
# a human-readable error pointing at the file to edit.

set -euo pipefail

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

if [[ $# -gt 1 ]]; then
  echo "usage: $0 [<version>]" >&2
  echo "  if version omitted, reads currentVersion from platform.cue" >&2
  exit 2
fi

if [[ $# -eq 1 ]]; then
  VERSION="$1"
else
  # Auto-resolve from the canonical source-of-truth.
  VERSION="$("${CUE_CMD[@]}" export ./platform-stack/cue/... \
              -e currentVersion --out text 2>&1)" || {
    echo "ERROR: failed to read currentVersion from platform-stack/cue/platform.cue" >&2
    echo "$VERSION" >&2
    exit 1
  }
  if [[ -z "${VERSION:-}" ]]; then
    echo "ERROR: currentVersion in platform-stack/cue/platform.cue is empty" >&2
    exit 1
  fi
  echo "Auto-resolved currentVersion from platform.cue: ${VERSION}"
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

(Tip: the CUE-level invariant \`compatibility: (currentVersion):
#VersionRecord\` in compatibility.cue catches this at edit time
via \`cue vet -c ./platform-stack/cue/...\` — run that locally
to fail-fast before committing.)
EOF
  exit 1
fi

echo "compatibility.cue entry for ${VERSION}:"
echo "${out}"
