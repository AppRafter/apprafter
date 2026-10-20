#!/usr/bin/env bash
# SPDX-License-Identifier: FSL-1.1-Apache-2.0
#
# check-no-holder-left.sh — fail when an `apprafter __holder` process is still running, and end it.
#
#     scripts/check-no-holder-left.sh             # CI only: test.yml, after cargo test
#     scripts/check-no-holder-left.sh --self-test
#
# WHY. D.4a G8: a holder keeps a target store's data key in memory for one terminal's session.
# The holder tests start real holders keyed by scratch stores and end each, and the spawn
# point's drop kills and fails on any one left; this is the backstop for one that slipped past
# both. It KILLS what it finds, so it refuses to run (exit 2) unless GITHUB_ACTIONS=true: on a
# person's computer it would end their unlock windows. --self-test starts a decoy under a name
# of its own and proves the match, the kill, the exit code and that refusal on that decoy alone,
# never on a real holder.
set -euo pipefail

# Exit 2 off CI: the check kills, and a person's holders are theirs.
on_ci_only() {
    if [ "${GITHUB_ACTIONS:-}" != "true" ]; then
        echo "check-no-holder-left.sh kills every apprafter __holder it finds: it runs on CI only (GITHUB_ACTIONS=true); --self-test runs anywhere" >&2
        exit 2
    fi
}

# The command line of a holder started as `<dir>/<name> __holder`, anywhere in a process's.
pattern_for() {
    printf '(^|/)%s __holder( |$)' "$1"
}

# 0: no process matches <pattern>; 1: some did (listed on stderr, then killed).
check() {
    local pattern="$1" pids
    pids="$(pgrep -f -- "$pattern" || true)"
    if [ -z "$pids" ]; then
        echo "OK: no holder is running"
        return 0
    fi
    echo "::error::an apprafter __holder process outlived the tests:" >&2
    ps -o pid=,command= -p "$(paste -s -d, - <<<"$pids")" >&2 || true
    while IFS= read -r pid; do
        kill -KILL "$pid" 2>/dev/null || true
    done <<<"$pids"
    return 1
}

# expect <rc> <what> <pattern>: run check over <pattern>, require exit <rc>.
expect() {
    local want="$1" what="$2" pattern="$3" rc=0
    check "$pattern" >/dev/null 2>&1 || rc=$?
    if [ "$rc" -ne "$want" ]; then
        echo "self-test FAILED: $what (exit $rc, want $want)" >&2
        return 1
    fi
}

# The self-test's decoy and its directory: script-wide, so the EXIT trap still sees them when a
# failed case ends the script inside self_test (set -e).
DECOY="" DECOY_DIR=""

# A self-test that fails half-way leaves no decoy behind: on CI it would outlive the step, and
# a process holding the step's output can keep the step from ending.
end_decoy() {
    if [ -n "$DECOY" ]; then
        kill -KILL "$DECOY" 2>/dev/null || true
    fi
    if [ -n "$DECOY_DIR" ]; then
        rm -rf "$DECOY_DIR"
    fi
}

self_test() {
    local dir name pattern
    trap end_decoy EXIT
    dir="$(mktemp -d)"
    DECOY_DIR="$dir"
    name="apprafter-selftest-$$"
    pattern="$(pattern_for "$name")"
    printf '#!/usr/bin/env bash\nwhile :; do sleep 1; done\n' >"$dir/$name"
    chmod 755 "$dir/$name"
    expect 0 "nothing matches before the decoy starts" "$pattern"
    "$dir/$name" __holder </dev/null >/dev/null 2>&1 &
    DECOY=$!
    for _ in $(seq 50); do
        pgrep -f -- "$pattern" >/dev/null && break
        sleep 0.1
    done
    expect 1 "a running decoy is found and fails the check" "$pattern"
    for _ in $(seq 50); do
        pgrep -f -- "$pattern" >/dev/null || break
        sleep 0.1
    done
    expect 0 "the decoy was killed" "$pattern"
    wait "$DECOY" 2>/dev/null || true
    DECOY=""
    rm -rf "$dir"
    DECOY_DIR=""
    # Off CI the check refuses before it looks at any process. A `pgrep` that finds nothing
    # comes first on PATH, so a broken refusal still touches no real process.
    local fake rc=0
    fake="$(mktemp -d)"
    printf '#!/bin/sh\nexit 1\n' >"$fake/pgrep"
    chmod 755 "$fake/pgrep"
    env -u GITHUB_ACTIONS PATH="$fake:$PATH" bash "$0" >/dev/null 2>&1 || rc=$?
    rm -rf "$fake"
    if [ "$rc" -ne 2 ]; then
        echo "self-test FAILED: off CI the check refuses (exit $rc, want 2)" >&2
        return 1
    fi
    echo "OK: self-test (4 cases)"
}

case "${1:-}" in
    --self-test) self_test ;;
    "") on_ci_only; check "$(pattern_for apprafter)" ;;
    *) echo "usage: $0 [--self-test]" >&2; exit 2 ;;
esac
