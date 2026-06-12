#!/usr/bin/env bash
# SPDX-License-Identifier: FSL-1.1-Apache-2.0
#
# Render-assertion regression guard for the 1.83a public-ingress
# Gateway slice in platform-stack. The platform Gateway is
# umbrella-emitted (`templates/gateway.yaml`) and guarded by
# `{{- if .Values.gateway.allowedDomains }}` — host-network mode, so
# it emits ONLY a `Gateway` + a `platform-http-redirect` HTTPRoute and
# never an LB-IPAM / L2 announcement object. This script pins that
# contract so a future refactor of the template, the cilium component,
# or the gateway-api-crds Application can't silently regress it.
#
# It asserts, against a freshly rendered chart:
#
#   1. DEFAULT render (empty allowedDomains): no `platform` Gateway,
#      no `platform-http-redirect` HTTPRoute, no CiliumLoadBalancerIPPool,
#      no CiliumL2AnnouncementPolicy.
#   2. POPULATED render (two imported-cert domains): exactly one
#      `platform` Gateway with the five expected listeners (dot-free
#      names), one `platform-http-redirect` HTTPRoute (301), and still
#      NO LB-IPAM / L2 objects (host-network needs no LB Service).
#   3. The cilium Argo Application carries `gatewayAPI.hostNetwork.
#      enabled: true` and NO `l2announcements` / `externalIPs`.
#   4. The `gateway-api-crds` Argo Application renders at sync-wave -25
#      with targetRevision v1.2.1 and source.path `config/crd/experimental`
#      (the EXPERIMENTAL channel, NOT `standard` — cilium 1.16.5's gateway
#      controller fails its required-resources check without TLSRoute, which
#      lives only in the experimental channel; T8 walk-fix 2026-06-12).
#
# Usage:
#
#   bash scripts/check-gateway-render.sh
#
# Exit 0 = every assertion passed. Exit non-zero with a human-readable
# error pointing at what regressed. Self-contained: it resolves the
# version, renders the chart, and runs every assertion.

set -euo pipefail

REPO_ROOT="$(git rev-parse --show-toplevel)"
cd "$REPO_ROOT"

# Resolve the CUE binary the same way scripts/check-platform-stack-version.sh
# does: prefer a local install, fall back to `nix run nixpkgs#cue --`.
if command -v cue >/dev/null 2>&1; then
  CUE_CMD=(cue)
elif command -v nix >/dev/null 2>&1; then
  CUE_CMD=(nix run nixpkgs#cue --)
else
  echo "ERROR: cue is not installed and nix is unavailable." >&2
  exit 2
fi

# Resolve the Helm binary: prefer a local install, fall back to
# `nix run nixpkgs#kubernetes-helm --`.
if command -v helm >/dev/null 2>&1; then
  HELM_CMD=(helm)
elif command -v nix >/dev/null 2>&1; then
  HELM_CMD=(nix run nixpkgs#kubernetes-helm --)
else
  echo "ERROR: helm is not installed and nix is unavailable." >&2
  exit 2
fi

fail() {
  echo "::error::$*" >&2
  echo "FAIL: $*" >&2
  exit 1
}

# ---------------------------------------------------------------------------
# 1. Resolve currentVersion + render the chart (self-contained).
# ---------------------------------------------------------------------------
VERSION="$("${CUE_CMD[@]}" export ./platform-stack/cue/... \
            -e currentVersion --out text 2>&1)" || {
  echo "ERROR: failed to read currentVersion from platform-stack/cue/platform.cue" >&2
  echo "$VERSION" >&2
  exit 1
}
[[ -n "${VERSION:-}" ]] || fail "currentVersion is empty"
echo "Resolved currentVersion: ${VERSION}"

make -C platform-stack render-only >/dev/null
CHART="platform-stack/dist/platform-stack-${VERSION}"
[[ -d "$CHART" ]] || fail "rendered chart dir not found: ${CHART}"

DEFAULT_YAML="/tmp/gw-default.yaml"
POPULATED_YAML="/tmp/gw-populated.yaml"

# ---------------------------------------------------------------------------
# 2. DEFAULT render (empty allowedDomains) — the gateway is dormant.
# ---------------------------------------------------------------------------
"${HELM_CMD[@]}" template platform "$CHART" > "$DEFAULT_YAML"

# NOTE: `kind: HTTPRoute` strings also appear inside AppProject RBAC
# whitelists, so assert on the SPECIFIC `platform-http-redirect` name,
# never bare `HTTPRoute`.
if grep -qE '^kind: Gateway$' "$DEFAULT_YAML"; then
  fail "default render emitted a Gateway — gateway.yaml must be dormant when allowedDomains is empty"
fi
if grep -q 'name: platform-http-redirect' "$DEFAULT_YAML"; then
  fail "default render emitted the platform-http-redirect HTTPRoute — must be dormant when allowedDomains is empty"
fi
if grep -q 'kind: CiliumLoadBalancerIPPool' "$DEFAULT_YAML"; then
  fail "default render emitted a CiliumLoadBalancerIPPool — host-network mode must not ship LB-IPAM"
fi
if grep -q 'kind: CiliumL2AnnouncementPolicy' "$DEFAULT_YAML"; then
  fail "default render emitted a CiliumL2AnnouncementPolicy — host-network mode must not ship L2 announcements"
fi
echo "PASS: default render (empty allowedDomains) emits no Gateway / redirect / LB-IPAM / L2."

# ---------------------------------------------------------------------------
# 3. POPULATED render (two imported-cert domains).
# ---------------------------------------------------------------------------
"${HELM_CMD[@]}" template platform "$CHART" \
  --set-json 'gateway.allowedDomains=[{"domain":"example.com","certMode":"imported","importedCertRef":"example-com-tls","addedAt":"2026-06-11T00:00:00Z","addedBy":"ci"},{"domain":"my.example.org","certMode":"imported","importedCertRef":"my-example-org-tls","addedAt":"2026-06-11T00:00:00Z","addedBy":"ci"}]' \
  > "$POPULATED_YAML"

# Exactly one Gateway named `platform`.
gw_kind_count=$(grep -cE '^kind: Gateway$' "$POPULATED_YAML" || true)
[[ "$gw_kind_count" == "1" ]] || fail "expected exactly 1 'kind: Gateway', got ${gw_kind_count}"
gw_name_count=$(grep -cE '^  name: platform$' "$POPULATED_YAML" || true)
[[ "$gw_name_count" == "1" ]] || fail "expected exactly 1 Gateway named 'platform', got ${gw_name_count}"

# All five listeners present (4 dot-free https + the http listener).
for listener in \
  https-apex-example-com \
  https-wild-example-com \
  https-apex-my-example-org \
  https-wild-my-example-org; do
  grep -qE "^  - name: \"${listener}\"$" "$POPULATED_YAML" \
    || fail "populated render missing listener '${listener}'"
done
grep -qE '^  - name: http$' "$POPULATED_YAML" \
  || fail "populated render missing the 'http' listener"

# No https listener name may contain a dot (Gateway listener names must
# be DNS-label-ish; the template `replace "." "-"` guarantees this).
dotty=$(grep -E '^  - name: "https-' "$POPULATED_YAML" | grep -E '\.' || true)
if [[ -n "$dotty" ]]; then
  fail "populated render has a dotted https listener name (must be dot-free): ${dotty}"
fi
echo "PASS: populated render has exactly one 'platform' Gateway with the five expected dot-free listeners."

# One platform-http-redirect HTTPRoute with statusCode 301.
redirect_count=$(grep -cE '^  name: platform-http-redirect$' "$POPULATED_YAML" || true)
[[ "$redirect_count" == "1" ]] \
  || fail "expected exactly 1 'platform-http-redirect' HTTPRoute, got ${redirect_count}"
grep -qE '^[[:space:]]*statusCode: 301$' "$POPULATED_YAML" \
  || fail "populated render's platform-http-redirect missing statusCode: 301"
echo "PASS: populated render has one platform-http-redirect HTTPRoute (301)."

# Still NO LB-IPAM / L2 in host-network mode.
if grep -q 'kind: CiliumLoadBalancerIPPool' "$POPULATED_YAML"; then
  fail "populated render emitted a CiliumLoadBalancerIPPool — host-network mode must not ship LB-IPAM"
fi
if grep -q 'kind: CiliumL2AnnouncementPolicy' "$POPULATED_YAML"; then
  fail "populated render emitted a CiliumL2AnnouncementPolicy — host-network mode must not ship L2 announcements"
fi
echo "PASS: populated render still emits no LB-IPAM / L2 (host-network)."

# ---------------------------------------------------------------------------
# 4. Cilium Argo Application valuesObject — host-network on, no L2/externalIPs.
# ---------------------------------------------------------------------------
# Slice the cilium Application block (from its `name: "cilium"` line to the
# next document separator) and assert on that scope only.
cilium_block=$(awk '/^  name: "cilium"$/{f=1} f{print} f&&/^---$/{exit}' "$DEFAULT_YAML")
[[ -n "$cilium_block" ]] || fail "could not locate the cilium Argo Application block in the default render"

grep -qE '^[[:space:]]*gatewayAPI:$' <<<"$cilium_block" \
  || fail "cilium Application valuesObject missing 'gatewayAPI'"
# hostNetwork.enabled: true under gatewayAPI.
grep -qE '^[[:space:]]*hostNetwork:$' <<<"$cilium_block" \
  || fail "cilium Application valuesObject missing 'gatewayAPI.hostNetwork'"
grep -qE '^[[:space:]]*enabled: true$' <<<"$cilium_block" \
  || fail "cilium Application valuesObject missing an 'enabled: true' (gatewayAPI/hostNetwork)"
# L2 / externalIPs must be absent — host-network mode.
if grep -qE 'l2announcements' <<<"$cilium_block"; then
  fail "cilium Application valuesObject contains 'l2announcements' — host-network mode must not enable L2"
fi
if grep -qE 'externalIPs' <<<"$cilium_block"; then
  fail "cilium Application valuesObject contains 'externalIPs' — host-network mode must not enable externalIPs"
fi
echo "PASS: cilium Application has gatewayAPI.hostNetwork.enabled and no l2announcements/externalIPs."

# ---------------------------------------------------------------------------
# 5. gateway-api-crds Argo Application — wave -25, targetRevision v1.2.1.
# ---------------------------------------------------------------------------
crds_block=$(awk '/^  name: "gateway-api-crds"$/{f=1} f{print} f&&/^---$/{exit}' "$DEFAULT_YAML")
[[ -n "$crds_block" ]] || fail "could not locate the gateway-api-crds Argo Application block in the default render"

grep -qE '^[[:space:]]*argocd\.argoproj\.io/sync-wave: "-25"$' <<<"$crds_block" \
  || fail "gateway-api-crds Application missing sync-wave -25"
grep -qE '^[[:space:]]*targetRevision: "v1\.2\.1"$' <<<"$crds_block" \
  || fail "gateway-api-crds Application missing targetRevision v1.2.1"
# The EXPERIMENTAL channel is required: cilium 1.16.5's gateway controller
# runs a required-resources check at startup that includes TLSRoute (v1alpha2),
# which ships ONLY in the experimental channel. The standard channel makes the
# controller fail (`error="...tlsroutes...not found"`) and no Gateway ever
# reaches Programmed (T8 walk-fix 2026-06-12). Pin the experimental path so a
# revert to `standard` can't silently re-break the platform Gateway.
grep -qE '^[[:space:]]*path: "config/crd/experimental"$' <<<"$crds_block" \
  || fail "gateway-api-crds Application source.path is not config/crd/experimental (cilium 1.16.5 needs the experimental channel for TLSRoute)"
if grep -qE '^[[:space:]]*path: "config/crd/standard"$' <<<"$crds_block"; then
  fail "gateway-api-crds Application uses config/crd/standard — must be config/crd/experimental (TLSRoute required by cilium 1.16.5)"
fi
echo "PASS: gateway-api-crds Application renders at sync-wave -25 with targetRevision v1.2.1 and source.path config/crd/experimental."

echo
echo "SUCCESS: gateway render assertions passed for platform-stack ${VERSION}:"
echo "  - default render: no Gateway / platform-http-redirect / LB-IPAM / L2"
echo "  - populated render: 1 platform Gateway, 5 dot-free listeners, 1 redirect (301), no LB-IPAM / L2"
echo "  - cilium Application: gatewayAPI.hostNetwork.enabled, no l2announcements / externalIPs"
echo "  - gateway-api-crds Application: sync-wave -25, targetRevision v1.2.1, path config/crd/experimental"
