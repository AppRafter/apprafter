#!/usr/bin/env bash
# SPDX-License-Identifier: FSL-1.1-Apache-2.0
#
# check-kdf-policy.sh — fail when a release binary would derive keys with test parameters.
#
#     apprafter lock status 2>&1 | scripts/check-kdf-policy.sh     # over c1's fast-params vector
#     scripts/check-kdf-policy.sh --self-test
#
# WHY. D.4a T23 (guard L3): fast Argon2id parameters exist only behind apprafter-core's
# `insecure-fast-kdf` feature, which `cargo test --all-features` turns on for the tests. A
# binary that shipped with it would write lock-password slots guessable at microseconds per try.
# Over the frozen vector cli/apprafter-core/tests/vault_vectors/v1/fast-params — a password slot
# with fast parameters beside a recovery slot — a release binary's `lock status` refuses the
# password slot before deriving anything and names it `(refused by this build: kdf_too_weak)`;
# a feature build prints `INSECURE-FAST-KDF: …` instead and accepts the slot. Input with no
# `Slots:` line fails (exit 2): a check that read nothing is not a check.
set -euo pipefail

REFUSED='(refused by this build: kdf_too_weak)'
TEST_BUILD='INSECURE-FAST-KDF'

# Reads `lock status` output (stdout and stderr) on stdin. 0: the release policy; 1: a test
# build, or a binary that accepts the slot; 2: no Slots line.
check() {
    local text
    text="$(tr -d '\r')"
    if grep -qF "$TEST_BUILD" <<<"$text"; then
        echo "::error::the binary is a test build: it derives keys with weak parameters" >&2
        return 1
    fi
    if ! grep -q '^Slots:' <<<"$text"; then
        echo "::error::no Slots line in the input; expected apprafter lock status over the fast-params vector" >&2
        return 2
    fi
    if ! grep -qF "$REFUSED" <<<"$text"; then
        echo "::error::the binary would open a fast-parameter password slot (no kdf_too_weak on its Slots line)" >&2
        return 1
    fi
    echo "OK: the fast-parameter slot is refused before deriving; no test-build line"
}

HEAD=$'Target store:   /tmp/apprafter-kdf (id 0123456789abcdef; no targets)\nSecrets:        locked — in the vault, opened by a password or the recovery key\n'
RELEASE="${HEAD}"$'Slots:          lock password (refused by this build: kdf_too_weak) · recovery key\nThis terminal:  not run from a terminal\n'
FEATURE=$'INSECURE-FAST-KDF: this test build derives keys with weak parameters\n'"${HEAD}"$'Slots:          lock password · recovery key\nThis terminal:  not run from a terminal\n'
ACCEPTS="${HEAD}"$'Slots:          lock password · recovery key\nThis terminal:  not run from a terminal\n'
CRLF="${RELEASE//$'\n'/$'\r\n'}"
# Not a real output: the test-build line must fail the check on its own, whatever the slots say.
MIXED=$'INSECURE-FAST-KDF: this test build derives keys with weak parameters\n'"${RELEASE}"

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
    expect 0 "the release policy's refusal is accepted" "$RELEASE"
    expect 0 "the same with CRLF line ends (Windows) is accepted" "$CRLF"
    expect 1 "a feature build is refused" "$FEATURE"
    expect 1 "a binary that opens the fast slot is refused" "$ACCEPTS"
    expect 1 "the test-build line alone fails the check" "$MIXED"
    expect 2 "input with no Slots line is refused" $'Error: apprafter::secrets::vault_unreadable\n'
    echo "OK: self-test (6 cases)"
}

case "${1:-}" in
    --self-test) self_test ;;
    "") check ;;
    *) echo "usage: $0 [--self-test] < lock-status-output" >&2; exit 2 ;;
esac
