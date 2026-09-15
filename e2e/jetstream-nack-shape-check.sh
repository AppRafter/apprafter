#!/usr/bin/env bash
# SPDX-License-Identifier: FSL-1.1-Apache-2.0
#
# Does NACK's CRD accept every field the provisioner emits? (2.28 / ADR 0065 §2)
#
# WHAT THIS IS, AND WHAT IT IS NOT
# --------------------------------
# `stream_object` / `consumer_object` build a NACK `Stream` / `Consumer` body
# by hand, from field names read out of the pinned chart's CRD. Two things can
# go wrong with that and neither is visible to any Rust test:
#
#   * a field name is wrong, so the apiserver PRUNES it — the apply answers
#     200, the object stores without it, and the setting the manifest asked
#     for silently never reaches NATS;
#   * a field's TYPE is wrong (a string where the CRD wants an integer), so
#     the apply is rejected at reconcile time, in the operator's log, far from
#     the manifest that caused it.
#
# This check applies the exact bodies the provisioner renders against the
# REAL NACK CRDs from the version `platform-stack/cue/component_nack.cue`
# pins, on a throwaway kind cluster, and asserts every field round-trips.
#
# It is NOT the 2.28 walk. It says nothing about whether NATS then behaves —
# that is `docs/measurements/2.28-jetstream-2026-09-15.md`, taken against the
# pinned server — and nothing about the provisioner's own reconcile path,
# which needs the platform stack and therefore Cilium. The three together
# cover the chain: our field names match the provider's CRD (here), the
# server honours those fields (the measurements), and the provisioner emits
# them (`nats.rs`'s pass-through tests, mutation-checked).
#
# Usage:  bash e2e/jetstream-nack-shape-check.sh
# Judge it by READING ITS LOG: every assertion prints `ok:`, and the last
# line is GREEN.

set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
CLUSTER="${APPRAFTER_E2E_CLUSTER:-apprafter-nack-shape}"
CHART_VERSION="$(grep -oE '^\s*version:\s*"[0-9.]+"' "${REPO_ROOT}/platform-stack/cue/component_nack.cue" \
    | head -1 | grep -oE '[0-9.]+')"
CLUSTER_UP=0

_kind() { if command -v kind >/dev/null 2>&1; then kind "$@"; else "$HOME/bin/kind" "$@"; fi; }
_helm() { if command -v helm >/dev/null 2>&1; then helm "$@"; else "$HOME/bin/helm" "$@"; fi; }

fail() { printf 'FAILED: %s\n' "$1" >&2; exit 1; }
cleanup() {
    [ "$CLUSTER_UP" = "1" ] || return 0
    [ "${APPRAFTER_E2E_KEEP:-0}" = "1" ] && { printf '\nkeeping cluster %s\n' "$CLUSTER"; return 0; }
    _kind delete cluster --name "$CLUSTER" >/dev/null 2>&1 || true
}
trap cleanup EXIT

printf '=== 1/4  the pinned NACK chart (%s) ===\n' "$CHART_VERSION"
[ -n "$CHART_VERSION" ] || fail "could not read the nack chart version from component_nack.cue"
WORK=$(mktemp -d)
_helm repo add nats https://nats-io.github.io/k8s/helm/charts/ >/dev/null 2>&1 || true
_helm repo update nats >/dev/null 2>&1 || true
_helm pull nats/nack --version "$CHART_VERSION" --destination "$WORK" >/dev/null \
    || fail "could not pull nack $CHART_VERSION"
tar xzf "$WORK"/nack-*.tgz -C "$WORK"
[ -s "$WORK/nack/crds/crds.yml" ] || fail "the pulled chart has no crds/crds.yml"
printf '  ok: chart pulled, CRDs present\n'

printf '\n=== 2/4  kind cluster ===\n'
_kind delete cluster --name "$CLUSTER" >/dev/null 2>&1 || true
KUBECONFIG="$(mktemp -t apprafter-nack-kube.XXXXXX)"
export KUBECONFIG
_kind create cluster --name "$CLUSTER" >/dev/null 2>&1 || fail "kind create failed"
CLUSTER_UP=1
kubectl apply -f "$WORK/nack/crds/crds.yml" >/dev/null || fail "applying the NACK CRDs"
kubectl wait --for=condition=Established --timeout=60s \
    crd/streams.jetstream.nats.io crd/consumers.jetstream.nats.io >/dev/null \
    || fail "the NACK CRDs never Established"
kubectl create namespace nats-system >/dev/null
printf '  ok: NACK CRDs Established\n'

printf '\n=== 3/4  every stream field the provisioner emits ===\n'
kubectl apply -f - >/dev/null <<'YAML' || fail "the NACK Stream CRD REJECTED a body the provisioner renders"
apiVersion: jetstream.nats.io/v1beta2
kind: Stream
metadata:
  name: demo-streamapp-orders
  namespace: nats-system
spec:
  name: streamapp_orders
  subjects: ["streamapp.orders.>"]
  storage: file
  retention: limits
  maxAge: "24h"
  maxBytes: 1073741824
  account: ns-demo
  maxMsgs: 1000000
  maxMsgsPerSubject: 1000
  maxMsgSize: 1048576
  maxConsumers: 16
  discard: new
  discardPerSubject: true
  duplicateWindow: "2m"
  compression: s2
  allowDirect: true
  allowRollup: true
  description: "order events"
  consumerLimits:
    inactiveThreshold: "24h"
    maxAckPending: 512
YAML
for pair in \
    "maxMsgs=1000000" "maxMsgsPerSubject=1000" "maxMsgSize=1048576" "maxConsumers=16" \
    "discard=new" "discardPerSubject=true" "duplicateWindow=2m" "compression=s2" \
    "allowDirect=true" "allowRollup=true" "consumerLimits.maxAckPending=512" \
    "consumerLimits.inactiveThreshold=24h"; do
    key=${pair%%=*}; want=${pair#*=}
    got=$(kubectl -n nats-system get stream demo-streamapp-orders \
        -o "jsonpath={.spec.${key}}" 2>/dev/null || true)
    [ "$got" = "$want" ] || fail "stream field ${key} did not round-trip: wanted '${want}', read back '${got}' — the provisioner would emit a setting the apiserver silently drops"
done
printf '  ok: 12 stream fields stored exactly as emitted\n'

printf '\n=== 4/4  every consumer field the provisioner emits ===\n'
kubectl apply -f - >/dev/null <<'YAML' || fail "the NACK Consumer CRD REJECTED a body the provisioner renders"
apiVersion: jetstream.nats.io/v1beta2
kind: Consumer
metadata:
  name: demo-consumerapp-reader
  namespace: nats-system
spec:
  durableName: consumerapp_reader
  streamName: streamapp_orders
  account: ns-demo
  ackPolicy: explicit
  deliverPolicy: byStartSequence
  ackWait: "30s"
  maxDeliver: 5
  backoff: ["1s", "5s", "30s"]
  maxAckPending: 100
  filterSubject: "streamapp.orders.eu"
  optStartSeq: 42
  replayPolicy: original
  maxWaiting: 64
  maxRequestBatch: 16
  maxRequestExpires: "10s"
  maxRequestMaxBytes: 1024
  inactiveThreshold: "1h"
  rateLimitBps: 2048
  headersOnly: true
  memStorage: true
  sampleFreq: "10%"
  description: "eu reader"
YAML
for pair in \
    "ackPolicy=explicit" "deliverPolicy=byStartSequence" "ackWait=30s" "maxDeliver=5" \
    "maxAckPending=100" "filterSubject=streamapp.orders.eu" "optStartSeq=42" \
    "replayPolicy=original" "maxWaiting=64" "maxRequestBatch=16" "maxRequestExpires=10s" \
    "maxRequestMaxBytes=1024" "inactiveThreshold=1h" "rateLimitBps=2048" \
    "headersOnly=true" "memStorage=true" "sampleFreq=10%" "description=eu reader"; do
    key=${pair%%=*}; want=${pair#*=}
    got=$(kubectl -n nats-system get consumer demo-consumerapp-reader \
        -o "jsonpath={.spec.${key}}" 2>/dev/null || true)
    [ "$got" = "$want" ] || fail "consumer field ${key} did not round-trip: wanted '${want}', read back '${got}'"
done
got=$(kubectl -n nats-system get consumer demo-consumerapp-reader \
    -o 'jsonpath={.spec.backoff[2]}' 2>/dev/null || true)
[ "$got" = "30s" ] || fail "consumer backoff did not round-trip: read back '${got}'"
printf '  ok: 19 consumer fields stored exactly as emitted\n'

printf '\nGREEN — every field the provisioner renders is accepted and stored by NACK %s.\n' \
    "$CHART_VERSION"
