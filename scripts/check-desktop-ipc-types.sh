#!/usr/bin/env bash
# SPDX-License-Identifier: FSL-1.1-Apache-2.0
#
# check-desktop-ipc-types.sh — fail if `desktop/src/ipc/generated/` differs from
# what the desktop/ipc export writes, so the committed TypeScript IPC types cannot
# lag the Rust types and constants they are generated from.
#
# ## Why
#
# The webview imports these files; the Rust shell serialises the types they
# describe. A stale declaration still COMPILES: the frontend type-checks against
# a shape the shell no longer sends, and the mismatch surfaces only at run time,
# as an `undefined` where a field used to be. Nothing else compares the two.
#
# ## What it does
#
# Deletes the directory, runs the export (`desktop/ipc/tests/export.rs`, the
# same test `just desktop-ipc-types` runs), and compares the result with the
# git INDEX: `git diff` for changed or deleted files, `git ls-files --others`
# for new ones. The delete makes the export prove it wrote THIS directory; a
# moved output would otherwise compare the committed files with themselves.
#
# Unlike scripts/check-payload-types.sh this does not restore a backup: the
# export test rewrites the directory in place on every `cargo test
# --all-features` anyway, so the committed bytes live in the index, not in the
# working tree. On drift the working tree is left holding the fresh export,
# which is exactly what `just desktop-ipc-types` would write.
#
# ## Environment
#
# `apprafter-desktop-ipc` is Tauri-free, so this builds no WebKitGTK: run from
# a plain shell (no `nix develop .#desktop`) it compiles only the ipc crate and
# the apprafter-core closure. That closure's cli-providers/build.rs runs the
# pinned cue, so export CUE_BIN when the PATH cue is not v0.17.1. CI runs it in
# test.yml's rust-desktop job, Linux leg only: the output is the same on every
# OS (ts-rs writes `/` in imports, .gitattributes keeps LF).
#
# There is no pre-commit hook for this, for the reasons check-payload-types.sh
# gives: a hook sees the working tree, a commit is the index.

set -euo pipefail

REPO_ROOT="$(git rev-parse --show-toplevel)"
cd "$REPO_ROOT"

# `git rev-parse` resolves from the CWD, so it can land in another repository.
if [ ! -f Justfile ] || [ ! -f cue.mod/module.cue ] || [ ! -f desktop/ipc/Cargo.toml ]; then
    echo "ERROR: $REPO_ROOT is not the apprafter repo root (no desktop/ipc here)." >&2
    exit 1
fi

GEN="desktop/src/ipc/generated"

rm -rf "$GEN"

if ! (cd desktop && cargo test --locked -p apprafter-desktop-ipc --features ts --test export); then
    echo "ERROR: the TypeScript export (desktop/ipc/tests/export.rs) failed." >&2
    exit 1
fi

if [ ! -d "$GEN" ]; then
    echo "ERROR: the export ran but did not write $GEN." >&2
    echo "       Moving the output takes three edits: the test's destination," >&2
    echo "       GEN= here, and the ignore entry in desktop/biome.json." >&2
    exit 1
fi

# Every file the export wrote must be tracked, ignored or not.
untracked="$(git ls-files --others -- "$GEN")"

if git diff --quiet -- "$GEN" && [ -z "$untracked" ]; then
    echo "OK: $GEN matches the Rust types it is generated from."
    exit 0
fi

echo "ERROR: $GEN is stale — it does not match what desktop/ipc exports." >&2
echo >&2
git --no-pager diff --stat -- "$GEN" >&2
git --no-pager diff -- "$GEN" >&2
if [ -n "$untracked" ]; then
    echo "New files, not in the index:" >&2
    printf '%s\n' "$untracked" | sed 's/^/  /' >&2
fi
echo >&2
echo "       Fix: just desktop-ipc-types    (then commit $GEN)" >&2
exit 1
