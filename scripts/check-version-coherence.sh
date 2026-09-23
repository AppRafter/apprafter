#!/usr/bin/env bash
# SPDX-License-Identifier: FSL-1.1-Apache-2.0
#
# check-version-coherence.sh — the version pins in this repo must agree with
# each other, and the chart must be re-published when one of them moves.
#
# ## Why
#
# Five things carry a version here, and four of them pin another: the operator
# and webhook charts declare an `appVersion`, `platform-stack` pins both by
# literal, `compatibility.cue` records which operator a chart version ships,
# and `argocd-cue-cmp/version.cue` is read by the chart at render time. Each
# had a guard for whether IT was bumped. None checked whether they still
# agreed, so the failure mode was always the same shape: something publishes,
# the pin does not move, and clusters keep running the previous image while
# every signal reports success.
#
# That has now happened twice — the backup runner sat six weeks behind its
# tree, and the cue-cmp sidecar would have done the same on the very next
# release. This is the guard for the class, not for another instance of it.
#
# ## What it checks
#
#   0. each operator chart's `version` == its own `appVersion`. The chart is
#      published under `version` (release-operator.yml packages it with
#      `helm package`, which names the artefact after Chart.yaml `version`),
#      while platform-stack pins it by that same string. A bump that moved
#      only `appVersion` (7b3f4f2, caught by the wave-1 upgrade walk on
#      2026-09-23) would re-publish the OLD chart version and never produce
#      the pinned one: Argo CD's `helm pull --version vX` then fails, the
#      operator and webhook Applications sit in ComparisonError on the old
#      images, and the root Application still reports Synced/Healthy.
#   1. platform-stack's operator pin == the operator chart's appVersion
#   2. platform-stack's webhook pin == the webhook chart's appVersion
#   3. the compatibility entry for `currentVersion` names that same operator
#   4. `argocd-cue-cmp/version.cue` has not moved since the published
#      `platform-stack/v<currentVersion>` — the chart READS that file, so a
#      sidecar bump that does not ride a chart bump never reaches a cluster
#
# 1-3 are local and need no network. 4 asks the remote whether the current
# chart version is already published; when it is not, the bump is in flight
# and there is nothing to check.
#
# Usage: check-version-coherence.sh [remote]   (default: origin)

set -euo pipefail

REPO_ROOT="$(git rev-parse --show-toplevel)"
cd "$REPO_ROOT"
REMOTE="${1:-origin}"

fail=0
note() { printf '  %-34s %s\n' "$1" "$2"; }
bad() {
    printf '::error::%s\n' "$1" >&2
    fail=1
}

# --- read every pin -------------------------------------------------------
# Text extraction rather than `cue export`: this runs in `just lint`, which
# must work in a fresh checkout with no dev shell. Each pattern is anchored to
# the one line in a file that carries the value.
read_yaml_field() { sed -n "s/^$2:[[:space:]]*\"\{0,1\}\([^\"]*\)\"\{0,1\}[[:space:]]*$/\1/p" "$1" | head -1; }
read_cue_field() { sed -n "s/^[[:space:]]*$2:[[:space:]]*\"\([^\"]*\)\".*/\1/p" "$1" | head -1; }

operator_app="$(read_yaml_field operator/charts/apprafter-operator/Chart.yaml appVersion)"
webhook_app="$(read_yaml_field operator/charts/apprafter-admission-webhook/Chart.yaml appVersion)"
operator_chart="$(read_yaml_field operator/charts/apprafter-operator/Chart.yaml version)"
webhook_chart="$(read_yaml_field operator/charts/apprafter-admission-webhook/Chart.yaml version)"
operator_pin="$(read_cue_field platform-stack/cue/component_apprafter-operator.cue version)"
webhook_pin="$(read_cue_field platform-stack/cue/component_admission-webhook.cue version)"
cuecmp_version="$(read_cue_field argocd-cue-cmp/version.cue version)"
current_version="$(sed -n 's/^currentVersion:[[:space:]]*#Version[[:space:]]*&[[:space:]]*"\([^"]*\)".*/\1/p' \
    platform-stack/cue/platform.cue | head -1)"

for v in operator_app webhook_app operator_chart webhook_chart operator_pin webhook_pin cuecmp_version current_version; do
    if [[ -z "${!v:-}" ]]; then
        echo "::error::could not read ${v} — a version literal moved or changed shape" >&2
        exit 2
    fi
done

# The operator recorded against THIS chart version. `compatibility.cue` is one
# block per version; read the `operatorVersion` inside the block for
# `currentVersion` specifically, not the first one in the file.
compat_operator="$(awk -v ver="\"${current_version}\"" '
    $0 ~ "^compatibility: " ver ": \\{" { inblock = 1; next }
    inblock && /^}/ { exit }
    inblock && /^[[:space:]]*operatorVersion:/ {
        gsub(/.*operatorVersion:[[:space:]]*"/, ""); gsub(/".*/, ""); print; exit
    }
' platform-stack/cue/compatibility.cue)"

echo "==> version pins"
note "operator chart version" "$operator_chart"
note "operator chart appVersion" "$operator_app"
note "webhook chart version" "$webhook_chart"
note "webhook chart appVersion" "$webhook_app"
note "platform-stack operator pin" "$operator_pin"
note "platform-stack webhook pin" "$webhook_pin"
note "platform-stack currentVersion" "$current_version"
note "compatibility operatorVersion" "${compat_operator:-<missing>}"
note "argocd-cue-cmp version" "$cuecmp_version"

# --- 0: a chart is published under its `version` -------------------------
if [[ "$operator_chart" != "$operator_app" ]]; then
    bad "the operator chart declares version ${operator_chart} but appVersion ${operator_app}.
release-operator.yml publishes the chart under \`version\`, and platform-stack pins it by that
string, so chart ${operator_app} would never exist and clusters would stay on the old operator
while the root Application reports Synced/Healthy.
Fix: operator/charts/apprafter-operator/Chart.yaml — move \`version\` with \`appVersion\`"
fi
if [[ "$webhook_chart" != "$webhook_app" ]]; then
    bad "the admission-webhook chart declares version ${webhook_chart} but appVersion ${webhook_app}.
Fix: operator/charts/apprafter-admission-webhook/Chart.yaml — move \`version\` with \`appVersion\`"
fi

# --- 1-3: the pins must agree --------------------------------------------
if [[ "$operator_pin" != "$operator_app" ]]; then
    bad "platform-stack pins operator ${operator_pin}, but the operator chart declares appVersion ${operator_app}.
Clusters would install ${operator_pin} while this tree builds ${operator_app} — a stale operator
starts, serves, and reports healthy, so nothing else would say so.
Fix: platform-stack/cue/component_apprafter-operator.cue"
fi
if [[ "$webhook_pin" != "$webhook_app" ]]; then
    bad "platform-stack pins the admission webhook at ${webhook_pin}, but its chart declares ${webhook_app}.
Fix: platform-stack/cue/component_admission-webhook.cue"
fi
if [[ -z "${compat_operator:-}" ]]; then
    bad "compatibility.cue has no entry for currentVersion ${current_version}.
Every published chart version needs one — PlatformController reads it to gate upgrades.
Fix: platform-stack/cue/compatibility.cue"
elif [[ "$compat_operator" != "$operator_pin" ]]; then
    bad "the compatibility entry for ${current_version} says operator ${compat_operator}, but the chart
pins ${operator_pin}. The entry is what an operator reads before upgrading, so the two
disagreeing means the release notes describe a different release.
Fix: platform-stack/cue/compatibility.cue"
fi

# --- 4: a sidecar bump must ride a chart bump -----------------------------
# The chart imports `argocd-cue-cmp/version.cue`, so that file IS chart
# source — but it lives outside `platform-stack/`, where the chart's own
# drift guard looks. Without this, bumping the sidecar publishes an image
# no chart points at.
tag="platform-stack/v${current_version}"
if git ls-remote --tags --exit-code "$REMOTE" "refs/tags/${tag}" >/dev/null 2>&1; then
    if ! git rev-parse --verify --quiet "refs/tags/${tag}" >/dev/null; then
        git fetch --quiet "$REMOTE" "refs/tags/${tag}:refs/tags/${tag}" 2>/dev/null || true
    fi
    if git rev-parse --verify --quiet "refs/tags/${tag}" >/dev/null; then
        if ! git diff --quiet "${tag}" HEAD -- 'argocd-cue-cmp/version.cue'; then
            bad "argocd-cue-cmp/version.cue changed since ${tag} was published, but currentVersion is
still ${current_version}. The chart reads that file at render time, so the new sidecar image
would be published with no chart pointing at it and clusters would keep the old one.
Fix: bump currentVersion in platform-stack/cue/platform.cue, add its compatibility entry."
        fi
    fi
else
    echo "  note: ${tag} not on ${REMOTE} yet — chart bump in flight, sidecar check skipped"
fi

if [[ "$fail" -ne 0 ]]; then
    exit 1
fi
echo "version coherence OK: operator, webhook, compatibility and sidecar pins agree"
