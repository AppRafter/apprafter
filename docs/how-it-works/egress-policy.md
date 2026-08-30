---
description: "How declared dependencies become egress rules: the per-application CiliumNetworkPolicy, what each profile emits, and how to observe a drop."
---

# Egress derived from declared dependencies

The recipe is [Egress policy](../operator-guide/egress-policy.md). This page is
the mechanism behind it. None of it is needed to set a profile.

The design — the per-app policy, the static connection-target catalog, the three
profiles, and the deferred `connects` / `AccessGrant` / `ExternalSurface`
follow-ups — is in [ADR
0045](../adr/0045-needs-networkpolicy-egress.md).

## One policy per application

For **every** Application the operator renders a `CiliumNetworkPolicy` named
`<deployment-name>-egress` in the application's own namespace, selecting that
application's pods on egress. A Cilium policy is an allow-list, so selecting the
pods at all is what makes them default-deny — everything below is an exception
to that.

The policy is owned by its Application, so deleting the app takes the policy
with it.

Its rules come from two places:

- **Baseline rules**, chosen by the cluster-wide profile: DNS (kube-dns),
  same-namespace traffic, and the external internet (`toEntities: [world]`).
- **One rule per declared network need**, always emitted whatever the profile
  says. `needs.pg` adds an allow to `cnpg-system` on 5432; `needs.redis` adds
  one to `dragonfly-system` on 6379. `needs.disk` adds nothing — a mounted
  volume has no network target.

| Profile | Baseline allows | Meaning |
| --- | --- | --- |
| `internet` (default) | DNS + same-namespace + `world` + needs | Internet open; cross-service in-cluster gated by needs. |
| `internal` | DNS + same-namespace + needs | In-cluster only — no external internet. |
| `strict` | DNS + needs | Even same-namespace egress is denied; applications are isolated within a namespace. |

The profile lives on the cluster-wide `PlatformStack` singleton. Unset, the
operator falls back to `internet`, so a freshly bootstrapped cluster behaves
like the first row.

Everything not named above is denied: another namespace's database, an
undeclared in-cluster service, a co-located pod under `strict`.

## Observing a drop

Two things make this harder than it looks, and both are worth knowing before
you conclude anything from a probe.

**Probe from inside the application's own pods.** The policy selects those pods
on egress. A separate debug pod is not selected, so it has open egress and
proves nothing — it will happily reach a destination the real workload cannot.

```sh
POD=$(kubectl -n demo get pod -l apprafter.io/application=web \
  -o jsonpath='{.items[0].metadata.name}')
kubectl -n demo exec "$POD" -- \
  timeout 8 nc -w 5 -z platform-postgres-rw.cnpg-system 5432; echo "exit=$?"
```

**A failed connect does not tell you why it failed.** A policy drop and a
missing listener look identical from inside the pod — both time out. Hubble is
the lens that distinguishes them, by reporting the datapath's own verdict:

```sh
hubble observe --pod demo/web --to-namespace cnpg-system --verdict DROPPED
hubble observe --pod demo/web --to-namespace cnpg-system --verdict FORWARDED
```

`DROPPED` is the policy refusing the flow. No flow at all usually means the
name did not resolve or nothing was listening.

## How the profile survives a GitOps sync

`apprafter platform egress set` writes the profile with a server-side apply
under the `apprafter-cli` field manager.

On a Tier-1 cluster with no infrastructure repository the platform-stack chart
does not declare `spec.network.egress.profile`, so nothing in Git competes with
the CLI's value and it survives Argo CD self-heal. If you opt into an
infrastructure repository that *does* declare the field, Git wins on the next
sync — the CLI prints a warning saying so, and the profile has to change in the
repository instead.

## Not covered by the shipped slice

- **`connects` / app-to-app egress**, and `AccessGrant`-gated per-application
  access. The shipped slice is the cluster-wide profile plus need-derived rules;
  finer control layers onto the same policy later.
- **`ExternalSurface`-gated external egress.** The `internet` profile leaves
  `world` open; allow-listing specific external destinations is a later step.
- **L7 and DNS-aware rules, and mTLS.** The policy is L3/L4 only at launch.

## For contributors

`just e2e-networkpolicy` runs the enforcement proof on a local kind cluster: it
brings kind up with the default CNI and kube-proxy disabled so Cilium owns the
datapath, bootstraps with Cilium, enables Hubble, and proves the chain end to
end — a needs-less application dropped on the way to Postgres, a `needs.pg`
application forwarded, and profile switches through the CLI. It needs a
checkout of the AppRafter repository.

On **rootless podman** the script preflights the memlock limit and fails in
seconds with the exact fix if it is too low, rather than after a seven-minute
cilium-agent `CrashLoopBackOff`. Cilium's eBPF agent raises `RLIMIT_MEMLOCK` to
infinity at startup; under rootless podman the kind node is capped at the host
user's systemd memlock hard limit (8 MB by default) and no container flag —
privileged, `CAP_SYS_RESOURCE`, `--ulimit` — can exceed it. Raise it once, as
root, then log back in:

```sh
sudo mkdir -p /etc/systemd/system/user@.service.d
printf '[Service]\nLimitMEMLOCK=infinity\n' \
  | sudo tee /etc/systemd/system/user@.service.d/90-memlock.conf
sudo systemctl daemon-reload
loginctl terminate-user "$USER"   # or log out and in; a reboot also works
# verify: podman run --rm busybox sh -c 'ulimit -Hl'   # must print: unlimited
```

**Rootful Docker** — including GitHub Actions `ubuntu-latest` — needs nothing:
`dockerd` ships `LimitMEMLOCK=infinity`. In CI the script runs nightly on a
rootful Docker runner. The other end-to-end scripts use kindnet rather than
Cilium and never hit this.
