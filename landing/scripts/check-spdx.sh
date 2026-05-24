#!/usr/bin/env bash
# SPDX-FileCopyrightText: 2026 AppRafter contributors
# SPDX-License-Identifier: FSL-1.1-Apache-2.0
#
# Local SPDX-header check restricted to landing/. The repo-wide check
# (scripts/check-spdx-headers.sh) does not include landing/** patterns
# yet (per ADR 0032 layout); this helper enforces the same rule for
# landing-only sources so the convention stays consistent.

set -euo pipefail

LANDING_ROOT="$(cd "$(dirname "$0")/.." && pwd)"
REPO_ROOT="$(cd "$LANDING_ROOT/.." && pwd)"

cd "$REPO_ROOT"

PATTERNS=(
  'landing/web/src/**/*.ts'
  'landing/web/src/**/*.svelte'
  'landing/web/src/**/*.astro'
  'landing/web/src/**/*.css'
  'landing/web/astro.config.ts'
  'landing/web/svelte.config.js'
  'landing/web/apprafter/*.cue'
  'landing/cms/src/**/*.ts'
  'landing/cms/next.config.mjs'
  'landing/cms/apprafter/*.cue'
  'landing/scripts/*.sh'
)

# git ls-files honours .gitignore and only returns tracked files.
mapfile -t files < <(git ls-files -- "${PATTERNS[@]}" 2>/dev/null | sort -u)

if [[ ${#files[@]} -eq 0 ]]; then
  echo "no landing source files matched the SPDX patterns yet — nothing to check"
  exit 0
fi

failed=0
for f in "${files[@]}"; do
  if ! head -5 "$f" | grep -q 'SPDX-License-Identifier:'; then
    echo "::error file=$f::missing SPDX-License-Identifier in first 5 lines"
    failed=1
  fi
done

if [[ $failed -ne 0 ]]; then
  exit 1
fi

echo "all ${#files[@]} landing source files declare SPDX-License-Identifier"
