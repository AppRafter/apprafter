#!/usr/bin/env bash
# SPDX-License-Identifier: FSL-1.1-Apache-2.0
#
# AppRafter E2E MVP smoke — 3-step flow.
#
# Step 1 (init): write provider/tier/region into state so that
#   bootstrap-all knows what to provision.
# Step 2 (bootstrap-all): `apprafter bootstrap-all` — provisions
#   Hetzner Cloud resources (phase 1/3), polls for the node to be
#   SSH-reachable and caches the kubeconfig (phase 2/3), then runs
#   cluster-bootstrap to install Cilium, Gateway API CRDs, the
#   Application CRD, Argo CD, cert-manager, and the platform
#   components (phase 3/3).  The call returns only once
#   cluster-bootstrap completes.
# Step 3 (Application smoke): assert the platform components are
#   Argo CD-managed and Synced, then apply an Application CR and
#   verify the operator reconciles it to Ready with an Available
#   Deployment.
#
# Required env:
#   HCLOUD_TOKEN              — Hetzner Cloud API token
#   APPRAFTER_SSH_PUBLIC_KEY  — SSH public key body inline (one line);
#                               read directly by apply's credential
#                               resolver (cli-core credentials.rs §7,
#                               SSH_PUBLIC_KEY_ENV="APPRAFTER_SSH_PUBLIC_KEY")
#
# Optional env:
#   APPRAFTER_E2E_SKIP_DESTROY=1  — keep the cluster + state
#                                   around at the end (for debug)
#   APPRAFTER_E2E_REGION=nbg1     — Hetzner region; default nbg1
#
# Exit codes:
#   0 — smoke green
#   1 — generic failure (set -e propagates the original exit code)
#   2 — env precondition missing (HCLOUD_TOKEN / SSH key)

set -euo pipefail

# ---------------------------------------------------------------
# Source shared helpers
# ---------------------------------------------------------------

# shellcheck source=e2e/lib.sh
source "$(dirname "$0")/lib.sh"

# ---------------------------------------------------------------
# Cleanup trap
# ---------------------------------------------------------------

_KUBECONFIG_FILE=""

cleanup_on_failure() {
    local exit_code=$?
    if [ "$exit_code" -ne 0 ]; then
        printf '\n!!! E2E smoke failed at %s !!!\n' "$(elapsed)" >&2
        if [ -z "${APPRAFTER_E2E_SKIP_DESTROY:-}" ]; then
            printf 'Tearing down (set APPRAFTER_E2E_SKIP_DESTROY=1 to keep the cluster).\n' >&2
            apprafter destroy --yes || true
        fi
    fi
    if [ -n "$_KUBECONFIG_FILE" ] && [ -f "$_KUBECONFIG_FILE" ]; then
        rm -f "$_KUBECONFIG_FILE"
    fi
    exit "$exit_code"
}
trap cleanup_on_failure EXIT

# ---------------------------------------------------------------
# Preconditions
# ---------------------------------------------------------------

require_env HCLOUD_TOKEN APPRAFTER_SSH_PUBLIC_KEY

REGION="${APPRAFTER_E2E_REGION:-nbg1}"
_KUBECONFIG_FILE=$(mktemp)
export KUBECONFIG="$_KUBECONFIG_FILE"

# ---------------------------------------------------------------
# Step 1: init (write provider/tier/region into state)
# ---------------------------------------------------------------

phase "Step 1: init (Hetzner, tier=solo, region=${REGION})"

# Write provider/tier/region into state so that bootstrap-all
# knows what to provision.  HCLOUD_TOKEN and APPRAFTER_SSH_PUBLIC_KEY
# are read by the credential resolver at apply time (cli-core
# credentials.rs SSH_PUBLIC_KEY_ENV); no target-store entry is
# required.
apprafter init \
    --provider hetzner-cloud \
    --tier     solo \
    --region   "$REGION"

# ---------------------------------------------------------------
# Step 2: bootstrap-all (provision + kubeconfig poll + bootstrap)
# ---------------------------------------------------------------

phase "Step 2: bootstrap-all (provision + kubeconfig poll + cluster-bootstrap)"

# bootstrap-all runs three phases internally:
#   [1/3] apply        — provision Hetzner Cloud resources
#   [2/3] k3s-ready    — poll SSH until k3s kubeconfig is reachable
#   [3/3] cluster-bootstrap — Cilium, Gateway API CRDs, Application
#                             CRD, Argo CD, cert-manager, ClusterIssuer,
#                             platform-stack Argo CD Apps
# Running `apprafter apply` before this would double-provision (B1).
apprafter bootstrap-all

# Pull the kubeconfig into our temp file so kubectl calls below work.
apprafter kubeconfig > "$_KUBECONFIG_FILE"

# ---------------------------------------------------------------
# Step 3: Application smoke
# ---------------------------------------------------------------

phase "Step 3a: Argo CD platform Applications are Synced"

# The self-managing-platform model (1.81 deliverable) requires that
# the platform components are managed by Argo CD, not applied
# directly by the CLI. Verify the expected Applications exist and
# are Synced.
PLATFORM_APPS=(cilium argocd cert-manager apprafter-operator admission-webhook)

for app in "${PLATFORM_APPS[@]}"; do
    sync_status=$(kubectl -n argocd get applications.argoproj.io "$app" \
        -o jsonpath='{.status.sync.status}' 2>/dev/null || true)
    if [ "$sync_status" != "Synced" ]; then
        printf '  FAIL: Argo CD Application %s expected Synced, got: %s\n' \
            "$app" "$sync_status" >&2
        kubectl -n argocd get applications.argoproj.io "$app" -o yaml >&2 || true
        exit 1
    fi
    printf '  Argo CD Application %s -> Synced\n' "$app"
done

phase "Step 3b: hello-world Deployment + Service"

kubectl apply -f - <<'EOF'
apiVersion: apps/v1
kind: Deployment
metadata:
  name: e2e-hello
  namespace: default
  labels:
    apprafter: "true"
    app: e2e-hello
spec:
  replicas: 1
  selector:
    matchLabels: { app: e2e-hello }
  template:
    metadata:
      labels: { app: e2e-hello, apprafter: "true" }
    spec:
      containers:
        - name: hello
          image: nginxdemos/hello:plain-text
          ports:
            - containerPort: 80
          readinessProbe:
            httpGet: { path: /, port: 80 }
            initialDelaySeconds: 2
            periodSeconds: 5
---
apiVersion: v1
kind: Service
metadata:
  name: e2e-hello
  namespace: default
  labels:
    apprafter: "true"
spec:
  type: ClusterIP
  selector:
    app: e2e-hello
  ports:
    - name: http
      port: 80
      targetPort: 80
EOF

kubectl wait --for=condition=Available deployment/e2e-hello \
    --namespace default --timeout=180s
printf '  e2e-hello Deployment -> Available\n'

phase "Step 3c: in-cluster endpoint verification"

# Note: `curlimages/curl:8` (bare-major tag) is NOT published —
# only specific versions like `:8.11.0` and the floating `:latest`
# exist. Use `:latest` per docs/operator-guide/quickstart.md §4.
# Fixed in v0.1.53.
curl_output=$(kubectl run --rm -i --restart=Never \
    --image=curlimages/curl:latest \
    --namespace default \
    e2e-smoke-curl -- \
    curl -sSf --max-time 10 \
    http://e2e-hello.default.svc.cluster.local/ 2>&1 || true)

if echo "$curl_output" | grep -q 'Server address:'; then
    printf '  endpoint OK (nginx-hello responded)\n'
else
    printf '  endpoint check failed; curl pod output:\n' >&2
    printf '%s\n' "$curl_output" >&2
    exit 1
fi

phase "Step 3d: dual-stack podIPs (ADR 0017)"

pod_ips=$(kubectl get pod -l app=e2e-hello -n default \
    -o jsonpath='{.items[0].status.podIPs[*].ip}' 2>&1)
printf '  pod ips: %s\n' "$pod_ips"

# Expect a v4 from k3s default pod CIDR (10.42.0.0/16) AND a v6
# from the dual-stack range (fd00:42::/64) per user_data k3s flags.
# Cilium must have both `ipv4.enabled` and `ipv6.enabled` for the
# second address to actually appear on the pod's NIC.
if ! echo "$pod_ips" | grep -qE '(^|[[:space:]])10\.42\.'; then
    printf '  no v4 pod IP from 10.42.0.0/16 — k3s --cluster-cidr likely missing\n' >&2
    exit 1
fi
if ! echo "$pod_ips" | grep -qE '(^|[[:space:]])fd00:42:'; then
    printf '  no v6 pod IP from fd00:42::/64 — Cilium ipv6.enabled likely false (closes 1.4 AUDIT)\n' >&2
    exit 1
fi
printf '  dual-stack confirmed: both v4 and v6 pod IPs present\n'

phase "Step 3e: Application CR reconcile through operator"

kubectl apply -f "${REPO_ROOT}/manifests/tier-1/application/example-app.yaml"

# Poll .status.phase up to 60s. Operator's reconcile loop typically
# resolves in <5s once the controller leader-elects, but allow
# headroom for the helm release to finish settling.
phase_val=""
deadline=$(( $(date +%s) + 60 ))
while [ "$(date +%s)" -lt "$deadline" ]; do
    phase_val=$(kubectl get applications.apprafter.io parser \
        -n default -o jsonpath='{.status.phase}' 2>/dev/null || true)
    if [ "$phase_val" = "Ready" ]; then
        printf '  Application parser -> phase: Ready\n'
        break
    fi
    sleep 2
done

if [ "$phase_val" != "Ready" ]; then
    printf '  Application parser did not reach phase: Ready within 60s\n' >&2
    printf '  last seen: %s\n' "$phase_val" >&2
    kubectl describe application.apprafter.io parser -n default >&2
    exit 1
fi

kubectl wait --for=condition=Available deployment/parser \
    --namespace default --timeout=60s
printf '  child Deployment parser -> Available\n'

# ---------------------------------------------------------------
# Destroy (unless opted out)
# ---------------------------------------------------------------

if [ -z "${APPRAFTER_E2E_SKIP_DESTROY:-}" ]; then
    phase "Destroy"
    apprafter destroy --yes
else
    printf '\nAPPRAFTER_E2E_SKIP_DESTROY set — leaving the cluster up.\n'
    printf 'Run: apprafter destroy --yes\n'
fi

# ---------------------------------------------------------------
# Done
# ---------------------------------------------------------------

rm -f "$_KUBECONFIG_FILE"
trap - EXIT
printf '\nE2E smoke green in %s\n' "$(elapsed)"
