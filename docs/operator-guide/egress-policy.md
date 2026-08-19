---
description: "How a declared dependency is also what opens the egress path to it, how to watch an undeclared reach get dropped, and the cluster-wide profile knob."
---

# Egress gated by declared dependencies

This guide walks a Tier-1 operator through the `needs`-derived egress
policy: the operator emits one egress **CiliumNetworkPolicy** per
Application, so an app can reach an in-cluster backend (Postgres, Redis)
only when it has **declared** that dependency. An app without `needs.pg`
cannot reach the shared Postgres — the attempt is dropped at the Cilium
datapath, visible as a Hubble `DROPPED` verdict. The cluster-wide posture
is a single knob, `PlatformStack.spec.network.egress.profile`, managed by
`apprafter platform egress`.

The full design — the per-app CNP, the static connection-target catalog,
the three profiles, and the deferred `connects` / `AccessGrant` /
`ExternalSurface` follow-ups — is in
[ADR 0045](https://github.com/apprafter/apprafter/blob/master/docs/adr/0045-needs-networkpolicy-egress.md).

## The model in one paragraph

For **every** Application the operator renders one
`CiliumNetworkPolicy` named `<deployment-name>-egress` in the app's
namespace, selecting the app's pods on egress (which makes those pods
default-deny on egress — a Cilium policy is an allow-list). The baseline
rules allow DNS (kube-dns), same-namespace traffic, and the external
internet (`toEntities: [world]`); then **one allow rule per declared
network need** — `needs.pg` adds an allow to `cnpg-system` on port 5432,
`needs.redis` adds an allow to `dragonfly-system` on 6379. Everything else
in-cluster (cross-namespace, another team's database, an undeclared
service) is denied. The `egress.profile` chooses which **baseline** rules
are emitted; the need rules are always emitted regardless of profile.

| Profile | Baseline allows | Meaning |
| --- | --- | --- |
| `internet` (default) | DNS + same-namespace + `world` + needs | Internet open; cross-service in-cluster gated by needs. |
| `internal` | DNS + same-namespace + needs | In-cluster only — no external internet. |
| `strict` | DNS + needs | Maximal — even same-namespace egress is denied (apps isolated within a namespace). |

The profile lives on the cluster-wide `PlatformStack` singleton. When the
field is unset the operator falls back to the documented `internet`
default, so a freshly bootstrapped cluster behaves like the table's first
row with nothing declared.

> **Behaviour change.** The moment the egress-aware operator rolls out,
> existing apps become egress-restricted for cross-namespace in-cluster
> traffic. DNS, same-namespace, the external internet, and any **declared**
> need stay open. An app that silently relied on reaching an **undeclared**
> in-cluster service (for example another namespace's Postgres without a
> `needs.pg`) will break — this is the intended security tightening.
> Declare the dependency, or relax the profile, to restore reach.

## Prerequisites

- A real Tier-1 cluster provisioned with `apprafter bootstrap-all` (see
  [`quickstart.md`](quickstart.md)) running **Cilium** (the platform's CNI
  on every tier) with Hubble available for flow observation.
- `kubectl` bound to the cluster:

  ```sh
  export KUBECONFIG="$(apprafter kubeconfig --refresh)"
  ```

- The `cilium` and `hubble` CLIs (the `nix develop` shell ships
  `cilium-cli`). Hubble is the lens that makes a drop **observable** —
  without it you only see a connection time out.

## Step 0 — run the automated end-to-end script first (cheap gate)

Before spending a real cluster, run the automated enforcement script on a
local kind cluster. It brings kind up with the default CNI and kube-proxy
disabled so Cilium owns the datapath, bootstraps with Cilium, enables
Hubble, then proves the chain end to end (a needs-less app dropped to
Postgres, a `needs.pg` app forwarded, profile switches via the CLI):

```sh
just e2e-networkpolicy
```

On **rootless podman** the script preflights the memlock limit and, if it is
too low, fails in a few seconds with the exact fix (instead of a ~7-minute
cilium-agent `CrashLoopBackOff`). Cilium's eBPF agent raises
`RLIMIT_MEMLOCK` to infinity at startup; under rootless podman the kind
node is capped at the host user's systemd memlock hard limit (default
8 MB) and no container flag (privileged / `CAP_SYS_RESOURCE` / `--ulimit`)
can exceed it. Raise it once, as root, then re-login:

```sh
sudo mkdir -p /etc/systemd/system/user@.service.d
printf '[Service]\nLimitMEMLOCK=infinity\n' \
  | sudo tee /etc/systemd/system/user@.service.d/90-memlock.conf
sudo systemctl daemon-reload
loginctl terminate-user "$USER"   # or log out / in (a reboot also works)
# verify: podman run --rm busybox sh -c 'ulimit -Hl'   # must print: unlimited
```

**Rootful Docker** (including GitHub Actions `ubuntu-latest`) needs nothing —
`dockerd` ships `LimitMEMLOCK=infinity`. In CI the script runs nightly on a
rootful Docker runner (`.github/workflows/e2e-networkpolicy-nightly.yml`).
The other AppRafter end-to-end scripts use kindnet (no Cilium) and
never hit this.

## Step 1 — deploy one app with a need and one without

Register two Applications in a tenant namespace. `web` declares
`needs.pg`; `noproxy` declares nothing.

```sh
kubectl create namespace demo

# web — declares needs.pg
kubectl apply -f - <<'YAML'
apiVersion: apprafter.io/v1alpha1
kind: Application
metadata:
  name: web
  namespace: demo
spec:
  base:
    image: nginxdemos/hello:plain-text
    replicas: 1
    expose: { port: 80, network: internal }
    needs:
      pg:
        selector: { tier: integrated }
        size: small
YAML

# noproxy — no needs
kubectl apply -f - <<'YAML'
apiVersion: apprafter.io/v1alpha1
kind: Application
metadata:
  name: noproxy
  namespace: demo
spec:
  base:
    image: nginxdemos/hello:plain-text
    replicas: 1
    expose: { port: 80, network: internal }
YAML
```

Wait for both to reach `Ready`:

```sh
kubectl -n demo wait --for=jsonpath='{.status.phase}'=Ready \
  application.apprafter.io/web application.apprafter.io/noproxy --timeout=8m
```

## Step 2 — inspect the emitted policies

Each Application has its own egress policy. `web-egress` carries the pg
allow rule; `noproxy-egress` carries only the baseline.

```sh
kubectl -n demo get ciliumnetworkpolicies

# web-egress references cnpg-system + port 5432 (the pg allow rule):
kubectl -n demo get ciliumnetworkpolicy web-egress -o jsonpath='{.spec.egress}'

# noproxy-egress has NO cnpg-system reference (no pg rule):
kubectl -n demo get ciliumnetworkpolicy noproxy-egress -o jsonpath='{.spec.egress}'
```

The policy is owned by its Application, so deleting the app cascades the
policy away.

## Step 3 — observe the enforcement (the load-bearing proof)

Connectivity must be probed **from inside the app's own pods**, because
those are the pods the policy selects on egress (a separate tool pod is not
selected, so it would have open egress and prove nothing).

```sh
WEB=$(kubectl -n demo get pod -l apprafter.io/application=web \
  -o jsonpath='{.items[0].metadata.name}')
NOPROXY=$(kubectl -n demo get pod -l apprafter.io/application=noproxy \
  -o jsonpath='{.items[0].metadata.name}')

# noproxy -> Postgres: DENIED (no needs.pg). The connect fails:
kubectl -n demo exec "$NOPROXY" -- \
  timeout 8 nc -w 5 -z platform-postgres-rw.cnpg-system 5432; echo "exit=$?"

# web -> Postgres: ALLOWED (declared needs.pg). The connect succeeds:
kubectl -n demo exec "$WEB" -- \
  timeout 8 nc -w 5 -z platform-postgres-rw.cnpg-system 5432; echo "exit=$?"
```

Confirm the **datapath verdict** with Hubble — this is what distinguishes a
policy drop from a missing listener:

```sh
# A DROPPED flow from noproxy toward cnpg-system:
hubble observe --pod demo/noproxy --to-namespace cnpg-system --verdict DROPPED

# A FORWARDED flow from web toward cnpg-system:
hubble observe --pod demo/web --to-namespace cnpg-system --verdict FORWARDED
```

Under the default `internet` profile, both pods can still reach the
external internet:

```sh
kubectl -n demo exec "$WEB" -- timeout 8 nc -w 5 -z 1.1.1.1 443; echo "exit=$?"
```

## Step 4 — tighten the posture with the CLI

`apprafter platform egress` reads and sets the cluster-wide profile. There
is no need to hand-edit the `PlatformStack` CR.

```sh
# Show the active profile and what it allows:
apprafter platform egress show

# Tighten to in-cluster only — drops the `world` rule, keeps the needs:
apprafter platform egress set internal
```

After the operator re-renders (a few seconds), `web-egress` no longer
carries the `world` rule. The external connect is now denied, while
Postgres still works for `web`:

```sh
kubectl -n demo get ciliumnetworkpolicy web-egress -o jsonpath='{.spec.egress}'  # no `world`
kubectl -n demo exec "$WEB" -- timeout 8 nc -w 5 -z 1.1.1.1 443; echo "exit=$?"   # fails
kubectl -n demo exec "$WEB" -- \
  timeout 8 nc -w 5 -z platform-postgres-rw.cnpg-system 5432; echo "exit=$?"      # succeeds
```

Tighten further to isolate apps **within** a namespace:

```sh
apprafter platform egress set strict
```

Under `strict` the same-namespace baseline rule is dropped too, so an app
can no longer reach a co-located pod it does not have a need for — only
DNS and its declared needs remain. The declared need stays open at every
profile, so `web` keeps reaching Postgres.

```sh
# noproxy can no longer reach the same-namespace web pod under strict:
WEB_IP=$(kubectl -n demo get pod "$WEB" -o jsonpath='{.status.podIP}')
kubectl -n demo exec "$NOPROXY" -- timeout 8 nc -w 5 -z "$WEB_IP" 80; echo "exit=$?"  # fails
hubble observe --pod demo/noproxy --to-namespace demo --verdict DROPPED
```

To return to the launch default:

```sh
apprafter platform egress set internet
```

## How the profile persists across GitOps syncs

`apprafter platform egress set` writes the profile with a server-side apply
under the `apprafter-cli` field manager. On a Tier-1 cluster with no
infra-repo, the platform-stack chart does **not** declare
`spec.network.egress.profile`, so nothing in Git competes with the CLI's
value and it survives Argo CD self-heal. If you opt into an infra-repo that
declares the field, Git wins on the next sync and `apprafter platform
egress set` prints a warning — change the profile in the repo instead.

## What this guide does NOT cover (deferred)

- **`connects` / app-to-app egress** and **`AccessGrant`-gated**, per-app
  fine-grained access. The shipped slice covers only the cluster-wide
  profile; finer
  control layers onto the same CNP mechanism later.
- **`ExternalSurface`-gated external egress.** The `internet` profile
  leaves `world` open; allow-listing specific external destinations is a
  future step.
- **L7 / DNS-aware rules and mTLS.** The policy is L3/L4 only at launch.
- **`needs.disk`** — a mounted volume has no network target, so it adds no
  egress rule.

## Related

- [ADR 0045 — needs → NetworkPolicy egress](https://github.com/apprafter/apprafter/blob/master/docs/adr/0045-needs-networkpolicy-egress.md)
- [Declaring dependencies — the `needs` block](../dev-guide/application-cue.md#declaring-dependencies-the-needs-block)
- [Postgres from a declared dependency](postgres.md),
  [Redis from a declared dependency](redis.md)
- [Platform management](platform-management.md)
