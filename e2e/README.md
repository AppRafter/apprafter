# e2e/

End-to-end smoke harness for AppRafter. Uses real Hetzner Cloud
credentials — every run costs a small amount of euros (one CX22
hour + a Floating IP if you use one). Set `APPRAFTER_E2E_SKIP_DESTROY=1`
to leave the cluster up after the run; otherwise the script tears
it down.

## mvp.sh

Provisions a fresh single-node cluster, bootstraps the in-cluster
stack (Cilium + Gateway API CRDs + Application CRD + Argo CD +
cert-manager + ClusterIssuer), applies a plain `Deployment` +
`Service` running `nginxdemos/hello:plain-text`, verifies the
endpoint via an in-cluster `curl` pod, and destroys.

The script does **NOT** install the AppRafter operator pod (its
container image isn't published yet — see the manual operator
quickstart at `docs/operator-guide/quickstart.md` for the full
Application-CRD flow). It tests the cluster lifecycle path end to
end; the Application reconcile loop is exercised by hand against a
real operator build.

### Usage

```sh
export HCLOUD_TOKEN=...                          # Hetzner API token
export APPRAFTER_SSH_PUBLIC_KEY="$(cat ~/.ssh/id_ed25519.pub)"
./e2e/mvp.sh                                     # green in ~6-8 min
```

### Env vars

| Var                              | Required | Default | Purpose                                             |
| -------------------------------- | -------- | ------- | --------------------------------------------------- |
| `HCLOUD_TOKEN`                   | yes      | —       | Hetzner Cloud API token (Read+Write).               |
| `APPRAFTER_SSH_PUBLIC_KEY`       | yes      | —       | SSH public key (single-line OpenSSH format).        |
| `APPRAFTER_E2E_REGION`           | no       | `nbg1`  | Hetzner region.                                     |
| `APPRAFTER_E2E_SKIP_DESTROY`     | no       | unset   | When `1`, keep the cluster up at the end.           |

### Phases + timing

| Phase                                    | Typical | Cumulative |
| ---------------------------------------- | ------- | ---------- |
| 1. provision (Hetzner CX22 + cloud-init) | ~30s    | 30s        |
| 2. wait for k3s                          | 3-5 min | 4-6 min    |
| 3. cluster-bootstrap                     | 1-2 min | 5-8 min    |
| 4. apply hello-world                     | <5s     | 5-8 min    |
| 5. wait for pod ready                    | 10-30s  | 5-9 min    |
| 6. verify endpoint                       | <10s    | 5-9 min    |
| 7. destroy                               | ~30s    | 6-9 min    |

Target time-to-first-Application: **< 30 minutes** (plan.md §1.12).
The smoke usually lands in 6-9 minutes wall-clock; the budget is
generous.

### What's missing

- The AppRafter operator pod itself isn't installed (operator
  image isn't published yet). The full
  `apprafter.io/v1alpha1.Application` flow lives in the manual
  operator quickstart.
- CI nightly job + auto-trigger — lands in v0.1.40 (sub-phase
  1.12b, closes phase 1.12).
- Image-build + registry-push integration — out of scope for the
  smoke; operators handle this via their own CI.

### Cleanup if the script crashes mid-run

```sh
cd cli && cargo run --bin platform-cli -- destroy --yes
```

`destroy` is idempotent and label-driven — it'll clean up
whatever's tagged `apprafter=true` regardless of the local state
file.
