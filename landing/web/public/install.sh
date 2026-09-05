#!/bin/sh
# SPDX-FileCopyrightText: 2026 AppRafter contributors
# SPDX-License-Identifier: FSL-1.1-Apache-2.0
#
# Install the `apprafter` CLI. Served from https://apprafter.dev/install.sh
# (Astro copies `public/` verbatim, so this file IS the published URL).
#
#     curl -fsSL https://apprafter.dev/install.sh | sh
#
# Or, to read it before running it — which is the better habit and costs
# one extra line:
#
#     curl -fsSL https://apprafter.dev/install.sh -o install.sh
#     less install.sh && sh install.sh
#
# Environment:
#   APPRAFTER_VERSION      pin an exact tag (e.g. v0.2.60) instead of resolving
#   APPRAFTER_INSTALL_DIR  where to put the binary (default /usr/local/bin)
#   GITHUB_TOKEN           optional, only to raise the anonymous rate limit
#
# WHY THIS RESOLVES THE VERSION THE LONG WAY:
# this is a monorepo and five workflows publish GitHub Releases into it
# (`v0.*` for the CLI, plus `platform-stack/v*`, `operator/v*`,
# `apprafter-backup/v*` and `argocd-cue-cmp/v*`). GitHub's
# `/releases/latest` returns the newest non-prerelease across ALL of them,
# so it can hand back `operator/v0.1.134` — and the download URL built
# from that 404s. The CLI itself learned this the hard way; see
# `pick_canonical_cli_tag` in cli/platform-cli/src/commands/version_check.rs.
#
# The filter below is deliberately STRICTER than that Rust function. It
# strips a leading `v` and accepts semver, which also accepts a
# prerelease — and `v0.1.0-mvp` is a real tag in this repository, matched
# by the `v0.*` trigger, with real assets. A human running an installer
# wants the newest STABLE release, so this requires an exact
# `vMAJOR.MINOR.PATCH`.

set -eu

REPO="AppRafter/apprafter"
INSTALL_DIR="${APPRAFTER_INSTALL_DIR:-/usr/local/bin}"

die() {
    printf 'apprafter install: %s\n' "$1" >&2
    exit 1
}

note() {
    printf '%s\n' "$1" >&2
}

command -v curl >/dev/null 2>&1 || die "curl is required and was not found on PATH"
command -v tar >/dev/null 2>&1 || die "tar is required and was not found on PATH"

# --- 1. Which build? -------------------------------------------------------
# Three targets are published (see .github/workflows/release-cli.yml). Linux
# aarch64 is deliberately absent, and saying so beats handing someone a URL
# that 404s.
os="$(uname -s)"
arch="$(uname -m)"

case "$os/$arch" in
    Linux/x86_64 | Linux/amd64)
        TARGET=x86_64-unknown-linux-gnu
        ;;
    Darwin/x86_64)
        TARGET=x86_64-apple-darwin
        ;;
    Darwin/arm64 | Darwin/aarch64)
        TARGET=aarch64-apple-darwin
        ;;
    Linux/aarch64 | Linux/arm64)
        die "Linux $arch has no published build yet.
  Prebuilt targets are Linux x86_64, macOS x86_64 and macOS arm64.
  For an ARM server, build from source:
    git clone https://github.com/$REPO && cd apprafter/cli
    cargo build --release --locked   # binary at cli/target/release/apprafter"
        ;;
    *)
        die "unsupported platform $os/$arch.
  Prebuilt targets are Linux x86_64, macOS x86_64 and macOS arm64.
  See https://apprafter.dev/download/ for the full list."
        ;;
esac

# --- 2. Which version? -----------------------------------------------------
if [ -n "${APPRAFTER_VERSION:-}" ]; then
    VERSION="$APPRAFTER_VERSION"
    note "apprafter install: using pinned version $VERSION"
else
    note "apprafter install: resolving the newest CLI release..."
    auth=""
    if [ -n "${GITHUB_TOKEN:-}" ]; then
        auth="Authorization: Bearer $GITHUB_TOKEN"
    fi

    # 30 is generous: the CLI series is the busiest of the five, so even a
    # burst of chart releases cannot push the newest CLI tag off the page.
    if [ -n "$auth" ]; then
        releases="$(curl -fsSL -H "$auth" \
            "https://api.github.com/repos/$REPO/releases?per_page=30")" \
            || die "could not reach the GitHub releases API.
  Pin a version instead:  APPRAFTER_VERSION=v0.2.60 sh install.sh
  Or download by hand:    https://apprafter.dev/download/"
    else
        releases="$(curl -fsSL \
            "https://api.github.com/repos/$REPO/releases?per_page=30")" \
            || die "could not reach the GitHub releases API.
  If this is a rate limit, set GITHUB_TOKEN and retry.
  Pin a version instead:  APPRAFTER_VERSION=v0.2.60 sh install.sh
  Or download by hand:    https://apprafter.dev/download/"
    fi

    # Releases come back newest-first, so the first tag that survives the
    # filter is the newest CLI release. `vMAJOR.MINOR.PATCH` exactly: no
    # prefix (that is another series) and no suffix (that is a milestone
    # tag like v0.1.0-mvp).
    VERSION="$(printf '%s\n' "$releases" \
        | sed -n 's/.*"tag_name"[[:space:]]*:[[:space:]]*"\([^"]*\)".*/\1/p' \
        | grep -E '^v[0-9]+\.[0-9]+\.[0-9]+$' \
        | head -n 1)"

    [ -n "$VERSION" ] || die "no CLI release found in the 30 most recent releases.
  This repository publishes five tag series and only 'vX.Y.Z' is the CLI.
  Pick one by hand at https://github.com/$REPO/releases and re-run with
  APPRAFTER_VERSION=<tag>."
    note "apprafter install: newest CLI release is $VERSION"
fi

# --- 3. Download, verify, install ------------------------------------------
ARCHIVE="apprafter-${VERSION}-${TARGET}.tar.gz"
BASE="https://github.com/$REPO/releases/download/${VERSION}"

workdir="$(mktemp -d)"
# `set -eu` plus a trap: the temp directory goes away on every exit path,
# including the failures below.
trap 'rm -rf "$workdir"' EXIT
cd "$workdir"

curl -fsSL -o "$ARCHIVE" "$BASE/$ARCHIVE" \
    || die "could not download $ARCHIVE
  Tried: $BASE/$ARCHIVE
  If the version is pinned, check that the tag exists and publishes $TARGET."

curl -fsSL -o "$ARCHIVE.sha256" "$BASE/$ARCHIVE.sha256" \
    || die "downloaded $ARCHIVE but not its checksum ($ARCHIVE.sha256).
  Refusing to install an unverified binary. Every release publishes a
  .sha256 beside every tarball, so a missing one is a signal, not noise."

# The sidecar records the bare basename, so `-c` works from this directory.
if command -v shasum >/dev/null 2>&1; then
    shasum -a 256 -c "$ARCHIVE.sha256" >/dev/null \
        || die "checksum MISMATCH for $ARCHIVE — nothing was installed."
elif command -v sha256sum >/dev/null 2>&1; then
    sha256sum -c "$ARCHIVE.sha256" >/dev/null \
        || die "checksum MISMATCH for $ARCHIVE — nothing was installed."
else
    # Not a silent pass: refusing is the only honest branch. A script that
    # says "verified" when it verified nothing is worse than one that stops.
    die "neither shasum nor sha256sum is available, so the download cannot
  be verified. Install one and re-run, or download and check by hand:
    $BASE/$ARCHIVE
    $BASE/$ARCHIVE.sha256"
fi
note "apprafter install: checksum OK"

tar xzf "$ARCHIVE" || die "could not unpack $ARCHIVE"
[ -f apprafter ] || die "$ARCHIVE did not contain an 'apprafter' binary"
chmod +x apprafter

if [ -w "$INSTALL_DIR" ]; then
    mv apprafter "$INSTALL_DIR/apprafter"
elif command -v sudo >/dev/null 2>&1; then
    note "apprafter install: $INSTALL_DIR is not writable, using sudo"
    sudo mv apprafter "$INSTALL_DIR/apprafter"
else
    die "$INSTALL_DIR is not writable and sudo is not available.
  Re-run with a writable location:
    APPRAFTER_INSTALL_DIR=\"\$HOME/.local/bin\" sh install.sh"
fi

note "apprafter install: installed $VERSION to $INSTALL_DIR/apprafter"

if command -v apprafter >/dev/null 2>&1; then
    apprafter --version
else
    note "apprafter install: $INSTALL_DIR is not on your PATH — add it:
    export PATH=\"$INSTALL_DIR:\$PATH\""
fi
