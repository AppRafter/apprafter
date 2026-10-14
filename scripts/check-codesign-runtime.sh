#!/usr/bin/env bash
# SPDX-License-Identifier: FSL-1.1-Apache-2.0
#
# check-codesign-runtime.sh — fail when a macOS binary's signature lacks the hardened runtime.
#
#     codesign --display --verbose=2 apprafter 2>&1 | scripts/check-codesign-runtime.sh
#     scripts/check-codesign-runtime.sh --self-test
#
# WHY. D.4a T55: the unlock holder keeps a target store's data key in memory. On macOS a process
# with the hardened runtime and no get-task-allow entitlement does not hand its task port to
# other programs running as the same user, so they cannot read that memory unless the computer
# is set up for debugging. release-cli.yml signs each macOS binary ad hoc with the runtime
# (`codesign --sign - --options runtime`, no Developer ID) after stripping it, and runs this
# over the result.
#
# Input is codesign's display text; the line that matters is
#     CodeDirectory v=20500 size=55678 flags=0x10002(adhoc,runtime) hashes=1729+2 location=embedded
# Input with no CodeDirectory line fails (exit 2) instead of passing: a guard that read nothing
# is not a guard. --self-test runs the check over built-in samples.
set -euo pipefail

# The flag list of a CodeDirectory line that names `runtime` among its flags.
RUNTIME='^CodeDirectory .*flags=0x[0-9a-f]+\(([^)]*,)?runtime(,[^)]*)?\)'

# Reads codesign output on stdin. 0: the runtime flag; 1: a CodeDirectory without it; 2: none.
check() {
    local text line
    text="$(tr -d '\r')"
    line="$(grep -E '^CodeDirectory ' <<<"$text" || true)"
    if [ -z "$line" ]; then
        echo "::error::no CodeDirectory line in the input; expected codesign --display --verbose=2 output" >&2
        return 2
    fi
    if ! grep -Eq "$RUNTIME" <<<"$line"; then
        echo "::error::the signature has no hardened-runtime flag: $line" >&2
        return 1
    fi
    echo "OK: signed with the hardened runtime ($line)"
}

SIGNED=$'Executable=/Users/runner/work/apprafter/cli/target/aarch64-apple-darwin/release/apprafter\nIdentifier=apprafter\nFormat=Mach-O thin (arm64)\nCodeDirectory v=20500 size=55678 flags=0x10002(adhoc,runtime) hashes=1729+2 location=embedded\nSignature=adhoc\nInfo.plist=not bound\nTeamIdentifier=not set\nRuntime Version=14.0.0\nSealed Resources=none\nInternal requirements count=0 size=12\n'
LINKER=$'Executable=/Users/runner/work/apprafter/cli/target/aarch64-apple-darwin/release/apprafter\nIdentifier=apprafter\nFormat=Mach-O thin (arm64)\nCodeDirectory v=20400 size=55666 flags=0x20002(adhoc,linker-signed) hashes=1729+0 location=embedded\nSignature=adhoc\nInfo.plist=not bound\nTeamIdentifier=not set\nSealed Resources=none\nInternal requirements=none\n'
ONLY=$'CodeDirectory v=20500 size=100 flags=0x10000(runtime) hashes=1+2 location=embedded\r\n'
UNSIGNED=$'apprafter: code object is not signed at all\n'

# expect <rc> <what> <input>: run check over <input>, require exit <rc>.
expect() {
    local want="$1" what="$2" input="$3" rc=0
    check <<<"$input" >/dev/null 2>&1 || rc=$?
    if [ "$rc" -ne "$want" ]; then
        echo "self-test FAILED: $what (exit $rc, want $want)" >&2
        return 1
    fi
}

self_test() {
    expect 0 "an ad-hoc signature with the runtime is accepted" "$SIGNED"
    expect 0 "the runtime alone, with CRLF, is accepted" "$ONLY"
    expect 1 "the linker's own signature (no runtime) is refused" "$LINKER"
    expect 2 "an unsigned binary is refused" "$UNSIGNED"
    expect 2 "no input is refused" ""
    echo "OK: self-test (5 cases)"
}

case "${1:-}" in
    --self-test) self_test ;;
    "") check ;;
    *) echo "usage: $0 [--self-test] < codesign-display-output" >&2; exit 2 ;;
esac
