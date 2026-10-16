#!/usr/bin/env bash
# SPDX-License-Identifier: FSL-1.1-Apache-2.0
#
# The account-password check of cli/os-slots against real PAM stacks, as a normal user: every
# test of cli/os-slots/tests/pam_container.rs, each in a fresh podman container of Debian 13 and
# of Fedora 42 with that distribution's libpam, pam_unix and unix_chkpwd, a throwaway account
# `walker` with a known password, its home holding the test-account marker, and /run/user/<uid>
# as logind would make it (D.4b-d; ADR 0069 answer 6: a test account the script makes, never a
# person's).
#
#   bash scripts/test-os-slots-linux.sh [--distro debian|fedora] [case...]   # all by default
#   PODMAN='sudo podman' bash scripts/test-os-slots-linux.sh                 # rootful, as CI
#   CARGO_CMD='<wrapper> cargo' bash scripts/test-os-slots-linux.sh          # cargo via a wrapper
#
# Needs cargo and podman. A missing podman FAILS: this is a regression test, and skipping it
# would read as a pass. Nothing touches the host's PAM or accounts: each container reads its own
# /etc/pam.d and /etc/shadow.
#
# The test binary is built on the host, mounted read-only, and started through the container's
# own loader with its library directory first (as scripts/test-osauth-linux.sh does), so the
# libpam it loads by dlopen and the PAM modules are the distribution's. A build-time
# APPRAFTER_LIBPAM path (the nix dev shell's) does not exist in the container and is skipped.
# x86_64 only (the library directories below).
#
# Each case's flags prepare the system before the test runs as walker:
#   --stack permit|fingerprint|second-prompt|echo-prompt|faillock   the auth stack of the
#       service a check uses (Debian common-auth, Fedora system-auth); second-prompt and
#       echo-prompt add pam_twoprompt.so, a test module built into the image that asks one more
#       hidden (or, with `echo`, echoing) prompt; faillock is pam_faillock deny=3 (Debian: the
#       d4b-a probe's lines with `[default=die]`; Fedora: authselect's with-faillock)
#   --tally              make walker's faillock tally file as a login does (su from `other`)
#   --expect-tally N     after the test, faillock must hold exactly N failures for walker
#   --empty-password | --locked-password      passwd -d / passwd -l walker
#   --no-libpam          move libpam.so.0 out of the library path
#   --no-pam-service     remove every service a check may use
#   --no-run-user        no /run/user/<uid> (a container, or su without logind)
#   --no-marker | --root-marker    no marker, or one root owns
#   --machine-id absent|empty|<32 hex>
#
# A case passes only when its one test reported `ok`, the harness ran exactly one test, and the
# driver's own checks held; the cases here must be exactly the tests in the binary, so a renamed
# test cannot turn into an empty, passing run.
set -euo pipefail

REPO_ROOT="$(git rev-parse --show-toplevel)"
# The account's password: a throwaway account in a throwaway container.
PASSWORD=walker-os-slots-test-1
TEST=pam_container
DISTROS=(debian fedora)
# Each case: the test's name, then the driver's flags for the system it needs.
CASES=(
    "the_right_password_is_verified_and_not_counted"
    "a_wrong_password_is_wrong_and_the_next_run_is_refused_before_pam"
    "a_stack_that_asks_for_nothing_is_not_checked_and_not_counted --stack permit"
    "a_fingerprint_module_before_the_password_is_not_checked --stack fingerprint"
    "a_second_hidden_prompt_is_not_checked_and_counts --stack second-prompt"
    "an_echoing_prompt_is_not_checked --stack echo-prompt"
    "an_empty_entry_never_reaches_pam --stack permit"
    "an_account_without_a_password_reads_empty_and_is_never_verified --empty-password"
    "a_locked_password_reads_locked_and_is_wrong --locked-password"
    "without_libpam_there_is_no_check --no-libpam"
    "without_a_pam_service_there_is_no_check --no-pam-service"
    "without_a_runtime_directory_there_is_no_back_off_record --no-run-user --stack permit"
    "an_unmarked_account_is_fenced --no-marker"
    "a_marker_another_account_owns_is_no_marker --root-marker"
    "a_failure_counts_toward_faillock_once_its_tally_exists --stack faillock --tally --expect-tally 1"
    "without_a_tally_file_faillock_records_no_failure --stack faillock --expect-tally 0"
    "the_machine_id_is_its_32_characters --machine-id 0123456789abcdef0123456789abcdef"
    "an_empty_machine_id_is_none --machine-id empty"
    "a_missing_machine_id_is_none --machine-id absent"
    "this_container_is_seen_as_one"
)

die() {
    echo "test-os-slots-linux: $*" >&2
    exit 1
}

read -ra podman <<<"${PODMAN:-podman}"
read -ra cargo <<<"${CARGO_CMD:-cargo}"
command -v "${podman[0]}" >/dev/null ||
    die "${podman[0]} is not installed: the cases cannot run, and they are not skipped"
command -v "${cargo[0]}" >/dev/null || die "${cargo[0]} is not on PATH"

# 1. Which distributions and cases: the arguments, or all of them.
if [[ ${1:-} == --distro ]]; then
    [[ ${2:-} == debian || ${2:-} == fedora ]] || die "--distro takes debian or fedora"
    DISTROS=("$2")
    shift 2
fi
selected=()
if [[ $# -eq 0 ]]; then
    selected=("${CASES[@]}")
else
    for want in "$@"; do
        found=""
        for c in "${CASES[@]}"; do
            read -ra words <<<"$c"
            [[ "${words[0]}" == "$want" ]] && found="$c"
        done
        [[ -n "$found" ]] || die "no case named $want"
        selected+=("$found")
    done
fi

work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT

# 2. The test binary, built on the host.
echo "==> building cli/os-slots/tests/$TEST.rs"
if ! (cd "$REPO_ROOT/cli" &&
    "${cargo[@]}" test --locked --color never -p apprafter-os-slots --test "$TEST" --no-run) \
    >"$work/build.log" 2>&1; then
    cat "$work/build.log" >&2
    die "building the test failed"
fi
bin="$(sed -n "s/^ *Executable tests\/$TEST\.rs (\(.*\))\$/\1/p" "$work/build.log")"
[[ -n "$bin" ]] || {
    cat "$work/build.log" >&2
    die "cargo did not name the $TEST binary"
}
[[ "$bin" == /* ]] || bin="$REPO_ROOT/cli/$bin"

# 3. The cases here are exactly the binary's tests.
listed="$("$bin" --list --ignored | sed -n 's/: test$//p' | sort)"
known="$(for c in "${CASES[@]}"; do read -ra words <<<"$c"; echo "${words[0]}"; done | sort)"
if [[ "$listed" != "$known" ]]; then
    printf 'tests in the binary:\n%s\ncases in this script:\n%s\n' "$listed" "$known" >&2
    die "the cases here and the tests in the binary differ"
fi

# 4. The images: a test PAM module, the driver, and per distribution its PAM, a walker with a
#    known password, a second account `other` (it logs in as walker for --tally).
cat >"$work/pam_twoprompt.c" <<'C_EOF'
/* A test-only PAM module for scripts/test-os-slots-linux.sh: asks one more prompt — hidden, or
 * echoing with the argument `echo` — and succeeds when it was answered. Built into the
 * throwaway test images only. */
#define PAM_SM_AUTH
#include <security/pam_ext.h>
#include <security/pam_modules.h>
#include <stdlib.h>
#include <string.h>

int pam_sm_authenticate(pam_handle_t *pamh, int flags, int argc, const char **argv) {
    (void)flags;
    int echo = argc > 0 && strcmp(argv[0], "echo") == 0;
    char *answer = NULL;
    int rc = pam_prompt(pamh, echo ? PAM_PROMPT_ECHO_ON : PAM_PROMPT_ECHO_OFF, &answer, "%s",
                        echo ? "One-time code: " : "Second password: ");
    if (answer != NULL) {
        memset(answer, 0, strlen(answer));
        free(answer);
    }
    return rc == PAM_SUCCESS ? PAM_SUCCESS : PAM_AUTH_ERR;
}

int pam_sm_setcred(pam_handle_t *pamh, int flags, int argc, const char **argv) {
    (void)pamh;
    (void)flags;
    (void)argc;
    (void)argv;
    return PAM_SUCCESS;
}
C_EOF
cat >"$work/os-slots-case" <<'DRIVER_EOF'
#!/bin/bash
# Runs one case as the container's PID 1: prepares the system the flags ask for, runs that one
# test as walker, then the driver's own checks.
#   os-slots-case [flags] <test name>
set -euo pipefail
stack=distro tally=no expect_tally="" password=set libpam=yes service=yes run_user=yes
marker=walker machine_id=keep
while [[ $# -gt 1 ]]; do
    case "$1" in
    --stack) stack="$2"; shift ;;
    --tally) tally=yes ;;
    --expect-tally) expect_tally="$2"; shift ;;
    --empty-password) password=empty ;;
    --locked-password) password=locked ;;
    --no-libpam) libpam=no ;;
    --no-pam-service) service=no ;;
    --no-run-user) run_user=no ;;
    --no-marker) marker=none ;;
    --root-marker) marker=root ;;
    --machine-id) machine_id="$2"; shift ;;
    *) echo "os-slots-case: unknown flag $1" >&2; exit 2 ;;
    esac
    shift
done
name="$1"
# DISTRO, SERVICE (the service a check uses), LIBDIR.
. /etc/os-slots-distro
uid="$(id -u walker)" gid="$(id -g walker)"

write_stack() {
    rm -f "/etc/pam.d/$SERVICE"
    printf '%s\n' "$@" >"/etc/pam.d/$SERVICE"
}
case "$stack" in
distro) ;;
permit) write_stack 'auth sufficient pam_permit.so' ;;
fingerprint) write_stack 'auth sufficient pam_permit.so' 'auth required pam_unix.so' ;;
second-prompt) write_stack 'auth required pam_unix.so' 'auth required pam_twoprompt.so' ;;
echo-prompt) write_stack 'auth required pam_unix.so' 'auth required pam_twoprompt.so echo' ;;
faillock)
    mkdir -p /run/faillock
    if [[ $DISTRO == fedora ]]; then
        authselect select local with-faillock --force >/dev/null
    else
        write_stack \
            'auth    required                        pam_faillock.so preauth deny=3 unlock_time=600' \
            'auth    [success=1 default=ignore]      pam_unix.so nullok' \
            'auth    [default=die]                   pam_faillock.so authfail deny=3 unlock_time=600' \
            'auth    sufficient                      pam_faillock.so authsucc deny=3 unlock_time=600' \
            'auth    requisite                       pam_deny.so' \
            'auth    required                        pam_permit.so'
    fi
    ;;
*) echo "os-slots-case: unknown stack $stack" >&2; exit 2 ;;
esac
if [[ $tally == yes ]]; then
    # A root-side check for walker creates its tally file (d4b-a's probe; GOTCHA-326).
    runuser -u other -- sh -c 'printf "%s\n" "$0" | su -c true walker' "$OS_SLOTS_PASSWORD" \
        >/dev/null 2>&1 || true
    [[ -e /run/faillock/walker ]] || { echo "os-slots-case: su left no tally for walker" >&2; exit 3; }
fi
case "$password" in
empty) passwd -d walker >/dev/null ;;
locked) passwd -l walker >/dev/null ;;
esac
if [[ $libpam == no ]]; then
    mkdir -p /opt/hidden
    mv "$LIBDIR"/libpam.so.0* /opt/hidden/
    ldconfig
fi
if [[ $service == no ]]; then
    for dir in /etc/pam.d /usr/lib/pam.d; do
        rm -f "$dir/common-auth" "$dir/system-auth" "$dir/login"
    done
fi
if [[ $run_user == yes ]]; then
    install -d -m 0700 -o "$uid" -g "$gid" "/run/user/$uid"
fi
case "$marker" in
walker) install -m 0600 -o "$uid" -g "$gid" /dev/null /home/walker/.apprafter-test-account ;;
root) install -m 0644 -o root -g root /dev/null /home/walker/.apprafter-test-account ;;
none) rm -f /home/walker/.apprafter-test-account ;;
esac
case "$machine_id" in
keep) ;;
absent) rm -f /etc/machine-id ;;
empty) : >/etc/machine-id ;;
*) printf '%s\n' "$machine_id" >/etc/machine-id ;;
esac

loaders=(/lib64/ld-linux-*.so.2)
status=0
setpriv --reuid=walker --regid=walker --init-groups --reset-env \
    env APPRAFTER_OS_SLOTS_CONTAINER=1 APPRAFTER_OS_SLOTS_PASSWORD="$OS_SLOTS_PASSWORD" \
    "${loaders[0]}" --library-path "$LIBDIR" \
    /opt/os-slots/pam_container --ignored --exact --nocapture "$name" || status=$?
if [[ $status -eq 0 && -n $expect_tally ]]; then
    count="$(faillock --user walker | tail -n +3 | grep -c . || true)"
    if [[ $count != "$expect_tally" ]]; then
        echo "os-slots-case: faillock holds $count failure(s) for walker, expected $expect_tally" >&2
        faillock --user walker >&2 || true
        status=4
    fi
fi
exit "$status"
DRIVER_EOF
cat >"$work/Containerfile.debian" <<EOF
FROM docker.io/library/debian:13-slim AS build
RUN apt-get update \\
 && DEBIAN_FRONTEND=noninteractive apt-get install -y --no-install-recommends gcc libc6-dev libpam0g-dev \\
 && rm -rf /var/lib/apt/lists/*
COPY pam_twoprompt.c /src/
RUN gcc -shared -fPIC -O1 -o /src/pam_twoprompt.so /src/pam_twoprompt.c -lpam

FROM docker.io/library/debian:13-slim
RUN apt-get update \\
 && DEBIAN_FRONTEND=noninteractive apt-get install -y --no-install-recommends \\
      passwd login util-linux libpam0g libpam-modules libpam-modules-bin libpam-runtime \\
 && rm -rf /var/lib/apt/lists/*
COPY --from=build /src/pam_twoprompt.so /usr/lib/x86_64-linux-gnu/security/pam_twoprompt.so
RUN useradd --create-home --shell /bin/bash walker && echo 'walker:$PASSWORD' | chpasswd \\
 && useradd --create-home --shell /bin/bash other \\
 && printf 'DISTRO=debian\nSERVICE=common-auth\nLIBDIR=/usr/lib/x86_64-linux-gnu\n' >/etc/os-slots-distro
COPY os-slots-case /usr/local/bin/os-slots-case
RUN chmod 0755 /usr/local/bin/os-slots-case
ENV OS_SLOTS_PASSWORD=$PASSWORD
EOF
cat >"$work/Containerfile.fedora" <<EOF
FROM registry.fedoraproject.org/fedora:42 AS build
RUN dnf -y -q install gcc pam-devel && dnf clean all
COPY pam_twoprompt.c /src/
RUN gcc -shared -fPIC -O1 -o /src/pam_twoprompt.so /src/pam_twoprompt.c -lpam

FROM registry.fedoraproject.org/fedora:42
RUN dnf -y -q install pam passwd shadow-utils authselect util-linux && dnf clean all \\
 && authselect select local --force >/dev/null
COPY --from=build /src/pam_twoprompt.so /usr/lib64/security/pam_twoprompt.so
RUN useradd --create-home walker && echo 'walker:$PASSWORD' | chpasswd \\
 && useradd --create-home other \\
 && printf 'DISTRO=fedora\nSERVICE=system-auth\nLIBDIR=/usr/lib64\n' >/etc/os-slots-distro
COPY os-slots-case /usr/local/bin/os-slots-case
RUN chmod 0755 /usr/local/bin/os-slots-case
ENV OS_SLOTS_PASSWORD=$PASSWORD
EOF
for distro in "${DISTROS[@]}"; do
    echo "==> building the $distro image"
    "${podman[@]}" build --quiet --tag "localhost/apprafter-os-slots-$distro:dev" \
        --file "$work/Containerfile.$distro" "$work" >"$work/image-$distro.log" 2>&1 || {
        cat "$work/image-$distro.log" >&2
        die "building the $distro image failed"
    }
done

# 5. The cases.
failed=()
for distro in "${DISTROS[@]}"; do
    for c in "${selected[@]}"; do
        read -ra words <<<"$c"
        name="${words[0]}"
        flags=("${words[@]:1}")
        log="$work/$distro-$name.log"
        if "${podman[@]}" run --rm --volume "$bin:/opt/os-slots/$TEST:ro" \
            "localhost/apprafter-os-slots-$distro:dev" os-slots-case "${flags[@]}" "$name" \
            >"$log" 2>&1 &&
            grep -qx "test $name ... ok" "$log" &&
            grep -q '^test result: ok\. 1 passed; 0 failed' "$log"; then
            echo "ok    $name ($distro)"
        else
            echo "FAIL  $name ($distro)"
            sed 's/^/    /' "$log"
            failed+=("$distro/$name")
        fi
    done
done

if [[ ${#failed[@]} -gt 0 ]]; then
    die "${#failed[@]} case(s) failed: ${failed[*]}"
fi
echo "all $((${#selected[@]} * ${#DISTROS[@]})) case(s) passed"
