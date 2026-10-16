#!/usr/bin/env bash
# SPDX-License-Identifier: FSL-1.1-Apache-2.0
#
# The macOS account-password check of cli/os-slots (D.4b-d; GOTCHA-327) against accounts this
# script makes: every test of cli/os-slots/tests/macos_real.rs, each run as `walker` (its home
# /Users/walker holds the test-account marker) or, for the fence's case, `walker2` (none).
#
#   bash scripts/test-os-slots-macos.sh        # on a GitHub-hosted macOS runner only
#
# It creates and deletes accounts with sudo, so it refuses to run anywhere but a GitHub-hosted
# macOS runner (GITHUB_ACTIONS=true, RUNNER_ENVIRONMENT=github-hosted, RUNNER_OS=macOS): never
# on a person's Mac (CLAUDE.md; ADR 0069 answer 6). A case passes only when its one test reported
# `ok` and the harness ran exactly one test; the cases here must be exactly the tests in the
# binary.
set -euo pipefail

die() {
    echo "test-os-slots-macos: $*" >&2
    exit 1
}

[[ "${GITHUB_ACTIONS:-}" == true && "${RUNNER_ENVIRONMENT:-}" == github-hosted && "${RUNNER_OS:-}" == macOS ]] ||
    die "this script creates accounts: it runs only on a GitHub-hosted macOS runner"

REPO_ROOT="$(git rev-parse --show-toplevel)"
# Each case: the account it runs as, then the test's name.
CASES=(
    "walker the_right_password_is_verified_through_chkpasswd"
    "walker a_wrong_password_is_wrong_and_the_next_run_is_refused_before_pam"
    "walker an_empty_entry_never_reaches_pam"
    "walker the_hardware_uuid_is_its_16_bytes_and_the_back_off_has_its_place"
    "walker the_password_status_is_unknown_and_its_change_time_is_read"
    "walker2 an_unmarked_account_is_fenced"
)
SHARED=/Users/Shared/apprafter-os-slots-test

work="$(mktemp -d)"
cleanup() {
    sudo sysadminctl -deleteUser walker >/dev/null 2>&1 || true
    sudo sysadminctl -deleteUser walker2 >/dev/null 2>&1 || true
    sudo rm -rf "$SHARED"
    rm -rf "$work"
}
trap cleanup EXIT

# 1. The test binary.
echo "==> building cli/os-slots/tests/macos_real.rs"
(cd "$REPO_ROOT/cli" && cargo test --locked --color never -p apprafter-os-slots --test macos_real --no-run) \
    >"$work/build.log" 2>&1 || {
    cat "$work/build.log" >&2
    die "building the test failed"
}
bin="$(sed -n 's/^ *Executable tests\/macos_real\.rs (\(.*\))$/\1/p' "$work/build.log")"
[[ -n "$bin" ]] || {
    cat "$work/build.log" >&2
    die "cargo did not name the macos_real binary"
}
[[ "$bin" == /* ]] || bin="$REPO_ROOT/cli/$bin"

# 2. The cases here are exactly the binary's tests.
listed="$("$bin" --list --ignored | sed -n 's/: test$//p' | sort)"
known="$(for c in "${CASES[@]}"; do set -- $c; echo "$2"; done | sort)"
if [[ "$listed" != "$known" ]]; then
    printf 'tests in the binary:\n%s\ncases in this script:\n%s\n' "$listed" "$known" >&2
    die "the cases here and the tests in the binary differ"
fi

# 3. The accounts, the marker, and the binary where they can run it.
PASSWORD="Os-slots-$(openssl rand -hex 8)!"
UUID="$(ioreg -rd1 -c IOPlatformExpertDevice | awk -F'"' '/IOPlatformUUID/ {print $4}')"
[[ "$UUID" =~ ^[0-9A-F-]{36}$ ]] || die "no hardware UUID from ioreg: '$UUID'"
for account in walker walker2; do
    sudo sysadminctl -addUser "$account" -fullName "AppRafter os-slots test" -password "$PASSWORD" \
        -home "/Users/$account" -shell /bin/zsh >/dev/null 2>&1
    sudo createhomedir -c -u "$account" >/dev/null
done
sudo touch /Users/walker/.apprafter-test-account
sudo chown walker /Users/walker/.apprafter-test-account
sudo rm -f /Users/walker2/.apprafter-test-account
sudo mkdir -p "$SHARED"
sudo cp "$bin" "$SHARED/macos_real"
sudo chmod 0755 "$SHARED" "$SHARED/macos_real"

# 4. The cases, each with no back-off record left by the one before.
failures=0
failed=""
for c in "${CASES[@]}"; do
    set -- $c
    account="$1" name="$2"
    tmp="$(sudo -u walker getconf DARWIN_USER_TEMP_DIR)"
    sudo rm -f "${tmp}apprafter-account-password-backoff"
    log="$work/$name.log"
    if sudo -u "$account" env -i PATH=/usr/bin:/bin:/usr/sbin:/sbin HOME="/Users/$account" \
        APPRAFTER_OS_SLOTS_MACOS_JOB=1 APPRAFTER_OS_SLOTS_PASSWORD="$PASSWORD" \
        APPRAFTER_OS_SLOTS_HOST_UUID="$UUID" \
        "$SHARED/macos_real" --ignored --exact --nocapture "$name" >"$log" 2>&1 &&
        grep -qx "test $name ... ok" "$log" &&
        grep -q '^test result: ok\. 1 passed; 0 failed' "$log"; then
        echo "ok    $name"
    else
        echo "FAIL  $name"
        sed 's/^/    /' "$log"
        failures=$((failures + 1))
        failed="$failed $name"
    fi
done
[[ $failures -eq 0 ]] || die "$failures of ${#CASES[@]} case(s) failed:$failed"
echo "all ${#CASES[@]} case(s) passed"
