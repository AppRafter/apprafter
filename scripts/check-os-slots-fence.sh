#!/usr/bin/env bash
# SPDX-License-Identifier: FSL-1.1-Apache-2.0
#
# check-os-slots-fence.sh — fail when a binary carries the account-password stand-in (D.4b G-OS3).
#
#     scripts/check-os-slots-fence.sh run <binary> <fresh scratch dir>   # what its lock on does
#     scripts/check-os-slots-fence.sh strings <binary>                   # what its bytes hold
#     scripts/check-os-slots-fence.sh --self-test
#
# WHY. D.4b B16, B43 (G-OS3): with the dev-only cargo feature `test-os-slots`, the CLI takes its
# account-password check and this computer's facts from a scripted stand-in when
# APPRAFTER_TEST_OS_SLOTS names a script; a binary shipped with it would take any typed text as the
# person's account password. The release profile strips symbols (cli/Cargo.toml), so the check is
# behavioural first, then on the bytes:
#
# `run`: on an empty scratch store, `lock on` with APPRAFTER_SANDBOX set to the scratch dir,
# APPRAFTER_TEST_OS_SLOTS naming a script whose check says Verified, and no terminal. A binary
# without the stand-in finds the real check behind the APPRAFTER_SANDBOX fence (the account running
# it is no test account), asks the system nothing, falls back to a lock password and says why —
# naming APPRAFTER_SANDBOX — then refuses (`usage_refused`: no terminal) naming a new lock password.
# A binary with the stand-in reads the script, which writes <script>.calls beside it, plans the
# account password and refuses naming it. Output with no `usage_refused` fails apart (exit 2):
# lock on never reached its plan, and a check that read nothing is not a check. A scratch store
# other accounts can write is refused before the plan (`store_not_private`, WI-475) and named as
# such, also exit 2: the caller gives a dir only this account can change.
# `strings`: neither the variable's name nor the stand-in's own mark (`STAND_IN_MARK`,
# cli/os-slots/src/stand_in.rs) is in the binary's bytes (grep -a: Git Bash has no `strings`).
#
# `run` sets the store, home, runtime and temporary dirs, KUBECONFIG=/nonexistent and no startup
# checks itself; on Windows also the profile's folders, the age key and the SSH identity, which no
# HOME moves there. Nothing it starts can reach a person's store, account or cluster.
set -euo pipefail

VARIABLE='APPRAFTER_TEST_OS_SLOTS'
MARK='apprafter-os-slots scripted stand-in'
REFUSED='apprafter::cli::usage_refused'
NOT_PRIVATE='apprafter::secrets::store_not_private'
LOCK_PASSWORD='asks for a new lock password'
ACCOUNT_PASSWORD='asks for your account password'
FENCE='while APPRAFTER_SANDBOX is set'

# check <calls file> < lock on's output: 0 no stand-in; 1 a stand-in, or no fence; 2 lock on
# never reached its plan.
check() {
    local calls="$1" text
    text="$(tr -d '\r')"
    # Before the fence matters, lock on refuses a store other accounts can change (WI-475): a
    # scratch dir whose folder grants others write access (Windows' RUNNER_TEMP on D:) says so.
    if grep -qF "$NOT_PRIVATE" <<<"$text"; then
        echo "::error::lock on refused the scratch store as not private: give a scratch dir only this account can change" >&2
        return 2
    fi
    if ! grep -qF "$REFUSED" <<<"$text"; then
        echo "::error::no $REFUSED in the output: lock on did not reach its plan" >&2
        return 2
    fi
    if [ -e "$calls" ]; then
        echo "::error::the binary read the stand-in's script ($calls exists): it was built with test-os-slots" >&2
        return 1
    fi
    if grep -qF "$ACCOUNT_PASSWORD" <<<"$text" || ! grep -qF "$LOCK_PASSWORD" <<<"$text"; then
        echo "::error::lock on did not take a lock password under APPRAFTER_SANDBOX" >&2
        return 1
    fi
    if ! grep -qF "$FENCE" <<<"$text"; then
        echo "::error::lock on did not say that APPRAFTER_SANDBOX made it take a lock password" >&2
        return 1
    fi
    echo "OK: the fence kept the account-password check unasked; no stand-in read its script"
}

# literals <binary>: 0 when neither literal is in its bytes.
literals() {
    local bin="$1" found=0 needle
    for needle in "$VARIABLE" "$MARK"; do
        if grep -aqF "$needle" "$bin"; then
            echo "::error::$bin holds '$needle': the account-password stand-in is compiled in" >&2
            found=1
        fi
    done
    if [ "$found" -eq 0 ]; then
        echo "OK: neither $VARIABLE nor the stand-in's mark is in $bin"
    fi
    return "$found"
}

# run <binary> <scratch>: `lock on` there, as the header says; its output, then check's verdict.
run() {
    local bin="$1" scratch="$2" out windows=no
    case "$(uname -s)" in MINGW* | MSYS* | CYGWIN*) windows=yes ;; esac
    if [ -e "$scratch" ]; then
        echo "run: $scratch exists: give a fresh scratch dir" >&2
        return 2
    fi
    mkdir -p "$scratch/store" "$scratch/home/.ssh" "$scratch/run" "$scratch/tmp" \
        "$scratch/config" "$scratch/data"
    if [ "$windows" = no ]; then
        chmod 700 "$scratch/run"
    fi
    printf 'check verified-if right horse battery staple\n' >"$scratch/os-slots.script"
    local vars=(
        APPRAFTER_SKIP_STARTUP_CHECKS=1 KUBECONFIG=/nonexistent
        APPRAFTER_CONFIG_DIR="$scratch/store" HOME="$scratch/home"
        XDG_RUNTIME_DIR="$scratch/run" TMPDIR="$scratch/tmp"
        APPRAFTER_SANDBOX="$scratch" APPRAFTER_TEST_OS_SLOTS="$scratch/os-slots.script"
    )
    if [ "$windows" = yes ]; then
        vars+=(
            USERPROFILE="$scratch/home" APPDATA="$scratch/config" LOCALAPPDATA="$scratch/data"
            TMP="$scratch/tmp" TEMP="$scratch/tmp"
            APPRAFTER_AGE_KEY="$scratch/home/age.key"
            APPRAFTER_SSH_PRIVATE_KEY="$scratch/home/.ssh/id_ed25519"
        )
    fi
    out="$(env "${vars[@]}" "$bin" lock on </dev/null 2>&1)" || true
    printf '%s\n' "$out"
    check "$scratch/os-slots.script.calls" <<<"$out"
}

# expect <rc> <what> <calls file> <input>: run check over <input>, require exit <rc>.
expect() {
    local want="$1" what="$2" calls="$3" input="$4" rc=0
    check "$calls" <<<"$input" >/dev/null 2>&1 || rc=$?
    if [ "$rc" -ne "$want" ]; then
        echo "self-test FAILED: $what (exit $rc, want $want)" >&2
        return 1
    fi
}

self_test() {
    local tmp
    tmp="$(mktemp -d)"
    # shellcheck disable=SC2064 # expand now: the dir is this run's
    trap "rm -rf '$tmp'" EXIT
    local none="$tmp/none.calls" some="$tmp/some.calls"
    printf '# %s\nBackend\n' "$MARK" >"$some"
    local head=$'AppRafter does not check your account password while APPRAFTER_SANDBOX is set: the variable fences tests and agents away from it.\nThe store gets a separate lock password instead.\n'
    local lock=$'Error: apprafter::cli::usage_refused\n\n  x `apprafter lock on` asks for a new lock password and shows a recovery key: run it in a terminal\n'
    local account=$'Error: apprafter::cli::usage_refused\n\n  x `apprafter lock on` asks for your account password and shows a recovery key: run it in a terminal\n'
    local release="${head}${lock}"
    expect 0 "the shipped binary's fallback is accepted" "$none" "$release"
    expect 0 "the same with CRLF line ends (Windows) is accepted" "$none" "${release//$'\n'/$'\r\n'}"
    expect 1 "a binary that read the script is refused, whatever it said" "$some" "$release"
    expect 1 "a binary that planned the account password is refused" "$none" "$account"
    expect 1 "a fallback that does not name the fence is refused" "$none" "$lock"
    expect 2 "output that never reached the plan is refused apart" "$none" $'Error: apprafter::sandbox::escape\n'
    printf 'clean bytes\n' >"$tmp/clean"
    printf 'x%sy\n' "$MARK" >"$tmp/marked"
    printf 'x%sy\n' "$VARIABLE" >"$tmp/named"
    literals "$tmp/clean" >/dev/null 2>&1 || { echo "self-test FAILED: clean bytes refused" >&2; return 1; }
    ! literals "$tmp/marked" >/dev/null 2>&1 || { echo "self-test FAILED: the stand-in's mark passed" >&2; return 1; }
    ! literals "$tmp/named" >/dev/null 2>&1 || { echo "self-test FAILED: the variable passed" >&2; return 1; }
    echo "OK: self-test (9 cases)"
}

case "${1:-}" in
    --self-test) self_test ;;
    run)
        [ $# -eq 3 ] || { echo "usage: $0 run <binary> <fresh scratch dir>" >&2; exit 2; }
        run "$2" "$3"
        ;;
    strings)
        [ $# -eq 2 ] || { echo "usage: $0 strings <binary>" >&2; exit 2; }
        literals "$2"
        ;;
    *)
        echo "usage: $0 run <binary> <fresh scratch dir> | strings <binary> | --self-test" >&2
        exit 2
        ;;
esac
