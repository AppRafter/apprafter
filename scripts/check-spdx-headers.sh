#!/usr/bin/env bash
# SPDX-License-Identifier: FSL-1.1-MIT
#
# Verify that every tracked source file under known code paths
# declares an SPDX-License-Identifier in its first 5 lines (to allow
# room for shebang + blank + comment header).
#
# The patterns intentionally exclude:
#   - Markdown / docs (no SPDX requirement).
#   - Generated files (lockfiles, target/).
#   - Top-level meta files (LICENSE, NOTICE).
#
# Extend `PATTERNS` as new languages come online.

set -euo pipefail

REPO_ROOT="$(git rev-parse --show-toplevel)"
cd "$REPO_ROOT"

PATTERNS=(
  # Schemas + examples (CUE)
  'cue.mod/module.cue'
  'schemas/**/*.cue'
  'examples/**/*.cue'
  # Scripts
  'scripts/*.sh'
  'e2e/*.sh'
  '.devcontainer/*.sh'
  # Source code
  'cli/**/*.rs'
  'operator/**/*.rs'
  'providers/**/*.rs'
  'backstage-plugins/**/*.ts'
  'backstage-plugins/**/*.tsx'
  # Platform manifests
  'manifests/**/*.yaml'
  'manifests/**/*.yml'
  # CI / GitHub meta
  '.github/workflows/*.yml'
  '.github/workflows/*.yaml'
  '.github/ISSUE_TEMPLATE/*.yml'
  '.github/ISSUE_TEMPLATE/*.yaml'
  '.github/CODEOWNERS'
  '.github/PULL_REQUEST_TEMPLATE.md'
  # Repo tooling
  'lefthook.yml'
  'lefthook.yaml'
  'flake.nix'
  'mise.toml'
  'Justfile'
  'mkdocs.yml'
  'cli/**/Cargo.toml'
  'cli/**/rust-toolchain.toml'
)

# Collect tracked files matching any pattern. `git ls-files` honours
# .gitignore and only returns files we actually track.
mapfile -t files < <(git ls-files -- "${PATTERNS[@]}" 2>/dev/null | sort -u)

if [[ ${#files[@]} -eq 0 ]]; then
  echo "no source files matched the SPDX patterns yet — nothing to check"
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

echo "all ${#files[@]} tracked source files declare SPDX-License-Identifier"
