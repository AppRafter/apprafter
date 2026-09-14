#!/usr/bin/env bash
# SPDX-License-Identifier: FSL-1.1-Apache-2.0
#
# Regression test for the `discover.find.command` shell
# snippet in `argocd-cue-cmp/plugin.yaml`. Argo CD's CMP
# `MatchRepository` reads STDOUT of the discover command —
# non-empty output means "plugin claims this repo", empty
# output means "fall through to the next plugin / directory
# mode". Walk-fix #10 (chart 0.1.46) exists because the
# v0.1.4 plugin piped `find -print -quit` through `grep -q`,
# which exits 0 on match but prints nothing — Argo CD saw
# empty stdout and never engaged the plugin.
#
# This test extracts the discover snippet from the
# in-tree `plugin.yaml`, runs it against four fixture
# directories that exercise both naming conventions, and
# asserts that stdout matches the expected match/no-match
# signal. Wired into `.github/workflows/argocd-cue-cmp-
# check.yml`; runs locally too via `bash test-discover.sh`.

set -euo pipefail

script_dir=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" >/dev/null 2>&1 && pwd)
plugin_yaml="$script_dir/plugin.yaml"
if [[ ! -f "$plugin_yaml" ]]; then
    echo "FAIL: $plugin_yaml not found" >&2
    exit 1
fi

# Extract the discover command body — it's a multi-line block
# scalar (`|`) starting after `- |` under
# `spec.discover.find.command`. The block is the last entry
# in the `command:` array (after the `- sh` + `- -c` lines).
# Strategy: pull every line between `- |` and the next less-
# indented line (which is `generate:` two spaces shallower).
# That body is what `argocd-repo-server` execs under sh -c.
script_body=$(awk '
    /^[[:space:]]+- \|$/ {
        in_block = 1
        block_indent = match($0, /[^ ]/) - 1
        next
    }
    in_block {
        # Stop at the first non-blank line that is shallower
        # than the block — that is the next YAML key
        # (`generate:` here).
        if ($0 ~ /^[[:space:]]*$/) { print; next }
        cur_indent = match($0, /[^ ]/) - 1
        if (cur_indent <= block_indent) { exit }
        # Strip the leading block_indent + 2 spaces (YAML
        # block scalar indentation rules: the block starts
        # at column block_indent + 2, where 2 = the offset
        # awk-parsed `- ` items use).
        prefix = block_indent + 2
        sub("^" sprintf("%*s", prefix, ""), "")
        print
    }
' "$plugin_yaml")

if [[ -z "$script_body" ]]; then
    echo "FAIL: could not extract discover shell body from $plugin_yaml" >&2
    exit 1
fi

# Hard guard against re-introducing the `grep -q .` regression
# we're testing for — fail loud on any grep -q in the discover
# script, regardless of whether the fixture cases pass. The
# fixture tests below would catch this too, but a string match
# gives a much clearer failure message for the obvious case.
if echo "$script_body" | grep -Fq 'grep -q'; then
    echo "FAIL: discover script contains 'grep -q' — re-introducing the walk-fix #10 regression." >&2
    echo "      Argo CD reads stdout of the discover command; 'grep -q' is silent and breaks plugin matching." >&2
    echo "--- offending script body ---" >&2
    echo "$script_body" >&2
    exit 1
fi

tmp_root=$(mktemp -d)
trap 'rm -rf "$tmp_root"' EXIT

# Fixtures must be REAL manifests: the discover gate confirms intent by
# file CONTENT (ADR 0063), so a `package app` stub with no apiVersion and
# no schema import is correctly not ours. Before this, fixtures 1-3 were
# stubs — they matched only because the old snippet looked at filenames.
write_styleB() {  # $1 = file path, $2 = metadata.name
    mkdir -p "$(dirname "$1")"
    cat > "$1" <<EOF
package apprafter

import v1alpha1 "apprafter.io/schemas/v1alpha1"

app: v1alpha1.#Application & {
	metadata: name: "$2"
	spec: base: image: "nginxdemos/hello:plain-text"
}
EOF
}

write_styleA() {  # $1 = file path, $2 = metadata.name
    mkdir -p "$(dirname "$1")"
    cat > "$1" <<EOF
package apprafter

apiVersion: "apprafter.io/v1alpha1"
kind:       "Application"
metadata: name: "$2"
spec: base: image: "nginxdemos/hello:plain-text"
EOF
}

pass=0
fail=0

# Run the discover script in $1, assert stdout non-empty
# (expected="match") or empty (expected="nomatch"). $3 is
# a human-readable label for the test output. $4 is the
# repo-relative source path `argocd-repo-server` exports as
# ARGOCD_APP_SOURCE_PATH (i.e. the Application's
# `spec.source.path`); it defaults to `.`, which is what a
# source registered at the repository root looks like.
run_case() {
    local cwd=$1
    local expected=$2
    local label=$3
    local stdout
    local source_path=${4:-.}
    stdout=$(cd "$cwd" && ARGOCD_APP_SOURCE_PATH="$source_path" sh -c "$script_body")
    local empty
    if [[ -z "$stdout" ]]; then empty=1; else empty=0; fi
    case "$expected" in
        match)
            if [[ "$empty" -eq 0 ]]; then
                echo "PASS: $label (stdout: $(echo "$stdout" | head -1))"
                pass=$((pass + 1))
            else
                echo "FAIL: $label — expected non-empty stdout, got empty"
                fail=$((fail + 1))
            fi
            ;;
        nomatch)
            if [[ "$empty" -eq 1 ]]; then
                echo "PASS: $label (stdout empty as expected)"
                pass=$((pass + 1))
            else
                echo "FAIL: $label — expected empty stdout, got: $stdout"
                fail=$((fail + 1))
            fi
            ;;
        *)
            echo "FAIL: $label — bad expected value '$expected'" >&2
            fail=$((fail + 1))
            ;;
    esac
}

# Fixture 1: the real-world landing-web convention —
# cwd points at the parent directory, `apprafter/Application.
# cue` lives one level down. Operator's
# `apprafter app add --path landing/web` ends up with Argo
# CD's discover running here.
write_styleB "$tmp_root/landing/web/apprafter/Application.cue" "landing-web"
run_case "$tmp_root/landing/web" match "parent-dir convention: apprafter/Application.cue" "landing/web"

# Fixture 2: cwd basename IS `apprafter` — operator's `path`
# already points at the convention directory. The discover
# snippet's depth-1 branch fires here.
run_case "$tmp_root/landing/web/apprafter" match "cwd-is-apprafter: Application.cue at depth 1" "landing/web/apprafter"

# Fixture 3: filename-prefix convention — `apprafter-foo.
# cue` at the path root, no `apprafter/` subdirectory.
# Per spec.md §3.2 the operator may keep the rendered file
# next to the app code rather than in a subdirectory.
write_styleB "$tmp_root/landing/cms/apprafter-app.cue" "landing-cms"
run_case "$tmp_root/landing/cms" match "filename-prefix convention: apprafter-app.cue at root" "landing/cms"

# Fixture 4: a repo with no `apprafter*.cue` files anywhere
# — discover MUST return empty stdout so Argo CD falls
# through to the next handler. (A user running raw helm /
# kustomize / plain YAML must not be intercepted.)
mkdir -p "$tmp_root/raw-yaml"
cat > "$tmp_root/raw-yaml/package.json" <<'EOF'
{ "name": "not-an-apprafter-app" }
EOF
run_case "$tmp_root/raw-yaml" nomatch "no apprafter files: stdout must stay empty" "raw-yaml"

# Fixture 5: regression guard for a near-miss — a `.cue`
# file at depth 1 with no `apprafter` prefix AND no
# `apprafter/` subdirectory must NOT match (otherwise we
# would intercept generic CUE-using repos). This is the
# inverse of fixture 3.
mkdir -p "$tmp_root/random-cue"
cat > "$tmp_root/random-cue/main.cue" <<EOF
package x
foo: "bar"
EOF
run_case "$tmp_root/random-cue" nomatch "plain main.cue (no apprafter prefix): stdout must stay empty" "random-cue"

echo ""
echo "Summary: $pass passed, $fail failed"
if [[ "$fail" -gt 0 ]]; then exit 1; fi
