#!/usr/bin/env bash
# SPDX-License-Identifier: FSL-1.1-MIT
#
# AppRafter E2E MVP smoke. Provisions a fresh Hetzner cluster,
# runs `apprafter cluster-bootstrap`, applies a hello-world
# workload directly via kubectl (Deployment + Service), verifies the
# endpoint, then applies an Application CR and asserts the operator
# reconciles it to Ready, and tears the cluster down.
#
# Required env:
#   HCLOUD_TOKEN              — Hetzner Cloud API token
#   APPRAFTER_SSH_PUBLIC_KEY  — SSH public key inline (one line)
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
# Helpers
# ---------------------------------------------------------------

START_NS=$(date +%s%N)

elapsed() {
    local end now
    end=$(date +%s%N)
    now=$(( (end - START_NS) / 1000000000 ))
    printf '%dm %ds' $(( now / 60 )) $(( now % 60 ))
}

phase() {
    printf '\n=== %s (elapsed %s) ===\n' "$1" "$(elapsed)"
}

cleanup_on_failure() {
    local exit_code=$?
    if [ "$exit_code" -ne 0 ]; then
        printf '\n!!! E2E smoke failed at %s !!!\n' "$(elapsed)" >&2
        if [ -z "${APPRAFTER_E2E_SKIP_DESTROY:-}" ]; then
            printf 'Tearing down (set APPRAFTER_E2E_SKIP_DESTROY=1 to keep the cluster).\n' >&2
            (cd cli && cargo run --quiet --bin apprafter -- destroy --yes) || true
        fi
    fi
    exit "$exit_code"
}
trap cleanup_on_failure EXIT

# ---------------------------------------------------------------
# Preconditions
# ---------------------------------------------------------------

if [ -z "${HCLOUD_TOKEN:-}" ]; then
    echo "HCLOUD_TOKEN is not set" >&2
    exit 2
fi
if [ -z "${APPRAFTER_SSH_PUBLIC_KEY:-}" ]; then
    echo "APPRAFTER_SSH_PUBLIC_KEY is not set" >&2
    exit 2
fi

REGION="${APPRAFTER_E2E_REGION:-nbg1}"
KUBECONFIG_FILE=$(mktemp)
export KUBECONFIG="$KUBECONFIG_FILE"

# ---------------------------------------------------------------
# Phase 1: provision
# ---------------------------------------------------------------

phase "Phase 1: provisioning Hetzner ($REGION)"
(cd cli && cargo run --quiet --bin apprafter -- init \
    --provider hetzner-cloud --tier solo --region "$REGION")
(cd cli && cargo run --quiet --bin apprafter -- apply)

# ---------------------------------------------------------------
# Phase 2: wait for cloud-init (k3s install)
# ---------------------------------------------------------------

phase "Phase 2: waiting for k3s (cloud-init takes ~3-5 min)"
for attempt in $(seq 1 30); do
    if (cd cli && cargo run --quiet --bin apprafter -- kubeconfig) > "$KUBECONFIG_FILE" 2>/dev/null; then
        if kubectl get nodes 2>/dev/null | grep -q ' Ready '; then
            echo "  k3s ready on attempt $attempt"
            break
        fi
    fi
    if [ "$attempt" -eq 30 ]; then
        echo "k3s never became ready after 30 attempts (~5 min)" >&2
        exit 1
    fi
    printf '  attempt %d: not ready yet, sleeping 10s\n' "$attempt"
    sleep 10
done

# ---------------------------------------------------------------
# Phase 3: cluster-bootstrap
# ---------------------------------------------------------------

phase "Phase 3: cluster-bootstrap"
(cd cli && cargo run --quiet --bin apprafter -- cluster-bootstrap)

# ---------------------------------------------------------------
# Phase 4: apply hello-world (plain Deployment + Service)
# ---------------------------------------------------------------

phase "Phase 4: applying hello-world workload"
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

# ---------------------------------------------------------------
# Phase 5: wait for the pod to become Ready
# ---------------------------------------------------------------

phase "Phase 5: waiting for pod readiness"
kubectl wait --for=condition=Available deployment/e2e-hello \
    --namespace default --timeout=180s

# ---------------------------------------------------------------
# Phase 6: verify the endpoint via an in-cluster curl
# ---------------------------------------------------------------

phase "Phase 6: verifying http://e2e-hello.default.svc.cluster.local"
# Note: `curlimages/curl:8` (bare-major tag) is NOT published —
# only specific versions like `:8.11.0` and the floating
# `:latest` exist. Was broken from v0.1.39 on, hidden because
# nightly never ran (workflow secrets unset until 2026-05-10).
# Match the docs (`docs/operator-guide/quickstart.md` §4 uses
# bare `curlimages/curl` ⇒ `:latest`); fixed in v0.1.53.
output=$(kubectl run --rm -i --restart=Never \
    --image=curlimages/curl:latest \
    --namespace default \
    e2e-smoke-curl -- \
    curl -sSf --max-time 10 \
    http://e2e-hello.default.svc.cluster.local/ 2>&1 || true)

if echo "$output" | grep -q 'Server address:'; then
    echo "  endpoint OK (nginx-hello responded)"
else
    echo "  endpoint check failed; curl pod output:" >&2
    echo "$output" >&2
    exit 1
fi

# ---------------------------------------------------------------
# Phase 6.4: dual-stack pod IPs (ADR 0017 — closes 1.2 AUDIT
# deferred 5th checkbox; only meaningful once Cilium values land
# `ipv6.enabled: true` per 1.4 AUDIT in v0.1.71).
# ---------------------------------------------------------------

phase "Phase 6.4: dual-stack podIPs assertion"
pod_ips=$(kubectl get pod -l app=e2e-hello -n default \
    -o jsonpath='{.items[0].status.podIPs[*].ip}' 2>&1)
echo "  pod ips: $pod_ips"
# Expect a v4 from k3s default pod CIDR (10.42.0.0/16) AND a v6
# from the dual-stack range (fd00:42::/64) per user_data k3s flags.
# Cilium must have both `ipv4.enabled` and `ipv6.enabled` for the
# second address to actually appear on the pod's NIC.
if ! echo "$pod_ips" | grep -qE '(^|[[:space:]])10\.42\.'; then
    echo "  no v4 pod IP from 10.42.0.0/16 — k3s --cluster-cidr likely missing" >&2
    exit 1
fi
if ! echo "$pod_ips" | grep -qE '(^|[[:space:]])fd00:42:'; then
    echo "  no v6 pod IP from fd00:42::/64 — Cilium ipv6.enabled likely false (closes 1.4 AUDIT)" >&2
    exit 1
fi
echo "  dual-stack confirmed: both v4 and v6 pod IPs present"

# ---------------------------------------------------------------
# Phase 6.5: Application CRD end-to-end (§1.14 integration cycle)
# ---------------------------------------------------------------

phase "Phase 6.5: Application reconcile through operator"
kubectl apply -f manifests/tier-1/application/example-app.yaml

# Poll .status.phase up to 60s. Operator's reconcile loop typically
# resolves in <5s once the controller leader-elects, but allow
# headroom for the helm release to finish settling.
deadline=$(( $(date +%s) + 60 ))
while [ "$(date +%s)" -lt "$deadline" ]; do
    phase_val=$(kubectl get applications.apprafter.io parser \
        -n default -o jsonpath='{.status.phase}' 2>/dev/null || true)
    if [ "$phase_val" = "Ready" ]; then
        echo "  Application parser → phase: Ready"
        break
    fi
    sleep 2
done

if [ "$phase_val" != "Ready" ]; then
    echo "  Application parser did not reach phase: Ready within 60s" >&2
    echo "  last seen: '$phase_val'" >&2
    kubectl describe application.apprafter.io parser -n default >&2
    exit 1
fi

kubectl wait --for=condition=Available deployment/parser \
    --namespace default --timeout=60s
echo "  child Deployment parser → Available"

# ---------------------------------------------------------------
# Phase 7: destroy (unless opted out)
# ---------------------------------------------------------------

if [ -z "${APPRAFTER_E2E_SKIP_DESTROY:-}" ]; then
    phase "Phase 7: destroy"
    (cd cli && cargo run --quiet --bin apprafter -- destroy --yes)
else
    printf '\nAPPRAFTER_E2E_SKIP_DESTROY set — leaving the cluster up.\n'
    printf 'Run `cd cli && cargo run --bin apprafter -- destroy --yes` to clean up.\n'
fi

# ---------------------------------------------------------------
# Done
# ---------------------------------------------------------------

rm -f "$KUBECONFIG_FILE"
trap - EXIT
printf '\n✅ E2E smoke green in %s\n' "$(elapsed)"
