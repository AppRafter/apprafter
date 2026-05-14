# apprafter-operator chart

Helm chart that deploys the v0.1.27 AppRafter operator binary
(controllers + Prometheus metrics + leader election).

## Prerequisites

The Application CRD (`apprafter.io/v1alpha1`) must be installed
before the operator pod starts. The simplest way is to run
`apprafter cluster-bootstrap` (which applies the CRD as part of
v0.1.22), or apply it manually:

```sh
kubectl apply -f manifests/tier-1/application/example-crd.yaml
```

cert-manager + a self-signed `ClusterIssuer` is also expected
(installed by `cluster-bootstrap` v0.1.15).

## Install

```sh
helm install apprafter-operator \
    ./operator/charts/apprafter-operator \
    --namespace apprafter-system \
    --create-namespace \
    --set image.repository=ghcr.io/<your-org>/apprafter-operator \
    --set image.tag=<tag-you-pushed>
```

The chart provisions:

- `ServiceAccount` `apprafter-operator`
- `ClusterRole` + `ClusterRoleBinding` — get/list/watch/patch on
  `apprafter.io/applications`, get/list/watch/create/update/delete
  on `apps/deployments`, `services`, `gateway.networking.k8s.io/httproutes`
  (the latter three are pre-emptively granted for phase 1.9), plus
  `events` create/patch.
- `Role` + `RoleBinding` (in the install namespace) —
  get/list/watch/create/update/patch on `coordination.k8s.io/leases`
  for leader election.
- `Deployment` — 1 replica, runs as `nonroot` (uid 65532), all
  capabilities dropped, `readOnlyRootFilesystem: true`,
  `seccompProfile: RuntimeDefault`. Liveness + readiness probes on
  `/healthz` + `/readyz`.
- `Service` (ClusterIP) — exposes the `/metrics` endpoint on port
  8080 for Prometheus scraping.

## Verify

```sh
kubectl -n apprafter-system get pods,svc,lease
# → pod apprafter-operator-... Running
# → svc apprafter-operator ClusterIP
# → lease apprafter-operator (holderIdentity = pod name)

kubectl -n apprafter-system port-forward svc/apprafter-operator 8080:8080 &
curl -s http://127.0.0.1:8080/healthz   # → ok
curl -s http://127.0.0.1:8080/metrics   # → apprafter_reconcile_*
```

## Values

See `values.yaml` for the full set of overridable values. Most
operators only need to set `image.repository` + `image.tag`.

## What's not in this chart yet

- A ServiceMonitor for the Prometheus operator.
- A NetworkPolicy that hardens egress beyond apiserver + DNS.
- The Application CRD itself (deliberately — see `Prerequisites`).
- `cluster-bootstrap` automation for `helm install` (operators run
  it manually for now).

These land in follow-up cycles.
