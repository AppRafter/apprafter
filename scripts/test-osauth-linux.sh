#!/usr/bin/env bash
# SPDX-License-Identifier: FSL-1.1-Apache-2.0
#
# AppRafter Desktop's Linux authentication (desktop/os-auth/src/linux/) against a real polkitd
# and a real PAM stack, and its session watch (desktop/os-auth/src/session/) against real D-Bus
# daemons: every test of desktop/os-auth/tests/{polkit,pam,session}_container.rs, each in a
# fresh podman container of Debian stable with dbus, polkitd, the app's policy file, Debian's
# PAM (pam_unix and its set-group-id unix_chkpwd) and a user `walk` with a known password.
#
#   bash scripts/test-osauth-linux.sh [case...]                # every case by default
#   PODMAN='sudo podman' bash scripts/test-osauth-linux.sh     # rootful, as CI runs it
#
# Needs cargo (the desktop toolchain; cli-providers' build script runs cue; linking needs
# libpam's development files) and podman. A missing podman FAILS: this is a regression test,
# and skipping it would read as a pass. Nothing touches the host's polkit, D-Bus or PAM: each
# container runs its own bus and polkitd and reads its own /etc/pam.d.
#
# The test binaries are built here, on the host, mounted read-only, and started through the
# container's own loader with the container's library directory first, so the libpam and the
# PAM modules they load are Debian's, as an installed app's would be the system's. A binary
# built in a nix shell names nix's loader, which never looks in the container's library
# directories. The container's glibc (Debian 13: 2.41) runs a binary built against a newer one
# as long as the binary uses no newer symbol; a build that does fails loudly here, it does not
# pass. CI builds on ubuntu-24.04 (glibc 2.39).
#
# The session: polkit applies `allow_active` only to a subject in an active local session, and
# looks that up through sd-login: the process's cgroup (`.../session-<id>.scope`) and the
# files logind keeps under /run/systemd/. The container runs no systemd, so the in-container
# driver writes those two files (active, or with --inactive-session inactive) and moves the
# test into such a cgroup (not with --no-session); the authenticator reads the same files to
# tell an active local session from the others. Moving it takes a writable cgroup tree:
# `--security-opt unmask=/sys/fs/cgroup`, plus, rootless, a cgroup delegated by a systemd user
# session. CI has no such session, so it runs rootful.
#
# The session watch's cases play logind and the screen savers themselves: --fake-logind lets
# `walk` own org.freedesktop.login1 on the container's system bus (there is no systemd), and
# --session-bus starts a session bus of `walk`'s and hands the test its address. --stopped-polkitd
# stops polkitd once it owns its name, so it never answers. --no-system-bus
# points the test's system bus at a socket that does not exist, as on a system without one (the
# container's own bus and polkitd still run).
#
# A case passes only when its one test reported `ok` and the harness ran exactly one test, and
# the cases here must be exactly the tests in the binaries, so a renamed test cannot turn into
# an empty, passing run.

set -euo pipefail

REPO_ROOT="$(git rev-parse --show-toplevel)"
IMAGE=localhost/apprafter-osauth-linux:dev
BASE_IMAGE=docker.io/library/debian:13-slim
POLICY="$REPO_ROOT/desktop/packaging/linux/dev.apprafter.desktop.policy"
# The container user's password: a throwaway account in a throwaway container.
PASSWORD=walk-osauth-test
# The logind session id the driver fakes for `walk`.
SESSION=c1

# The test binaries: desktop/os-auth/tests/<name>.rs.
TESTS=(polkit_container pam_container session_container)
# Each case: its test binary, the test's name (unique across the binaries), then the driver's
# flags for the system it needs.
CASES=(
    "polkit_container the_right_password_is_verified_and_asked_for_every_time"
    "polkit_container a_wrong_password_fails"
    "polkit_container a_dismissed_dialog_is_cancelled_by_the_user"
    "polkit_container without_an_agent_nobody_can_ask"
    "polkit_container without_the_policy_file_the_action_is_missing --no-policy"
    "polkit_container a_rule_that_grants_without_asking_is_refused --rule-yes"
    "polkit_container outside_an_active_local_session_it_is_not_permitted --no-session"
    "polkit_container a_dialog_the_app_cancels_is_cancelled_by_the_app"
    "polkit_container a_polkitd_that_does_not_answer_is_given_up_on --stopped-polkitd"
    "polkit_container an_unlock_against_a_hung_polkitd_ends_and_a_cancel_ends_it_at_once --stopped-polkitd"
    "pam_container the_right_password_is_verified"
    "pam_container wrong_passwords_fail_and_the_back_off_refuses_without_asking_pam"
    "pam_container without_a_service_file_there_is_no_pam_service --no-pam-service"
    "pam_container without_an_agent_the_authenticator_moves_to_the_password"
    "pam_container without_the_policy_file_the_authenticator_offers_the_password --no-policy"
    "pam_container an_empty_password_is_never_verified --empty-password"
    "pam_container an_administrator_s_no_in_an_active_session_is_final --rule-no"
    "pam_container outside_a_session_the_authenticator_offers_the_password --no-session"
    "pam_container in_an_inactive_session_the_authenticator_offers_the_password --inactive-session"
    "session_container each_source_reports_its_event --fake-logind --session-bus"
    "session_container a_dropped_watch_reports_nothing --fake-logind --session-bus"
    "session_container without_a_session_bus_logind_still_reports --fake-logind"
    "session_container without_a_system_bus_only_the_screen_savers_report --session-bus --no-system-bus"
    "session_container without_any_bus_nothing_listens --no-system-bus"
)

die() {
    echo "test-osauth-linux: $*" >&2
    exit 1
}

read -ra podman <<<"${PODMAN:-podman}"
command -v "${podman[0]}" >/dev/null ||
    die "${podman[0]} is not installed: the cases cannot run, and they are not skipped"
command -v cargo >/dev/null || die "cargo is not on PATH"

work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT

# 1. The test binaries, built on the host.
echo "==> building desktop/os-auth/tests/{$(IFS=,; echo "${TESTS[*]}")}.rs"
test_flags=()
for t in "${TESTS[@]}"; do
    test_flags+=(--test "$t")
done
if ! (cd "$REPO_ROOT/desktop" &&
    cargo test --locked --color never -p apprafter-os-auth "${test_flags[@]}" --no-run) \
    >"$work/build.log" 2>&1; then
    cat "$work/build.log" >&2
    die "building the tests failed"
fi
declare -A bins
for t in "${TESTS[@]}"; do
    bin="$(sed -n "s/^ *Executable tests\/$t\.rs (\(.*\))\$/\1/p" "$work/build.log")"
    [[ -n "$bin" ]] || {
        cat "$work/build.log" >&2
        die "cargo did not name the $t binary"
    }
    [[ "$bin" == /* ]] || bin="$REPO_ROOT/desktop/$bin"
    bins[$t]="$bin"
done

# 2. The cases here are exactly the binaries' tests.
mapfile -t listed < <(for t in "${TESTS[@]}"; do
    "${bins[$t]}" --list --ignored | sed -n "s/: test\$//p" | sed "s/^/$t /"
done | sort)
mapfile -t known < <(for c in "${CASES[@]}"; do
    read -ra words <<<"$c"
    echo "${words[0]} ${words[1]}"
done | sort)
if [[ "${listed[*]}" != "${known[*]}" ]]; then
    printf 'tests in the binaries:\n' >&2
    printf '    %s\n' "${listed[@]}" >&2
    printf 'cases in this script:\n' >&2
    printf '    %s\n' "${known[@]}" >&2
    die "the cases here and the tests in the binaries differ"
fi
mapfile -t names < <(for c in "${CASES[@]}"; do
    read -ra words <<<"$c"
    echo "${words[1]}"
done | sort | uniq -d)
[[ ${#names[@]} -eq 0 ]] || die "test names in more than one binary: ${names[*]}"

# 3. Which cases to run: the arguments (test names), or all of them.
selected=()
if [[ $# -eq 0 ]]; then
    selected=("${CASES[@]}")
else
    for want in "$@"; do
        found=""
        for c in "${CASES[@]}"; do
            read -ra words <<<"$c"
            [[ "${words[1]}" == "$want" ]] && found="$c"
        done
        [[ -n "$found" ]] || die "no case named $want"
        selected+=("$found")
    done
fi

# 4. The image.
cp "$POLICY" "$work/dev.apprafter.desktop.policy"
cat >"$work/osauth-case" <<'DRIVER_EOF'
#!/bin/bash
# Runs one case as the container's PID 1: prepares the system the flags ask for, starts the
# system bus and polkitd, and runs that one test as `walk`.
#
#   osauth-case [flags] <test binary> <test name>
set -euo pipefail

policy=yes rule=none session=yes active=yes pam_service=yes password=yes
fake_logind=no session_bus=no system_bus=yes stopped_polkitd=no
while [[ $# -gt 2 ]]; do
    case "$1" in
    --no-policy) policy=no ;;
    --rule-yes) rule=YES ;;
    --rule-no) rule=NO ;;
    --no-session) session=no ;;
    --inactive-session) active=no ;;
    --no-pam-service) pam_service=no ;;
    --empty-password) password=no ;;
    --fake-logind) fake_logind=yes ;;
    --session-bus) session_bus=yes ;;
    --no-system-bus) system_bus=no ;;
    --stopped-polkitd) stopped_polkitd=yes ;;
    *)
        echo "osauth-case: unknown flag $1" >&2
        exit 2
        ;;
    esac
    shift
done
binary="$1" name="$2"

if [[ $policy == no ]]; then
    rm /usr/share/polkit-1/actions/dev.apprafter.desktop.policy
fi
if [[ $pam_service == no ]]; then
    # Every service the PAM fallback probes for, from both directories libpam reads.
    for dir in /etc/pam.d /usr/lib/pam.d; do
        rm -f "$dir/common-auth" "$dir/system-auth" "$dir/login"
    done
fi
if [[ $password == no ]]; then
    # pam_unix with `nullok` (Debian's common-auth) verifies an account like this without a
    # prompt unless the caller passes PAM_DISALLOW_NULL_AUTHTOK.
    passwd --delete walk >/dev/null
    if [[ -n "$(getent shadow walk | cut -d: -f2)" ]]; then
        echo "osauth-case: walk still has a password" >&2
        exit 3
    fi
fi
if [[ $rule != none ]]; then
    # An administrator's rule for the app's actions: YES grants without asking, NO refuses.
    cat >/etc/polkit-1/rules.d/00-apprafter-test.rules <<RULE_EOF
polkit.addRule(function (action, subject) {
    if (action.id.indexOf("dev.apprafter.desktop.") === 0) {
        return polkit.Result.$rule;
    }
});
RULE_EOF
    chmod 0644 /etc/polkit-1/rules.d/00-apprafter-test.rules
fi

# What logind would keep for a local session of `walk` on seat0, active or (another session in
# the foreground of the seat) inactive: sd-login reads the session's seat and activity, and the
# user's state, from these files, and finds a process's session from its cgroup.
uid="$(id -u walk)"
if [[ $active == yes ]]; then
    flag=1 state=active active_sessions="$OSAUTH_SESSION" active_seats=seat0
else
    flag=0 state=online active_sessions="" active_seats=""
fi
mkdir -p /run/systemd/sessions /run/systemd/users /run/systemd/seats
printf '%s\n' "UID=$uid" USER=walk "ACTIVE=$flag" IS_DISPLAY=0 "STATE=$state" REMOTE=0 \
    TYPE=tty ORIGINAL_TYPE=tty CLASS=user "SCOPE=session-$OSAUTH_SESSION.scope" SEAT=seat0 \
    >"/run/systemd/sessions/$OSAUTH_SESSION"
printf '%s\n' NAME=walk "STATE=$state" STOPPING=no "SESSIONS=$OSAUTH_SESSION" SEATS=seat0 \
    "ACTIVE_SESSIONS=$active_sessions" "ONLINE_SESSIONS=$OSAUTH_SESSION" \
    "ACTIVE_SEATS=$active_seats" ONLINE_SEATS=seat0 >"/run/systemd/users/$uid"
scope="/sys/fs/cgroup/user.slice/user-$uid.slice/session-$OSAUTH_SESSION.scope"
if ! mkdir -p "$scope"; then
    echo "osauth-case: cannot create $scope: the cgroup tree is read-only or not delegated" \
        "(rootless podman without a systemd user session); run with PODMAN='sudo podman'" >&2
    exit 3
fi

if [[ $fake_logind == yes ]]; then
    # The test plays logind: `walk` may own its name on the system bus and be asked GetSession.
    mkdir -p /etc/dbus-1/system.d
    cat >/etc/dbus-1/system.d/apprafter-test-login1.conf <<'POLICY_EOF'
<!DOCTYPE busconfig PUBLIC "-//freedesktop//DTD D-BUS Bus Configuration 1.0//EN"
 "http://www.freedesktop.org/standards/dbus/1.0/busconfig.dtd">
<busconfig>
  <policy user="walk">
    <allow own="org.freedesktop.login1"/>
    <allow send_destination="org.freedesktop.login1"/>
  </policy>
</busconfig>
POLICY_EOF
fi

mkdir -p /run/dbus
dbus-daemon --system --fork
/usr/lib/polkit-1/polkitd --log-level=info >/tmp/polkitd.log 2>&1 &
polkitd=$!
ready=no
for _ in $(seq 100); do
    reply="$(dbus-send --system --print-reply --dest=org.freedesktop.DBus /org/freedesktop/DBus \
        org.freedesktop.DBus.NameHasOwner string:org.freedesktop.PolicyKit1 2>/dev/null || true)"
    if [[ "$reply" == *"boolean true"* ]]; then
        ready=yes
        break
    fi
    sleep 0.1
done
if [[ $ready != yes ]]; then
    echo "osauth-case: polkitd did not take its bus name" >&2
    cat /tmp/polkitd.log >&2
    exit 3
fi
if [[ $stopped_polkitd == yes ]]; then
    # polkitd keeps its name and stops answering: a hung polkitd.
    kill -STOP "$polkitd"
fi

# A session bus of `walk`'s, which only `walk` may connect to.
session_env=()
if [[ $session_bus == yes ]]; then
    address="$(setpriv --reuid=walk --regid=walk --init-groups --reset-env \
        dbus-daemon --session --fork --print-address=1)"
    if [[ $address != unix:* ]]; then
        echo "osauth-case: the session bus did not start (address '$address')" >&2
        exit 3
    fi
    session_env=("DBUS_SESSION_BUS_ADDRESS=$address")
fi
# The test's system bus, when the case wants none: a socket that does not exist.
system_env=()
if [[ $system_bus == no ]]; then
    system_env=("DBUS_SYSTEM_BUS_ADDRESS=unix:path=/nonexistent")
fi

# The container's loader and library directory (see the script's header).
shopt -s nullglob
loaders=(/lib64/ld-linux-*.so.2 /lib/ld-linux-*.so.1)
shopt -u nullglob
if [[ ${#loaders[@]} -eq 0 ]]; then
    echo "osauth-case: no dynamic loader in /lib64 or /lib" >&2
    exit 3
fi
libdir="/usr/lib/$(uname -m)-linux-gnu"

status=0
(
    # Only the test joins the session. sd-login reads a process's cgroup relative to PID 1's,
    # so moving this script (PID 1) there too would put every process back at the root.
    if [[ $session == yes ]]; then
        echo "$BASHPID" >"$scope/cgroup.procs"
    fi
    exec setpriv --reuid=walk --regid=walk --init-groups --reset-env \
        env APPRAFTER_OSAUTH_CONTAINER=1 APPRAFTER_OSAUTH_PASSWORD="$OSAUTH_PASSWORD" \
        APPRAFTER_OSAUTH_SESSION="$OSAUTH_SESSION" "${session_env[@]}" "${system_env[@]}" \
        "${loaders[0]}" --library-path "$libdir" \
        "/opt/osauth/$binary" --ignored --exact --nocapture "$name"
) || status=$?
if [[ $status -ne 0 ]]; then
    echo "--- polkitd log"
    cat /tmp/polkitd.log
fi
exit "$status"
DRIVER_EOF
cat >"$work/Containerfile" <<EOF
FROM $BASE_IMAGE
# libpam0g, libpam-modules (pam_unix), libpam-modules-bin (unix_chkpwd) and libpam-runtime
# (/etc/pam.d/common-auth) are already in the base image; naming them keeps it so.
RUN apt-get update \\
 && DEBIAN_FRONTEND=noninteractive apt-get install -y --no-install-recommends \\
      dbus-daemon dbus-bin dbus-session-bus-common polkitd \\
      libpam0g libpam-modules libpam-modules-bin libpam-runtime \\
 && rm -rf /var/lib/apt/lists/*
RUN useradd --create-home --shell /bin/bash walk \\
 && echo 'walk:$PASSWORD' | chpasswd
COPY dev.apprafter.desktop.policy /usr/share/polkit-1/actions/
COPY osauth-case /usr/local/bin/osauth-case
RUN chmod 0644 /usr/share/polkit-1/actions/dev.apprafter.desktop.policy \\
 && chmod 0755 /usr/local/bin/osauth-case
ENV OSAUTH_PASSWORD=$PASSWORD OSAUTH_SESSION=$SESSION
EOF
echo "==> building the image ($BASE_IMAGE)"
"${podman[@]}" build --quiet --tag "$IMAGE" "$work" >"$work/image.log" 2>&1 || {
    cat "$work/image.log" >&2
    die "building the image failed"
}
# shellcheck disable=SC2016 # expanded by the container's shell
"${podman[@]}" run --rm "$IMAGE" sh -c 'echo "    $(pkaction --version), $(dbus-daemon --version | head -1), libpam $(dpkg-query -W -f="\${Version}" libpam0g)"'

# 5. The cases.
mounts=()
for t in "${TESTS[@]}"; do
    mounts+=(--volume "${bins[$t]}:/opt/osauth/$t:ro")
done
failed=()
for c in "${selected[@]}"; do
    read -ra words <<<"$c"
    binary="${words[0]}"
    name="${words[1]}"
    flags=("${words[@]:2}")
    log="$work/$name.log"
    if "${podman[@]}" run --rm --security-opt unmask=/sys/fs/cgroup "${mounts[@]}" "$IMAGE" \
        osauth-case "${flags[@]}" "$binary" "$name" >"$log" 2>&1 &&
        grep -qx "test $name ... ok" "$log" &&
        grep -q '^test result: ok\. 1 passed; 0 failed' "$log"; then
        echo "ok    $name"
    else
        echo "FAIL  $name"
        sed 's/^/    /' "$log"
        failed+=("$name")
    fi
done

if [[ ${#failed[@]} -gt 0 ]]; then
    die "${#failed[@]} of ${#selected[@]} case(s) failed: ${failed[*]}"
fi
echo "all ${#selected[@]} case(s) passed"
