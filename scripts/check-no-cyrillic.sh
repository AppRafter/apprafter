#!/usr/bin/env bash
# SPDX-License-Identifier: FSL-1.1-Apache-2.0
#
# Fail if Cyrillic characters appear in code or public-facing docs.
#
# The AppRafter codebase is English-only for everything that ships or is
# read by outsiders. Cyrillic there is contamination — homoglyph letters
# (Cyrillic a/e/c masquerading as Latin) or stray Russian words leaking
# into comments, doc-comments, scaffold templates, CI, or — worst —
# user-facing strings, where they render as garbage to operators.
#
# Scope (allowlist, so intentional Russian never trips a false positive):
#   * Code:        *.rs *.cue *.hbs *.sh *.ts/.tsx *.js/.jsx *.toml
#                  *.yml/.yaml, Dockerfile*
#   * Public docs: README* / CONTRIBUTING* / CODE_OF_CONDUCT* (.md),
#                  spec.md, docs/**/*.md
#
# NOT scanned (Russian is allowed): root-level working markdown notes
# (plan.md, speedrun-plan.md, *_CHECKLIST.md, refactor-plan.md,
# marketing-strategy.md, idiomatic-rust-audit.md, ...) and other internal
# working .md files. Public-named docs (README/CONTRIBUTING/...) are NOT
# working notes and ARE scanned, wherever they live.
#
# Engine is python3 (reliable Unicode matching across environments);
# CI runners and the dev shell both provide it.
set -euo pipefail

cd "$(git rev-parse --show-toplevel)"

if ! command -v python3 >/dev/null 2>&1; then
    echo "ERROR: python3 is required for scripts/check-no-cyrillic.sh." >&2
    exit 2
fi

python3 - <<'PYEOF'
import re
import subprocess
import sys

# Cyrillic + Cyrillic Supplement blocks (escapes, so this script is
# itself Cyrillic-free and passes its own check).
CYRILLIC = re.compile("[\\u0400-\\u04FF\\u0500-\\u052F]")

CODE_SUFFIXES = (
    ".rs", ".cue", ".hbs", ".sh", ".ts", ".tsx", ".js", ".jsx",
    ".toml", ".yml", ".yaml",
)
PUBLIC_DOC_PREFIXES = ("README", "CONTRIBUTING", "CODE_OF_CONDUCT")


def is_checked(path):
    base = path.rsplit("/", 1)[-1]
    if path.endswith(CODE_SUFFIXES):
        return True
    if base.startswith("Dockerfile"):
        return True
    if base.endswith(".md") and base.startswith(PUBLIC_DOC_PREFIXES):
        return True
    if path == "spec.md":
        return True
    if path.startswith("docs/") and path.endswith(".md"):
        return True
    return False


tracked = subprocess.run(
    ["git", "ls-files"],
    capture_output=True, text=True, check=True,
).stdout.split()
files = [p for p in tracked if is_checked(p)]

fail = False
for path in files:
    try:
        with open(path, encoding="utf-8") as fh:
            for lineno, line in enumerate(fh, 1):
                if CYRILLIC.search(line):
                    fail = True
                    print(f"{path}:{lineno}: {line.rstrip()}", file=sys.stderr)
    except (OSError, UnicodeDecodeError):
        # Binary or unreadable file under a scanned suffix — skip quietly.
        continue

if fail:
    print(
        "\nERROR: Cyrillic found in code / public docs. These must be "
        "English-only.\nReplace each with the intended ASCII English. "
        "(Russian is allowed in root working .md notes only.)",
        file=sys.stderr,
    )
    sys.exit(1)

print(f"==> no Cyrillic in {len(files)} scanned code/public-doc files")
PYEOF
