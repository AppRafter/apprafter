#!/usr/bin/env bash
# SPDX-License-Identifier: FSL-1.1-Apache-2.0
#
# AppRafter Desktop's polkit backend (desktop/os-auth/src/linux/polkit.rs) against a real
# polkitd: every test of desktop/os-auth/tests/polkit_container.rs, each in a fresh podman
# container of Debian stable with dbus, polkitd, the app's policy file and a user `walk`.
#
#   bash scripts/test-osauth-linux.sh [case...]                # every case by default
#   PODMAN='sudo podman' bash scripts/test-osauth-linux.sh     # rootful, as CI runs it
#
# Needs cargo (the desktop toolchain; cli-providers' build script runs cue) and podman. A
# missing podman FAILS: this is a regression test, and skipping it would read as a pass.
# Nothing touches the host's polkit or D-Bus: each container runs its own bus and polkitd.
#
# The test binary is built here, on the host, and mounted read-only. CI builds it on
# ubuntu-24.04 (glibc 2.39), which runs on Debian 13's newer glibc; a Nix-built binary loads
# its glibc from /nix/store, mounted read-only when the host has one.
#
# The session: polkit applies `allow_active` only to a subject in an active local session, and
# looks that up through sd-login: the process's cgroup (`.../session-<id>.scope`) and the
# files logind keeps under /run/systemd/. The container runs no systemd, so the in-container
# driver writes those two files and moves the test into such a cgroup. That takes a writable
# cgroup tree: `--security-opt unmask=/sys/fs/cgroup`, plus, rootless, a cgroup delegated by a
# systemd user session. CI has no such session, so it runs rootful.
#
# A case passes only when its one test reported `ok` and the harness ran exactly one test, and
# the cases here must be exactly the tests in the binary, so a renamed test cannot turn into
# an empty, passing run.

set -euo pipefail

REPO_ROOT="$(git rev-parse --show-toplevel)"
IMAGE=localhost/apprafter-osauth-polkit:dev
BASE_IMAGE=docker.io/library/debian:13-slim
POLICY="$REPO_ROOT/desktop/packaging/linux/dev.apprafter.desktop.policy"
# The container user's password: a throwaway account in a throwaway container.
PASSWORD=walk-osauth-test
# The logind session id the driver fakes for `walk`.
SESSION=c1

# Each case: the test's name, then the driver's flags for the system it needs.
CASES=(
    "the_right_password_is_verified_and_asked_for_every_time"
    "a_wrong_password_fails"
    "a_dismissed_dialog_is_cancelled_by_the_user"
    "without_an_agent_nobody_can_ask"
    "without_the_policy_file_the_action_is_missing --no-policy"
    "a_rule_that_grants_without_asking_is_refused --rule-yes"
    "outside_an_active_local_session_it_is_not_permitted --no-session"
    "a_dialog_the_app_cancels_is_cancelled_by_the_app"
)

die() {
    echo "test-osauth-linux: $*" >&2
    exit 1
}

read -ra podman <<<"${PODMAN:-podman}"
command -v "${podman[0]}" >/dev/null ||
    die "${podman[0]} is not installed: the polkit cases cannot run, and they are not skipped"
command -v cargo >/dev/null || die "cargo is not on PATH"

work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT

# 1. The test binary, built on the host.
echo "==> building desktop/os-auth/tests/polkit_container.rs"
if ! (cd "$REPO_ROOT/desktop" &&
    cargo test --locked --color never -p apprafter-os-auth --test polkit_container --no-run) \
    >"$work/build.log" 2>&1; then
    cat "$work/build.log" >&2
    die "building the test failed"
fi
bin="$(sed -n 's/^ *Executable tests\/polkit_container\.rs (\(.*\))$/\1/p' "$work/build.log")"
[[ -n "$bin" ]] || {
    cat "$work/build.log" >&2
    die "cargo did not name the test binary"
}
[[ "$bin" == /* ]] || bin="$REPO_ROOT/desktop/$bin"

# 2. The cases here are exactly the binary's tests.
mapfile -t listed < <("$bin" --list --ignored | sed -n 's/: test$//p' | sort)
mapfile -t known < <(for c in "${CASES[@]}"; do echo "${c%% *}"; done | sort)
if [[ "${listed[*]}" != "${known[*]}" ]]; then
    echo "tests in the binary: ${listed[*]}" >&2
    echo "cases in this script: ${known[*]}" >&2
    die "the cases here and the tests in polkit_container.rs differ"
fi

# 3. Which cases to run: the arguments, or all of them.
selected=()
if [[ $# -eq 0 ]]; then
    selected=("${CASES[@]}")
else
    for want in "$@"; do
        found=""
        for c in "${CASES[@]}"; do
            [[ "${c%% *}" == "$want" ]] && found="$c"
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
set -euo pipefail

policy=yes rule=no session=yes
while [[ $# -gt 1 ]]; do
    case "$1" in
    --no-policy) policy=no ;;
    --rule-yes) rule=yes ;;
    --no-session) session=no ;;
    *)
        echo "osauth-case: unknown flag $1" >&2
        exit 2
        ;;
    esac
    shift
done
name="$1"

if [[ $policy == no ]]; then
    rm /usr/share/polkit-1/actions/dev.apprafter.desktop.policy
fi
if [[ $rule == yes ]]; then
    cat >/etc/polkit-1/rules.d/00-apprafter-test-yes.rules <<'RULE_EOF'
polkit.addRule(function (action, subject) {
    if (action.id.indexOf("dev.apprafter.desktop.") === 0) {
        return polkit.Result.YES;
    }
});
RULE_EOF
    chmod 0644 /etc/polkit-1/rules.d/00-apprafter-test-yes.rules
fi

# What logind would keep for an active local session of `walk` on seat0: sd-login reads the
# session's seat and activity from these files, and finds a process's session from its cgroup.
uid="$(id -u walk)"
mkdir -p /run/systemd/sessions /run/systemd/users /run/systemd/seats
printf '%s\n' "UID=$uid" USER=walk ACTIVE=1 IS_DISPLAY=0 STATE=active REMOTE=0 TYPE=tty \
    ORIGINAL_TYPE=tty CLASS=user "SCOPE=session-$OSAUTH_SESSION.scope" SEAT=seat0 \
    >"/run/systemd/sessions/$OSAUTH_SESSION"
printf '%s\n' NAME=walk STATE=active STOPPING=no "SESSIONS=$OSAUTH_SESSION" SEATS=seat0 \
    "ACTIVE_SESSIONS=$OSAUTH_SESSION" "ONLINE_SESSIONS=$OSAUTH_SESSION" ACTIVE_SEATS=seat0 \
    ONLINE_SEATS=seat0 >"/run/systemd/users/$uid"
scope="/sys/fs/cgroup/user.slice/user-$uid.slice/session-$OSAUTH_SESSION.scope"
if ! mkdir -p "$scope"; then
    echo "osauth-case: cannot create $scope: the cgroup tree is read-only or not delegated" \
        "(rootless podman without a systemd user session); run with PODMAN='sudo podman'" >&2
    exit 3
fi

mkdir -p /run/dbus
dbus-daemon --system --fork
/usr/lib/polkit-1/polkitd --log-level=info >/tmp/polkitd.log 2>&1 &
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

status=0
(
    # Only the test joins the session. sd-login reads a process's cgroup relative to PID 1's,
    # so moving this script (PID 1) there too would put every process back at the root.
    if [[ $session == yes ]]; then
        echo "$BASHPID" >"$scope/cgroup.procs"
    fi
    exec setpriv --reuid=walk --regid=walk --init-groups --reset-env \
        env APPRAFTER_OSAUTH_CONTAINER=1 APPRAFTER_OSAUTH_PASSWORD="$OSAUTH_PASSWORD" \
        APPRAFTER_OSAUTH_SESSION="$OSAUTH_SESSION" \
        /opt/osauth/polkit_container --ignored --exact --nocapture "$name"
) || status=$?
if [[ $status -ne 0 ]]; then
    echo "--- polkitd log"
    cat /tmp/polkitd.log
fi
exit "$status"
DRIVER_EOF
cat >"$work/Containerfile" <<EOF
FROM $BASE_IMAGE
RUN apt-get update \\
 && DEBIAN_FRONTEND=noninteractive apt-get install -y --no-install-recommends \\
      dbus-daemon dbus-bin polkitd \\
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
"${podman[@]}" run --rm "$IMAGE" sh -c 'echo "    $(pkaction --version), $(dbus-daemon --version | head -1)"'

# 5. The cases.
mounts=(--volume "$bin:/opt/osauth/polkit_container:ro")
if [[ -d /nix/store ]]; then
    mounts+=(--volume /nix/store:/nix/store:ro)
fi
failed=()
for c in "${selected[@]}"; do
    read -ra words <<<"$c"
    name="${words[0]}"
    flags=("${words[@]:1}")
    log="$work/$name.log"
    if "${podman[@]}" run --rm --security-opt unmask=/sys/fs/cgroup "${mounts[@]}" "$IMAGE" \
        osauth-case "${flags[@]}" "$name" >"$log" 2>&1 &&
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
    die "${#failed[@]} of ${#selected[@]} polkit case(s) failed: ${failed[*]}"
fi
echo "all ${#selected[@]} polkit case(s) passed"
