#!/usr/bin/env bash
# SPDX-License-Identifier: FSL-1.1-Apache-2.0
#
# check-component-enablement.sh — render the umbrella chart with the exact
# override the provisioner writes, and assert the components that override is
# supposed to switch on are actually there.
#
# ## Why
#
# `needs.jetstream` did not work on any cluster, for four releases, and every
# gate passed.
#
# `ResourceClaim` provisioning merge-patches
# `PlatformStack.spec.overrides.nats.enabled = true`
# (`reconcile.rs: ensure_nats_component_enabled`). `component_nack.cue` stated
# that this was "one override covers both components; there is no separate
# `overrides.nack`", and ADR 0061 §1 says the same. But
# `templates/applications.yaml` resolves an override by component NAME
# (`index $overrides $name`), so `overrides.nats` never reached the component
# called `nack`, its literal `enabled: false` stood, the `jetstream.nats.io`
# CRDs were never installed, and every jetstream claim in the cluster sat at
# `Ready=False AwaitingNackCrds` — a reason whose message reads "waiting … to
# be Established" and whose own doc comment calls it transient.
#
# Nothing executed that path. `cue vet` type-checks the component and is happy
# with `enabled: false`; `helm lint` renders the default values, where nack is
# correctly absent; and `e2e/needs-jetstream-walk.sh` applies the nats AND nack
# charts BY HAND, saying so in its header and dismissing "does PlatformStack
# render a nats/nack Application from an override" as "generic platform-stack
# plumbing already exercised by every other component". It was not generic:
# nack is the only component in this chart whose enablement was meant to come
# from a DIFFERENT component's key, and that was the one question the
# substitution carved out.
#
# So this script asserts the thing no other gate does — that a rendered chart,
# given the override the running system actually writes, contains the
# Applications that override promises.
#
# Usage: bash scripts/check-component-enablement.sh
# Exit 0 = every case held.

set -euo pipefail

REPO_ROOT="$(git rev-parse --show-toplevel)"
cd "$REPO_ROOT"

if command -v cue >/dev/null 2>&1; then
    CUE_CMD=(cue)
elif command -v nix >/dev/null 2>&1; then
    CUE_CMD=(nix run nixpkgs#cue --)
else
    echo "::error::neither \`cue\` nor \`nix\` is on PATH" >&2
    exit 2
fi
command -v helm >/dev/null 2>&1 || {
    echo "::error::helm is not on PATH — run under \`nix develop\`" >&2
    exit 2
}

version="$("${CUE_CMD[@]}" export ./platform-stack/cue/... -e currentVersion --out text)"
chart="platform-stack/dist/platform-stack-${version}"
if [[ ! -d "$chart" ]]; then
    echo "==> rendering $chart"
    make -C platform-stack render-only >/dev/null
fi
[[ -d "$chart" ]] || { echo "::error::no rendered chart at $chart" >&2; exit 1; }

workdir="$(mktemp -d)"
trap 'rm -rf "$workdir"' EXIT

failures=0

# `$1` label, `$2` overrides YAML body (may be empty), `$3` space-separated
# component names that MUST render, `$4` space-separated names that must NOT.
assert_render() {
    local label="$1" body="$2" want="$3" unwanted="$4"
    local values="$workdir/values.yaml"
    if [[ -n "$body" ]]; then
        printf 'overrides:\n%s\n' "$body" > "$values"
    else
        : > "$values"
    fi
    local rendered
    rendered="$(helm template platform "$chart" --values "$values" 2>/dev/null |
        awk '/^kind: Application$/{f=1} f&&/^  name:/{gsub(/"/,"",$2); print $2; f=0}')"
    local name
    for name in $want; do
        if ! grep -qx -- "$name" <<<"$rendered"; then
            echo "::error::$label: expected the '$name' Application to render, and it did not." >&2
            failures=$((failures + 1))
        fi
    done
    for name in $unwanted; do
        if grep -qx -- "$name" <<<"$rendered"; then
            echo "::error::$label: the '$name' Application rendered and must not have." >&2
            failures=$((failures + 1))
        fi
    done
    echo "  ok: $label"
}

echo "==> component enablement, chart $version"

# The default: NATS is opt-in, so neither half is installed. This is the case
# `helm lint` already covers, kept so a fix in the other direction — wiring
# nack on unconditionally — fails here rather than costing every cluster a
# controller it never asked for.
assert_render "default values install neither nats nor nack" \
    "" "" "nats nack"

# THE regression. This is byte-for-byte what `ensure_nats_component_enabled`
# merge-patches onto the PlatformStack.
assert_render "overrides.nats.enabled=true installs BOTH nats and nack" \
    "  nats:
    enabled: true" \
    "nats nack" ""

# Precedence, narrowest first: a component's OWN override beats the one it
# follows, in both directions. An operator pinning one half of a pair must
# keep that ability — it is the escape hatch for a broken nack image.
assert_render "an explicit overrides.nack.enabled=false wins over nats=true" \
    "  nats:
    enabled: true
  nack:
    enabled: false" \
    "nats" "nack"

assert_render "an explicit overrides.nack.enabled=true stands alone" \
    "  nack:
    enabled: true" \
    "nack" "nats"

# The disable direction has no caller today (`nats_override_patch`'s `false`
# arm is implemented and unused), so this is the assertion that keeps it
# correct for the day a reaper uses it.
assert_render "overrides.nats.enabled=false disables both" \
    "  nats:
    enabled: false" \
    "" "nats nack"

if (( failures > 0 )); then
    echo "::error::$failures component-enablement assertion(s) failed." >&2
    echo "  A component that follows another declares it with \`enabledFrom\` in" >&2
    echo "  platform-stack/cue/component_<name>.cue; the template honours it in" >&2
    echo "  render_tool.cue's \`_applicationsTemplate\`." >&2
    exit 1
fi

echo "component enablement OK: every override reaches the components it names"
