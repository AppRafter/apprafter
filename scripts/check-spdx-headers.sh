#!/usr/bin/env bash
# SPDX-License-Identifier: FSL-1.1-Apache-2.0
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
  # platform-stack (CUE source + Helm Chart template)
  'platform-stack/cue/**/*.cue'
  'platform-stack/Chart.yaml.tmpl'
  # argocd-cue-cmp (Argo CD CMP sidecar image source — ADR 0029)
  'argocd-cue-cmp/Dockerfile'
  'argocd-cue-cmp/plugin.yaml'
  'argocd-cue-cmp/entrypoint.sh'
  # Scripts
  'scripts/*.sh'
  # Python gates and readers under scripts/ are source too. Both files that
  # match today already carry the header; naming the pattern is what stops
  # the next one from being added without it.
  'scripts/*.py'
  # The Argo CD resource-health fixtures (`check-argocd-health-lua.sh`).
  # Same reasoning as the `.py` line above: the pattern is what stops the
  # next one from arriving without a header.
  'scripts/*.lua'
  'e2e/*.sh'
  '.devcontainer/*.sh'
  # Source code
  'cli/**/*.rs'
  'operator/**/*.rs'
  'providers/**/*.rs'
  'backstage-plugins/**/*.ts'
  'backstage-plugins/**/*.tsx'
  # AppRafter Desktop (ADR 0067) — core licence. ONE `*`, on purpose: in a git pathspec
  # without :(glob) magic `*` crosses `/`, so `desktop/*.ts` covers desktop/vite.config.ts AND
  # desktop/src/**, while `desktop/**/*.ts` would skip every top-level file. Generated ts-rs
  # files under desktop/src/ipc/generated/ carry the header (the export prepends it).
  # JSON (package.json, tsconfig.json, biome.json) cannot carry one; json5 can. The SVG
  # icon source is hand-written and carries one; the rasters generated from it cannot.
  'desktop/*.rs'
  'desktop/*.ts'
  'desktop/*.tsx'
  'desktop/*.js'
  'desktop/*.mjs'
  'desktop/*.css'
  'desktop/*.html'
  'desktop/*.toml'
  'desktop/*.json5'
  'desktop/*.svg'
  # The Windows application manifest: an XML comment after the declaration.
  'desktop/*.xml'
  # polkit action files (desktop/packaging/linux/): XML too, and the XML declaration must be
  # line 1, so the SPDX comment is line 2.
  'desktop/*.policy'
  # The committed Claude Design export is upstream's file, kept verbatim.
  ':(exclude)desktop/design-source/*'
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
  # MkDocs build hooks. These are source, not documentation: they live
  # under docs/ to sit beside what they act on, and mkdocs.yml excludes
  # them from the built site. The "no SPDX on docs/" note above is
  # about markdown pages, so name them explicitly rather than widening
  # the exclusion's meaning.
  'docs/hooks/*.py'
  # A single `*` crosses `/` (no :(glob) magic), so these match cli/Cargo.toml and
  # cli/rust-toolchain.toml as well as every crate's; `cli/**/…` needs a directory level.
  'cli/*Cargo.toml'
  'cli/*rust-toolchain.toml'
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
