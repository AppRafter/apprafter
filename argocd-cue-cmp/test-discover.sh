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
# in-tree `plugin.yaml`, runs it against a set of fixture
# directories covering the naming conventions, the content
# gate and the repo-relative path signal (ADR 0063), and
# asserts that stdout matches the expected match/no-match
# signal. Wired into `.github/workflows/argocd-cue-cmp-
# check.yml`; runs locally too via `bash test-discover.sh`.

set -euo pipefail

script_dir=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" >/dev/null 2>&1 && pwd)
repo_root=$(cd -- "$script_dir/.." >/dev/null 2>&1 && pwd)
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

# Hard guard against the design ADR 0063 rejected: globbing the
# absolute $PWD. This closes the door on the obvious spelling only —
# `case "$PWD/"`, `case "/$PWD/"`, an unquoted `case $PWD` and a
# `find "$PWD" -path …` form all slip past a fixed-string match.
# Case 14 is what catches those, behaviourally; this guard just turns
# the one likely spelling into a legible message instead of a
# fixture failure.
if echo "$script_body" | grep -Fq 'case "$PWD"'; then
    echo "FAIL: discover script globs the absolute \$PWD — ADR 0063 forbids it." >&2
    echo "      The CMP workdir base is set by ARGOCD_CMP_WORKDIR and TMPDIR;" >&2
    echo "      an 'apprafter' component there disables the convention gate." >&2
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

# Assert on ONE discover run. $1 = match|nomatch, $2 = the
# human-readable label, $3 = the snippet's exit status, $4 =
# its stdout. Shared by both runners below.
assert_case() {
    local expected=$1
    local label=$2
    local rc=$3
    local stdout=$4
    # Argo CD's `MatchRepository` runs the discover command through
    # `runCommand`, which returns an error on a non-zero exit and
    # discards stdout — so a non-zero exit is NO MATCH no matter what
    # was printed. `find -exec grep -l … +` exits non-zero when the
    # last grep matched nothing, which is why the snippet ends in an
    # explicit `exit 0`. Check the status before the stdout assertion
    # so a regression here is a named failure rather than an
    # unexplained `set -e` abort halfway through the suite.
    if [[ "$rc" -ne 0 ]]; then
        echo "FAIL: $label — discover exited $rc; Argo CD treats a non-zero exit as NO MATCH"
        fail=$((fail + 1))
        return
    fi
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
    local source_path=${4:-.}
    local stdout rc
    # `&& rc=0 || rc=$?` keeps `set -e` from aborting the suite on a
    # non-zero snippet exit, so the remaining cases still run and
    # assert_case can report it by name.
    stdout=$(cd "$cwd" && ARGOCD_APP_SOURCE_PATH="$source_path" sh -c "$script_body") && rc=0 || rc=$?
    assert_case "$expected" "$label" "$rc" "$stdout"
}

# Same, but with ARGOCD_APP_SOURCE_PATH genuinely UNSET rather than
# set to a default. Two things only this shape reaches:
#
#   * the off-cluster arm `case "${PWD##*/}" in apprafter)`, which
#     every other case masks — each one whose cwd basename is
#     `apprafter` also passes a source path containing `apprafter`,
#     so the `sp` arm satisfies the gate first and deleting the
#     ${PWD##*/} arm entirely leaves the rest of the suite green;
#   * ADR 0063 §Decision 2's fails-closed reasoning — Argo's
#     `environ()` drops empty values, so unset ⟺ source.path empty
#     ⟺ the cwd IS the repository root ⟺ nothing above it to match.
run_case_no_source_path() {
    local cwd=$1
    local expected=$2
    local label=$3
    local stdout rc
    stdout=$(cd "$cwd" && env -u ARGOCD_APP_SOURCE_PATH sh -c "$script_body") && rc=0 || rc=$?
    assert_case "$expected" "$label" "$rc" "$stdout"
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

# --- ADR 0063 cases ---

# 6/7: a bundle split across service directories.
write_styleB "$tmp_root/split-parent/services/api/apprafter/Application.cue" "sp-api"
write_styleB "$tmp_root/split-parent/services/web/apprafter/Application.cue" "sp-web"
run_case "$tmp_root/split-parent"              match "split across services/*: from the repo root" "."
run_case "$tmp_root/split-parent/services/api" match "split across services/*: one service"        "services/api"

# 8/9/10: a bundle split UNDER apprafter/ — the old depth-1 branch cannot see these.
write_styleB "$tmp_root/split-under/apprafter/api/Application.cue" "su-api"
write_styleB "$tmp_root/split-under/apprafter/web/Application.cue" "su-web"
run_case "$tmp_root/split-under/apprafter"     match "apprafter/{api,web}: registered at apprafter"  "apprafter"
run_case "$tmp_root/split-under"               match "apprafter/{api,web}: from the repo root"       "."
run_case "$tmp_root/split-under/apprafter/api" match "apprafter/{api,web}: ANCESTOR-only convention" "apprafter/api"

# 11/12: the convention gate applies ALWAYS — a registered path is not intent enough.
write_styleB "$tmp_root/no-convention/apps/api/Application.cue" "nc-api"
run_case "$tmp_root/no-convention/apps/api" nomatch "apps/api: no apprafter component, no prefix" "apps/api"
run_case "$tmp_root/no-convention"          nomatch "apps/api: same, from the repo root"          "."

# 13: `..` hardening — argopath cleans it for the cwd, the env var keeps it raw.
run_case "$tmp_root/no-convention/apps/api" nomatch "apps/api via a .. source path" "apprafter/../apps/api"

# 14: CONTAMINATED WORKDIR — the mutation test for this whole change.
#     Revert the snippet to globbing $PWD and this MUST flip to match.
mkdir -p "$tmp_root/apprafter/_cmp_server/fc83875e-2356-43f7-91af-bcebfc3ce63f"
cp -r "$tmp_root/no-convention/apps" \
      "$tmp_root/apprafter/_cmp_server/fc83875e-2356-43f7-91af-bcebfc3ce63f/apps"
run_case "$tmp_root/apprafter/_cmp_server/fc83875e-2356-43f7-91af-bcebfc3ce63f/apps/api" \
         nomatch "contaminated workdir base must not satisfy the convention" "apps/api"

# 15: content gate — an `apprafter` directory that holds no manifest.
mkdir -p "$tmp_root/content-gate/apprafter"
cat > "$tmp_root/content-gate/apprafter/settings.cue" <<'EOF'
package settings
owner: "team-apprafter"
EOF
run_case "$tmp_root/content-gate/apprafter" nomatch "apprafter/ dir with no manifest" "apprafter"

# 16: content gate — a mere mention of the domain in a string.
mkdir -p "$tmp_root/mentions/apprafter"
cat > "$tmp_root/mentions/apprafter/notes.cue" <<'EOF'
package notes
vendor: "apprafter.io"
EOF
run_case "$tmp_root/mentions/apprafter" nomatch "mere apprafter.io mention is not a manifest" "apprafter"

# 17: render artefacts must not self-match.
#     `types.cue` and NOT `application.cue`, deliberately: this case is
#     only a real test while the copied file actually carries the
#     marker. application.cue's sole occurrence is a PROSE COMMENT
#     (`schemas/v1alpha1/application.cue:207`), so rewording one
#     sentence in an unrelated file would leave this case passing while
#     testing nothing — and the mode it guards (a rendered checkout
#     self-matching forever under `prune: true`) is destructive.
#     types.cue carries it in CODE (`#APIVersion: "apprafter.io/v1alpha1"`),
#     which cannot be reworded away. Production injects the whole
#     directory (`entrypoint.sh:239` — `cp -f "$SCHEMA_SRC"/*.cue`), so
#     types.cue is just as real an artefact as application.cue.
mkdir -p "$tmp_root/leftover/apprafter/cue.mod/pkg/apprafter.io/schemas/v1alpha1"
cp "$repo_root/schemas/v1alpha1/types.cue" \
   "$tmp_root/leftover/apprafter/cue.mod/pkg/apprafter.io/schemas/v1alpha1/"
cat > "$tmp_root/leftover/apprafter/apprafter_claim_gen.cue" <<'EOF'
package apprafter
import v1alpha1 "apprafter.io/schemas/v1alpha1"
claim: {for t, f in v1alpha1.#ClaimFieldsFor {(t): {}}}
EOF
run_case "$tmp_root/leftover/apprafter" nomatch "cue.mod + claim_gen are not a manifest" "apprafter"

# 18: the same tree WITH a real manifest still matches.
write_styleB "$tmp_root/leftover/apprafter/Application.cue" "leftover-real"
run_case "$tmp_root/leftover/apprafter" match "leftover artefacts beside a real manifest" "apprafter"

# 19: no hijack of another team's Argo CD Application written in CUE.
#     Belt-and-braces rather than a single-mechanism guard: the
#     CONVENTION gate is what rejects this today (no `apprafter`
#     directory component, no `apprafter` filename prefix), and the
#     regex would reject it independently (`argoproj.io/v1alpha1` is
#     not `apprafter.io/…`). It survives a fully-open convention gate
#     OR a loosened regex, and flips only when both fail together.
mkdir -p "$tmp_root/other-team"
cat > "$tmp_root/other-team/argoapp.cue" <<'EOF'
package other
apiVersion: "argoproj.io/v1alpha1"
kind:       "Application"
metadata: name: "not-ours"
EOF
run_case "$tmp_root/other-team" nomatch "another team's argoproj.io app in CUE" "."

# 20: off-cluster invocation — ARGOCD_APP_SOURCE_PATH genuinely unset.
#     The only case that reaches the `${PWD##*/}` arm (see
#     run_case_no_source_path) and the only one that exercises ADR 0063
#     §Decision 2's fails-closed reading of an absent variable. Delete
#     that arm from the snippet and this is the single case that flips.
run_case_no_source_path "$tmp_root/landing/web/apprafter" match \
    "off-cluster: cwd basename apprafter, ARGOCD_APP_SOURCE_PATH unset"

echo ""
echo "Summary: $pass passed, $fail failed"
if [[ "$fail" -gt 0 ]]; then exit 1; fi
