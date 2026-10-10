#!/usr/bin/env bash
# SPDX-License-Identifier: FSL-1.1-Apache-2.0
#
# AppRafter Desktop's Linux packages, installed where users install them: each .deb into a fresh
# Debian container, each .rpm into a fresh Fedora container. In each one:
#   1. the package carries the polkit policy at /usr/share/polkit-1/actions/, owned by root and
#      readable by everyone (polkitd does not run as root), and declares the PAM library and
#      the polkit daemon among its dependencies;
#   2. the package manager installs it, resolving every dependency from the distribution;
#   3. the installed policy is byte for byte desktop/packaging/linux/dev.apprafter.desktop.policy;
#   4. a polkitd running on the container's own system bus registers both actions with
#      `implicit any: no`, `implicit inactive: no`, `implicit active: auth_self` (pkaction);
#   5. every library /usr/bin/apprafter-desktop links resolves (ldd), and the binary starts: with
#      APPRAFTER_DESKTOP_DATA_DIR set but empty it refuses to run, exit code 2, before it builds
#      the app or opens any window. ldd traces through the container's loader whatever the binary
#      names; the start proves the binary's own loader exists and every library loads with the
#      symbol versions it needs (glibc's included).
#
#   bash scripts/test-desktop-packages.sh <package.deb|package.rpm>...
#   PODMAN='sudo podman' bash scripts/test-desktop-packages.sh \
#       desktop/target/release/bundle/deb/*.deb desktop/target/release/bundle/rpm/*.rpm
#
# The packages come from `bun run tauri build --bundles deb,rpm` in desktop/, as CI's
# desktop-linux-packages job builds them. Build them on a distribution as old as the oldest one
# they must run on, never on NixOS: a binary built in a nix shell names nix's loader, and step 5
# fails on it, as it must.
#
# Needs podman and network access (the base images and the distributions' repositories); the
# host needs neither dpkg nor rpm, since the containers read the packages. A missing podman
# FAILS: this is a regression test, and skipping it would read as a pass. Nothing touches the
# host's polkit, D-Bus or packages: each container runs its own bus and polkitd, and is removed.

set -euo pipefail

REPO_ROOT="$(git rev-parse --show-toplevel)"
POLICY="$REPO_ROOT/desktop/packaging/linux/dev.apprafter.desktop.policy"
DEB_IMAGE=docker.io/library/debian:13-slim
RPM_IMAGE=docker.io/library/fedora:44

die() {
    echo "test-desktop-packages: $*" >&2
    exit 1
}

[[ $# -gt 0 ]] || die "name at least one .deb or .rpm"
read -ra podman <<<"${PODMAN:-podman}"
command -v "${podman[0]}" >/dev/null ||
    die "${podman[0]} is not installed: the packages cannot be checked, and they are not skipped"

work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT

cat >"$work/check-package" <<'DRIVER_EOF'
#!/bin/bash
# Runs in the container: check-package <deb|rpm> <package path> <expected policy path>
set -euo pipefail

kind="$1" package="$2" expected="$3"
POLICY_PATH=/usr/share/polkit-1/actions/dev.apprafter.desktop.policy
BINARY=/usr/bin/apprafter-desktop

fail() {
    echo "FAIL: $*" >&2
    exit 1
}
step() {
    echo "--- $*"
}

step "the policy is in the package, owned by root and readable by everyone"
case "$kind" in
deb)
    # -rw-r--r-- 0/0  1536 2026-10-09 12:00 usr/share/polkit-1/actions/...: tar's listing,
    # whose names may start with `./` and whose owners may be numbers.
    listing="$(dpkg-deb --contents "$package")"
    entry="$(awk -v path="${POLICY_PATH#/}" '{ name = $NF; sub(/^\.?\//, "", name) }
        name == path' <<<"$listing")"
    echo "$entry"
    [[ -n "$entry" ]] || fail "no $POLICY_PATH in the package"
    read -r mode owner _ <<<"$entry"
    [[ "$owner" == root/root || "$owner" == 0/0 ]] || fail "owned by $owner, not root"
    ;;
rpm)
    # -rw-r--r--    1 root     root     1234 Oct  9 12:00 /usr/share/polkit-1/actions/...
    listing="$(rpm --query --list --verbose --package "$package")"
    entry="$(awk -v path="$POLICY_PATH" '$NF == path' <<<"$listing")"
    echo "$entry"
    [[ -n "$entry" ]] || fail "no $POLICY_PATH in the package"
    read -r mode _ user group _ <<<"$entry"
    [[ "$user:$group" == root:root ]] || fail "owned by $user:$group, not root:root"
    ;;
*) fail "unknown package kind $kind" ;;
esac
[[ "$mode" == -??????r?? ]] || fail "mode $mode: polkitd, not root, cannot read it"
[[ "$mode" != -???????w? ]] || fail "mode $mode: anyone can rewrite the actions"

step "the package depends on the PAM library and recommends the polkit daemon"
case "$kind" in
deb)
    depends="$(dpkg-deb --field "$package" Depends)"
    recommends="$(dpkg-deb --field "$package" Recommends)"
    pam=libpam0g polkit=polkitd
    ;;
rpm)
    depends="$(rpm --query --requires --package "$package")"
    recommends="$(rpm --query --recommends --package "$package")"
    pam='libpam.so.0()(64bit)' polkit=polkit
    ;;
esac
echo "depends: $(tr '\n' ' ' <<<"$depends")"
echo "recommends: $(tr '\n' ' ' <<<"$recommends")"
# One name per entry, whether the list is comma- or newline-separated.
has() {
    tr ',' '\n' <<<"$1" | sed 's/^ *//; s/ *$//' | grep -qxF -- "$2"
}
has "$depends" "$pam" || fail "the package does not depend on $pam"
has "$recommends" "$polkit" || fail "the package does not recommend $polkit"

step "the package manager installs it"
# Weak dependencies (Recommends) stay out: the daemon the checks need is named, so the run says
# what a minimal install brings, and is faster. The output only when it fails.
case "$kind" in
deb)
    export DEBIAN_FRONTEND=noninteractive
    install_package() {
        apt-get update &&
            apt-get install -y --no-install-recommends \
                "$package" polkitd dbus-daemon dbus-bin
    }
    ;;
rpm)
    install_package() {
        dnf install -y --setopt=install_weak_deps=False \
            "$package" polkit dbus-daemon dbus-tools
    }
    ;;
esac
install_package >/tmp/install.log 2>&1 || {
    cat /tmp/install.log >&2
    fail "the package manager did not install $package"
}
[[ -x "$BINARY" ]] || fail "$BINARY is not installed"

step "the package is named after the binary, and the menu entry says AppRafter"
# Tauri names the package after productName, kebab-cased: "AppRafter" became "app-rafter".
# tauri.linux.conf.json5 sets it to the binary's name; the launcher keeps the product's name.
case "$kind" in
deb) dpkg-query --show apprafter-desktop >/dev/null 2>&1 || fail "no installed package apprafter-desktop" ;;
rpm) rpm --query apprafter-desktop >/dev/null 2>&1 || fail "no installed package apprafter-desktop" ;;
esac
LAUNCHER=/usr/share/applications/apprafter-desktop.desktop
[[ -f "$LAUNCHER" ]] || fail "$LAUNCHER is not installed"
grep -qx 'Name=AppRafter' "$LAUNCHER" || fail "$LAUNCHER does not name the app AppRafter"
grep -qx 'Exec=apprafter-desktop' "$LAUNCHER" || fail "$LAUNCHER does not start apprafter-desktop"

step "the installed policy is the repository's"
# sha256sum, not cmp: Fedora's image has no diffutils.
[[ "$(sha256sum <"$POLICY_PATH")" == "$(sha256sum <"$expected")" ]] ||
    fail "$POLICY_PATH differs from desktop/packaging/linux"

step "polkitd registers both actions, for the active session's own password only"
mkdir -p /run/dbus
dbus-daemon --system --fork
/usr/lib/polkit-1/polkitd >/tmp/polkitd.log 2>&1 &
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
    cat /tmp/polkitd.log >&2
    fail "polkitd did not take its bus name"
fi
pkaction --version
for action in dev.apprafter.desktop.unlock dev.apprafter.desktop.confirm; do
    # pkaction pads the values into a column: `  implicit active:   auth_self`.
    shown="$(pkaction --action-id "$action" --verbose)" ||
        fail "pkaction does not know $action"
    echo "$shown"
    for expect in "implicit any: no" "implicit inactive: no" "implicit active: auth_self"; do
        tr -s ' ' <<<"$shown" | grep -qxF " $expect" || fail "$action: no '$expect'"
    done
done

step "every library $BINARY links resolves"
linked="$(ldd "$BINARY" 2>&1)" || {
    echo "$linked" >&2
    fail "ldd cannot read $BINARY"
}
if grep -F 'not found' <<<"$linked" >&2; then
    fail "$BINARY links libraries this system does not have"
fi
echo "$(wc -l <<<"$linked") libraries resolve"

step "$BINARY starts, and refuses an empty data directory before any window"
said="$(APPRAFTER_DESKTOP_DATA_DIR='' "$BINARY" 2>&1)" && code=0 || code=$?
echo "exit code $code: $said"
[[ $code -eq 2 && "$said" == *"APPRAFTER_DESKTOP_DATA_DIR is set but empty"* ]] ||
    fail "$BINARY did not start (a binary built in a nix shell names nix's loader)"
echo "PASS"
DRIVER_EOF
chmod 0755 "$work/check-package"

failed=()
for package in "$@"; do
    [[ -f "$package" ]] || die "no package at $package"
    case "$package" in
    *.deb) kind=deb image="$DEB_IMAGE" ;;
    *.rpm) kind=rpm image="$RPM_IMAGE" ;;
    *) die "$package is neither a .deb nor an .rpm" ;;
    esac
    name="$(basename "$package")"
    echo "==> $name in $image"
    if "${podman[@]}" run --rm \
        --volume "$(realpath "$package"):/check/$name:ro" \
        --volume "$POLICY:/check/expected.policy:ro" \
        --volume "$work/check-package:/usr/local/bin/check-package:ro" \
        "$image" check-package "$kind" "/check/$name" /check/expected.policy \
        >"$work/$name.log" 2>&1 &&
        [[ "$(tail -n 1 "$work/$name.log")" == PASS ]]; then
        sed 's/^/    /' "$work/$name.log"
        echo "ok    $name"
    else
        sed 's/^/    /' "$work/$name.log"
        echo "FAIL  $name"
        failed+=("$name")
    fi
done

if [[ ${#failed[@]} -gt 0 ]]; then
    die "${#failed[@]} of $# package(s) failed: ${failed[*]}"
fi
echo "all $# package(s) passed"
