#!/usr/bin/env bash
# SPDX-License-Identifier: FSL-1.1-Apache-2.0
#
# Fail if any tracked Rust source carries Cyrillic characters.
#
# The AppRafter codebase is English-only (see CLAUDE.md repository
# conventions). Cyrillic in source is contamination — homoglyph letters
# (Cyrillic а/е/с masquerading as Latin) or stray Russian/Ukrainian
# function words leaking into comments, doc-comments, or — worse —
# user-facing strings. Such text renders as garbage to operators and
# breaks the English-only norm.
#
# Scope is deliberately limited to CODE (Rust today). Root working docs
# (plan.md, speedrun-plan.md, *_CHECKLIST.md, ...) are allowed Russian
# and are NOT scanned. Extend the `git ls-files` glob as more languages
# come online.
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

# Cyrillic + Cyrillic Supplement blocks.
CYRILLIC = re.compile(r"[Ѐ-ӿԀ-ԯ]")

files = subprocess.run(
    ["git", "ls-files", "*.rs"],
    capture_output=True, text=True, check=True,
).stdout.split()

fail = False
for path in files:
    try:
        with open(path, encoding="utf-8") as fh:
            for lineno, line in enumerate(fh, 1):
                if CYRILLIC.search(line):
                    fail = True
                    print(f"{path}:{lineno}: {line.rstrip()}", file=sys.stderr)
    except (OSError, UnicodeDecodeError) as exc:
        print(f"{path}: cannot read: {exc}", file=sys.stderr)
        fail = True

if fail:
    print(
        "\nERROR: Cyrillic characters found in Rust source. The codebase is "
        "English-only.\nReplace each with the intended ASCII English.",
        file=sys.stderr,
    )
    sys.exit(1)

print("==> no Cyrillic in Rust source")
PYEOF
