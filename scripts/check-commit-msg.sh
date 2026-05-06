#!/usr/bin/env bash
# SPDX-License-Identifier: FSL-1.1-MIT
#
# Validate that a commit message follows Conventional Commits:
#     <type>(<scope>)?!?: <description>
#
# The same set of allowed types is enforced in
# .github/workflows/conventional-commits.yml so that local hooks and
# CI agree.

set -euo pipefail

MSG_FILE="${1:-.git/COMMIT_EDITMSG}"

if [[ ! -f "$MSG_FILE" ]]; then
  echo "ERROR: commit message file not found: $MSG_FILE" >&2
  exit 2
fi

FIRST_LINE="$(head -1 "$MSG_FILE")"

# Allow merge / revert / fixup / squash commits unconditionally.
case "$FIRST_LINE" in
  Merge*|Revert*|fixup!*|squash!*) exit 0 ;;
esac

PATTERN='^(chore|ci|docs|feat|fix|perf|refactor|revert|style|test)(\([a-z0-9._-]+\))?!?: .{1,72}$'

if [[ ! "$FIRST_LINE" =~ $PATTERN ]]; then
  cat >&2 <<EOF
ERROR: commit message does not follow Conventional Commits.

Got:
  $FIRST_LINE

Expected:
  <type>(<scope>)?!?: <description>

Allowed types:
  chore | ci | docs | feat | fix | perf | refactor | revert | style | test

Subject must be 1–72 characters.
EOF
  exit 1
fi
