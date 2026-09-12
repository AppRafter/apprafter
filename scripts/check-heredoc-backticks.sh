#!/usr/bin/env bash
# SPDX-License-Identifier: FSL-1.1-Apache-2.0
#
# check-heredoc-backticks.sh — fail on a backtick inside an UNQUOTED heredoc.
#
# ## Why
#
# A walk builds its fixtures from heredocs that must expand `${VAR}`, so the
# delimiter is deliberately unquoted (`<<YAML`, not `<<'YAML'`). The shell then
# also performs COMMAND SUBSTITUTION inside the body — including on backticks
# sitting in what looks like an inert YAML comment. Prose written in markdown
# habit (`` `report` is the default ``) is executed, and its output replaces the
# text.
#
# Found in `needs-jetstream-walk.sh` (2.5): twelve `command not found` lines per
# run, from words like `report`, `delete`, `small` and `workqueue` in comments.
# Harmless there only because the mangled text WAS a comment. The same
# substitution in a field value would silently strip it, and the walk would then
# assert against a fixture that is not the one it appears to declare — the
# "an assertion that cannot fail" class, arriving through the fixture instead of
# through the assertion. `bash -n` does not see it; the walk stays green.
#
# Only backticks are checked, and only unescaped ones. `$(…)` inside an unquoted
# heredoc is frequently deliberate (a walk interpolating a computed value), and
# `\`` is already correct. Backtick substitution in a config heredoc never is:
# `$(…)` exists, reads better, and nests.
#
# Fix: replace the backticks with plain quotes, or quote the delimiter if the
# body needs no expansion.

set -euo pipefail

cd "$(dirname "$0")/.."

mapfile -t files < <(git ls-files 'e2e/*.sh' 'scripts/*.sh')

fail=0
for f in "${files[@]}"; do
    # Track the open heredoc tag; only UNQUOTED openers arm the check.
    awk -v file="$f" '
        tag == "" {
            # An unquoted opener: <<TAG or <<-TAG at end of line. A quoted one
            # (<<"TAG" / <<'"'"'TAG'"'"') is inert and deliberately not matched.
            if (match($0, /<<-?[A-Za-z_][A-Za-z0-9_]*[ \t]*$/)) {
                t = substr($0, RSTART, RLENGTH)
                sub(/^<<-?/, "", t)
                gsub(/[ \t]+$/, "", t)
                tag = t
            }
            next
        }
        {
            body = $0
            gsub(/[ \t]+$/, "", body)
            if (body == tag) { tag = ""; next }
            # Strip escaped backticks before looking for live ones.
            probe = $0
            gsub(/\\`/, "", probe)
            if (index(probe, "`") > 0) {
                printf "%s:%d: backtick inside unquoted heredoc <<%s: %s\n", file, NR, tag, $0
                bad = 1
            }
        }
        END { exit bad ? 1 : 0 }
    ' "$f" || fail=1
done

if [ "$fail" -ne 0 ]; then
    cat >&2 <<'MSG'

A backtick inside an unquoted heredoc is executed by the shell, and its output
replaces the text. In a comment that is noise; in a field value it silently
removes content and the fixture stops being the one it appears to declare.

Replace the backticks with plain quotes, or quote the heredoc delimiter
(<<'TAG') if the body needs no ${VAR} expansion.
MSG
    exit 1
fi

printf 'no backticks inside unquoted heredocs in %d shell file(s)\n' "${#files[@]}"
