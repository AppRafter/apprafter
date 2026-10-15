#!/usr/bin/env bash
# SPDX-License-Identifier: FSL-1.1-Apache-2.0
#
# check-e2e-isolation.sh — every e2e walk runs the CLI on a target store of its
# own (D.4a T9, G6; ADR 0068 §9).
#
#     scripts/check-e2e-isolation.sh              # scan e2e/*.sh
#     scripts/check-e2e-isolation.sh --self-test  # the rules on fixtures, then
#                                                 # the lib.sh wrappers, live
#
# ## Why
#
# A walk that runs `apprafter` without a store of its own writes into the
# default target store of whoever runs it. mvp.sh did; five release walks
# hard-coded one machine's checkout and isolated only by HOME, which an
# exported XDG_CONFIG_HOME defeats. e2e/lib.sh gives a walk e2e_isolate_store;
# its wrappers (apprafter, apprafter_in, apprafter_bin) refuse to run without
# it, and the binary refuses a path outside APPRAFTER_SANDBOX. This keeps every
# walk on that road.
#
# ## Rules (each finding names file:line)
#
#   1. A script that runs the CLI calls e2e_isolate_store at its top level,
#      before every top-level line that runs the CLI. A line runs the CLI when
#      a command word on it is apprafter, apprafter_in, apprafter_bin, a function
#      whose body runs the CLI — of the same script, or of e2e/lib.sh
#      (bootstrap_with_retry, bootstrap_with_cilium), which is scanned first —
#      or "$APPRAFTER".
#   2. Outside e2e/lib.sh no script runs cargo run / cargo build, names a
#      target/{debug,release}/apprafter path, or sets APPRAFTER to anything but
#      "$(apprafter_bin …)": the wrappers are the one road to the binary.
#   3. No script names a path under /projects/ (one machine's checkout).
#
# Comments, heredoc bodies and single-quoted text are not code. A command word
# starts a line, or follows ; & | ( { ! $( or then / do / else / if / -- /
# trap / exec / time.
#
# --self-test runs the rules over fixtures, then sources e2e/lib.sh and
# checks, live: without e2e_isolate_store the wrapper refuses before cargo
# starts; the wrappers turn the startup checks off per call unless the call
# sets E2E_STARTUP_CHECKS=1 (a stub stands in for the CLI); a walk isolated as
# mvp.sh isolates itself (a throwaway key as APPRAFTER_E2E_SSH_KEY) runs
# `apprafter target list` — a command the fence checks — and exits 0; the same
# walk with its SSH identity outside the sandbox is refused with
# apprafter::sandbox::escape, so the green row is not vacuous. Those rows build
# the CLI (cargo, cue) unless APPRAFTER_BIN names one.
set -euo pipefail

cd "$(dirname "$0")/.."
ROOT="$PWD"
LIB="$ROOT/e2e/lib.sh"

# The rules, as awk over one script (-v file=<path> -v lib=0|1
# -v libfns="<names>" -v emit=0|1). libfns: the functions of e2e/lib.sh that
# run the CLI, which every script may call. emit=1 prints the script's own
# CLI-running functions, space-separated, and nothing else (run on lib.sh).
read -r -d '' SCAN <<'AWK' || true
BEGIN {
    pre = "(^|[;&|({!][[:space:]]*|[$][(][[:space:]]*|(^|[[:space:]])(then|do|else|if|--|trap|exec|time)[[:space:]]+|^[[:space:]]+)"
    post = "([[:space:];)|&]|$)"
    n = split(libfns, lf, " ")
    for (k = 1; k <= n; k++) clifn[lf[k]] = 1
}
function code(s) { gsub(/'[^']*'/, "", s); return s }
function runs_cli(s,   t, f) {
    if (s ~ /"[$][{]?APPRAFTER[}]?"/) return 1
    t = code(s)
    if (t ~ (pre "(apprafter|apprafter_in|apprafter_bin)" post)) return 1
    for (f in clifn) if (t ~ (pre f post)) return 1
    return 0
}
{ text[NR] = $0 }
END {
    tag = ""; fn = ""
    for (i = 1; i <= NR; i++) {
        s = text[i]; kind[i] = "code"; owner[i] = ""
        if (tag != "") {
            b = s; sub(/^[ \t]+/, "", b); sub(/[ \t]+$/, "", b)
            if (b == tag) tag = ""
            kind[i] = "heredoc"
            continue
        }
        if (s ~ /^[ \t]*#/) { kind[i] = "comment"; continue }
        if (match(s, /<<-?[ \t]*["']?[A-Za-z_][A-Za-z0-9_]*["']?/)) {
            t = substr(s, RSTART, RLENGTH); sub(/^<<-?[ \t]*["']?/, "", t); sub(/["']$/, "", t); tag = t
        }
        if (fn == "" && s ~ /^[A-Za-z_][A-Za-z0-9_]*[(][)][ \t]*[{]/) {
            fn = s; sub(/[(].*/, "", fn); owner[i] = fn
            if (s ~ /[}][ \t;]*$/) fn = ""
            continue
        }
        if (fn != "") { owner[i] = fn; if (s ~ /^[}]/) fn = "" }
    }
    do {
        grew = 0
        for (i = 1; i <= NR; i++)
            if (kind[i] == "code" && owner[i] != "" && !(owner[i] in clifn) && runs_cli(text[i])) {
                clifn[owner[i]] = 1; grew = 1
            }
    } while (grew)
    if (emit) {
        out = ""
        for (f in clifn) out = out (out == "" ? "" : " ") f
        print out
        exit
    }
    iso = 0; first = 0; ne = 0
    for (i = 1; i <= NR; i++) {
        if (kind[i] != "code") continue
        s = text[i]; t = code(s)
        if (owner[i] == "" && iso == 0 && t ~ /(^|[;&|[:space:]])e2e_isolate_store([[:space:];]|$)/) iso = i
        if (runs_cli(s)) {
            if (first == 0) first = i
            # A top-level CLI line before isolation: reported below, once it is
            # known whether the script isolates at all.
            if (!lib && owner[i] == "" && iso == 0) early[++ne] = i
        }
        if (!lib && t ~ /cargo[[:space:]]+(run|build)([[:space:]]|$)/)
            printf "%s:%d: builds or runs the CLI outside e2e/lib.sh (use apprafter, apprafter_in or apprafter_bin): %s\n", file, i, s
        if (!lib && t ~ /target[/](debug|release)[/]apprafter/)
            printf "%s:%d: names a built CLI outside e2e/lib.sh (use apprafter_bin): %s\n", file, i, s
        if (!lib && t ~ /(^|[^A-Za-z0-9_])APPRAFTER=/ && t !~ /APPRAFTER="[$][(]apprafter_bin/)
            printf "%s:%d: APPRAFTER comes from apprafter_bin only: %s\n", file, i, s
        if (t ~ /[/]projects[/]/)
            printf "%s:%d: a path of one machine (/projects/): %s\n", file, i, s
    }
    # One finding for a script that never isolates; one per early line otherwise.
    if (!lib && first > 0 && iso == 0)
        printf "%s:%d: runs the CLI and never calls e2e_isolate_store at its top level\n", file, first
    else if (!lib)
        for (k = 1; k <= ne; k++)
            printf "%s:%d: runs the CLI before e2e_isolate_store: %s\n", file, early[k], text[early[k]]
}
AWK

# The functions of e2e/lib.sh that run the CLI (bootstrap_with_retry and
# bootstrap_with_cilium at the time of writing): a walk that calls one runs
# the CLI as surely as one that calls apprafter (critic finding 1).
LIBFNS="$(awk -v file="$LIB" -v lib=1 -v libfns= -v emit=1 "$SCAN" "$LIB")"

# scan <file>…: every finding, one per line; status 1 when there is one.
scan() {
    local f lib out found=0
    for f in "$@"; do
        lib=0
        [ "$(basename "$f")" = lib.sh ] && lib=1
        out="$(awk -v file="$f" -v lib="$lib" -v libfns="$LIBFNS" -v emit=0 "$SCAN" "$f")"
        if [ -n "$out" ]; then
            printf '%s\n' "$out"
            found=1
        fi
    done
    return "$found"
}

FAILED=0
fail() { printf 'self-test FAILED: %s\n' "$1" >&2; FAILED=1; }

# fixture <want> <name>: scan the script on stdin; require <want> findings.
fixture() {
    local want="$1" name="$2" dir got
    dir="$(mktemp -d)"
    cat >"$dir/$name.sh"
    got="$( { scan "$dir/$name.sh" || true; } | grep -c . || true)"
    rm -rf "$dir"
    [ "$got" -eq "$want" ] || fail "$name: $got finding(s), want $want"
}

static_rows() {
    fixture 0 isolated <<'SH'
source "$(dirname "$0")/lib.sh"
TMPDIR_WORK="$(mktemp -d)"
e2e_isolate_store "${TMPDIR_WORK}"
apprafter target list
list_out="$(apprafter migration list)"
retry 3 20 -- apprafter apply
SH
    fixture 1 never_isolated <<'SH'
apprafter target list
SH
    fixture 1 isolated_too_late <<'SH'
apprafter target list
e2e_isolate_store
SH
    fixture 1 a_cli_function_called_first <<'SH'
from_fixture() {
    apprafter_in "$PWD" target list
}
from_fixture
e2e_isolate_store
SH
    fixture 1 a_trap_armed_before_isolation <<'SH'
cleanup() {
    apprafter destroy --yes || true
}
trap cleanup EXIT
e2e_isolate_store
SH
    # A function of e2e/lib.sh that runs the CLI, called before isolation, bare
    # and through a function of the walk's own (critic finding 1).
    fixture 1 a_lib_helper_called_first <<'SH'
source "$(dirname "$0")/lib.sh"
bootstrap_with_retry
e2e_isolate_store
SH
    fixture 1 a_lib_helper_inside_a_walk_function <<'SH'
cluster_up() {
    kind_up "$1"
    if bootstrap_with_cilium; then return 0; fi
}
cluster_up src
e2e_isolate_store
SH
    fixture 0 a_lib_helper_after_isolation <<'SH'
TMPDIR_WORK="$(mktemp -d)"
e2e_isolate_store "${TMPDIR_WORK}"
bootstrap_with_retry
SH
    fixture 1 cargo_run_outside_lib <<'SH'
e2e_isolate_store
( cd cli && cargo run --quiet --bin apprafter -- target list )
SH
    fixture 1 a_built_binary_by_path <<'SH'
e2e_isolate_store
"${REPO_ROOT}/cli/target/release/apprafter" target list
SH
    fixture 3 apprafter_from_a_machine_path <<'SH'
APPRAFTER=/projects/x/cli/target/release/apprafter
e2e_isolate_store "$WORK"
"$APPRAFTER" up
SH
    fixture 0 a_release_walk <<'SH'
source "$(dirname "$0")/lib.sh"
WORK=$(mktemp -d /tmp/apprafter-x-XXXXXX)
e2e_isolate_store "$WORK"
APPRAFTER="$(apprafter_bin)"
cleanup() {
    "$APPRAFTER" destroy --yes || true
}
trap cleanup EXIT
timeout 1500 "$APPRAFTER" up
env -u APPRAFTER_SKIP_NODE_SWAP "$APPRAFTER" node prep --yes
SH
    fixture 0 text_is_not_code <<'SH'
# apprafter up, in a comment
printf 'run apprafter destroy --yes by hand\n'
cat <<EOF
apprafter up
EOF
kubectl -n apprafter get pods
for tool in cargo kubectl; do command -v "$tool"; done
SH
}

# Without e2e_isolate_store the wrapper refuses, before cargo starts.
row_refuses_without_isolation() {
    local scratch out rc=0
    scratch="$(mktemp -d)"
    mkdir "$scratch/bin"
    printf '#!/bin/sh\ntouch "%s/cargo-ran"\nexit 0\n' "$scratch" >"$scratch/bin/cargo"
    chmod 755 "$scratch/bin/cargo"
    out="$(env -u APPRAFTER_SANDBOX -u E2E_SANDBOX_DIR -u APPRAFTER_BIN PATH="$scratch/bin:$PATH" \
        bash -c 'source "$1"; apprafter --version' _ "$LIB" 2>&1)" || rc=$?
    if [ "$rc" -ne 2 ] || ! grep -q 'call e2e_isolate_store' <<<"$out" || [ -e "$scratch/cargo-ran" ]; then
        fail "without e2e_isolate_store the wrapper must refuse before cargo starts (exit $rc): $out"
    fi
    rm -rf "$scratch"
}

# A walk isolated as mvp.sh isolates itself runs `target list`; with an SSH
# identity outside the sandbox the same walk is refused by the binary's fence.
row_admits_an_isolated_walk() {
    local scratch outside out rc
    command -v ssh-keygen >/dev/null || { fail "ssh-keygen is needed (the walk's key check)"; return; }
    scratch="$(mktemp -d)"
    outside="$(mktemp -d)"
    ssh-keygen -q -t ed25519 -N '' -C e2e-isolation-self-test -f "$scratch/id"
    cat >"$scratch/walk.sh" <<'WALK'
set -euo pipefail
source "$1"
e2e_isolate_store
e2e_require_ssh_key --body "$APPRAFTER_SSH_PUBLIC_KEY"
if [ -n "${OUTSIDE_KEY:-}" ]; then export APPRAFTER_SSH_PRIVATE_KEY="$OUTSIDE_KEY"; fi
rc=0
apprafter target list || rc=$?
e2e_isolation_cleanup
exit "$rc"
WALK
    for outside_key in "" "$outside/id_ed25519"; do
        rc=0
        # No XDG_RUNTIME_DIR either: a wrapper that stopped setting its own per
        # call must not hand the CLI the caller's real one (critic finding 17).
        out="$(env -u APPRAFTER_SANDBOX -u E2E_SANDBOX_DIR -u APPRAFTER_CONFIG_DIR -u APPRAFTER_AGE_KEY \
            -u APPRAFTER_E2E_STORE -u APPRAFTER_SSH_PRIVATE_KEY -u XDG_CONFIG_HOME -u XDG_RUNTIME_DIR \
            APPRAFTER_E2E_SSH_KEY="$scratch/id" APPRAFTER_SSH_PUBLIC_KEY="$(cat "$scratch/id.pub")" \
            KUBECONFIG=/nonexistent OUTSIDE_KEY="$outside_key" \
            bash "$scratch/walk.sh" "$LIB" 2>&1)" || rc=$?
        if [ -z "$outside_key" ]; then
            if [ "$rc" -ne 0 ] || grep -q 'apprafter::sandbox' <<<"$out"; then
                fail "an isolated walk's 'apprafter target list' must run (exit $rc): $out"
            fi
        elif [ "$rc" -eq 0 ] || ! grep -q 'apprafter::sandbox::escape' <<<"$out"; then
            fail "an SSH identity outside the sandbox must be refused by the fence (exit $rc): $out"
        fi
    done
    rm -rf "$scratch" "$outside"
}

# The startup checks are off for every call through either wrapper unless the
# call sets E2E_STARTUP_CHECKS=1 (critic finding 7). A stub stands in for the
# CLI and prints what it was given; nothing is built.
row_startup_checks_opt_in() {
    local scratch out
    scratch="$(mktemp -d)"
    printf '#!/bin/sh\nprintf "%%s\\n" "${APPRAFTER_SKIP_STARTUP_CHECKS-unset}"\n' >"$scratch/cli"
    chmod 755 "$scratch/cli"
    out="$(env -u APPRAFTER_SANDBOX -u E2E_SANDBOX_DIR -u APPRAFTER_E2E_STORE \
        -u APPRAFTER_SKIP_STARTUP_CHECKS -u E2E_STARTUP_CHECKS APPRAFTER_BIN="$scratch/cli" \
        bash -c '
            set -euo pipefail
            source "$1"
            e2e_isolate_store
            w="$(apprafter_bin)"
            printf "%s %s %s %s\n" "$(apprafter x)" "$(E2E_STARTUP_CHECKS=1 apprafter x)" \
                "$("$w" x)" "$(E2E_STARTUP_CHECKS=1 "$w" x)"
            e2e_isolation_cleanup' _ "$LIB" 2>&1)" || true
    if [ "$out" != "1 unset 1 unset" ]; then
        fail "the startup checks must be off per call unless E2E_STARTUP_CHECKS=1, through apprafter and apprafter_bin (want '1 unset 1 unset'): got '$out'"
    fi
    rm -rf "$scratch"
}

if [ "${1:-}" = "--self-test" ]; then
    static_rows
    grep -qw bootstrap_with_retry <<<"$LIBFNS" \
        || fail "the scan of e2e/lib.sh found no bootstrap_with_retry among its CLI functions: '$LIBFNS'"
    row_refuses_without_isolation
    row_startup_checks_opt_in
    row_admits_an_isolated_walk
    if [ "$FAILED" -ne 0 ]; then
        exit 1
    fi
    echo "check-e2e-isolation self-test: OK"
    exit 0
fi

mapfile -t files < <(git ls-files 'e2e/*.sh')
[ "${#files[@]}" -gt 0 ] || { echo "no e2e scripts: is this the repository root?" >&2; exit 2; }
if ! scan "${files[@]}"; then
    cat >&2 <<'MSG'

Every e2e walk runs the CLI on a target store of its own: call
e2e_isolate_store [<dir>] (e2e/lib.sh) at the top level before the first CLI
call, and reach the binary only through apprafter, apprafter_in or
APPRAFTER="$(apprafter_bin)". The header of scripts/check-e2e-isolation.sh
has the rules.
MSG
    exit 1
fi
printf 'every e2e walk that runs the CLI isolates its store (%d script(s) scanned)\n' "${#files[@]}"
