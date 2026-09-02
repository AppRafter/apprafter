#!/usr/bin/env bash
# SPDX-License-Identifier: FSL-1.1-Apache-2.0
#
# check-backup-runner-pin.sh — fail if the backup runner has changed since the
# image tag the chart pins, so the pin cannot silently rot.
#
# ## Why
#
# The scheduled off-site backup runs ONE image, named by a hard-coded literal in
# `platform-stack/cue/platform.cue` (`#BackupValues.image`). Nothing derives it:
# not the chart appVersion, not `currentVersion`, not a channel tag. It is the
# only thing that decides which published runner a cluster actually executes.
#
# `release-backup-runner.yml` publishes a new `apprafter-backup/v<version>` tag
# and image on every master push touching `cli/apprafter-backup/**` or
# `cli/backup-core/**`. Publishing, however, does not point the chart at what it
# published — that is a separate edit, and there was no gate on making it.
#
# So it rotted. The pin sat at v0.2.33 from 2026-07-17 (70acef4, which shipped
# as chart 0.2.42) to 2026-09-02: fifteen runner images were published past it
# (v0.2.34..v0.2.53) and eighteen chart versions shipped without moving it
# (0.2.43..0.2.60). In that window `cli/backup-core/src/extract.rs` gained
# persistent-Redis extraction (T12, 5cec8c6), which the
# release notes and docs/status.md both recorded as delivered. It was delivered
# — to the CLI binary, which runs `apprafter backup` from a laptop. The nightly
# CronJob went on running a binary from before it existed.
#
# That is the failure mode worth naming: a stale image is a WORKING image. It
# starts, it exits zero, it writes a snapshot. It just quietly does less than
# the version you think you shipped, and every green walk agrees with it.
#
# ## What it checks
#
# The pinned tag must be published, and no runner source may have changed since
# it. A newer published tag with an IDENTICAL runner is deliberately NOT an
# error: the runner's version comes from `cli/Cargo.toml`, so an unrelated CLI
# bump republishes a byte-identical binary under a new number, and demanding a
# chart release for that would be noise the next person learns to skip.
#
# Usage: check-backup-runner-pin.sh [remote]   (default: origin)
# In CI: needs full history + tags (actions/checkout fetch-depth: 0).

set -euo pipefail

SOURCE="platform-stack/cue/platform.cue"
REMOTE="${1:-origin}"

# The pinned reference, e.g. ghcr.io/apprafter/apprafter-backup:v0.2.53 — the
# CUE default on `#BackupValues.image`.
pinned_ref="$(sed -n 's/.*apprafter-backup:\(v[0-9][^"]*\)".*/\1/p' "$SOURCE" | head -1)"
if [[ -z "${pinned_ref:-}" ]]; then
    echo "::error::could not read the runner image pin from $SOURCE" >&2
    exit 2
fi
version="${pinned_ref#v}"
tag="apprafter-backup/v${version}"

# Runner source: the binary crate and the engine crate it shares with the CLI.
# `cli/Cargo.toml` is deliberately NOT here — it triggers a publish but does not
# change behaviour, and treating it as a source change is what would make this
# check noisy enough to be ignored.
paths=(cli/apprafter-backup cli/backup-core)

if ! git ls-remote --tags --exit-code "$REMOTE" "refs/tags/${tag}" >/dev/null 2>&1; then
    cat >&2 <<EOF
::error::The chart pins runner image ${pinned_ref}, but ${tag} is not on ${REMOTE}.
A cluster rendering this chart would try to pull an image that was never
published, and the backup CronJob would sit in ImagePullBackOff — visible only
to whoever reads pod status at 03:00.

Publish the runner first (push the runner change; release-backup-runner.yml tags
and pushes the image), then move the pin to what it published.
EOF
    exit 1
fi

if ! git rev-parse --verify --quiet "refs/tags/${tag}" >/dev/null; then
    git fetch --quiet "$REMOTE" "refs/tags/${tag}:refs/tags/${tag}" 2>/dev/null || {
        echo "::warning::could not fetch ${tag} for the diff — skipping the check." >&2
        exit 0
    }
fi

if git diff --quiet "refs/tags/${tag}..HEAD" -- "${paths[@]}"; then
    echo "OK: the backup runner is unchanged since ${tag}, which the chart pins."
    exit 0
fi

changed="$(git diff --name-only "refs/tags/${tag}..HEAD" -- "${paths[@]}" | head -20)"
latest="$(git ls-remote --tags "$REMOTE" 'refs/tags/apprafter-backup/v*' \
    | sed 's|.*refs/tags/apprafter-backup/||' | sort -V | tail -1)"
cat >&2 <<EOF
::error::The backup runner changed since ${tag}, which is what the chart pins.
Clusters would run a runner OLDER than this tree — and it would look fine,
because a stale runner still starts, still exits zero, and still writes a
snapshot. It just does less than the version you believe you shipped.

Move the \`#BackupValues.image\` default in ${SOURCE} to the newest published
runner (currently ${latest:-unknown}), bump platform-stack \`currentVersion\`
with a compatibility entry, and release the chart.

Runner source changed since ${tag}:
${changed}
EOF
exit 1
