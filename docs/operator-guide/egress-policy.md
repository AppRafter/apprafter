---
description: "The cluster-wide egress posture: what declaring a dependency opens, the three profiles, how to change one, and what to do when an application loses reach."
---

# Egress policy

An application may reach an in-cluster backend only when it has **declared**
that dependency. `needs.pg` is what opens the path to Postgres; `needs.redis`
is what opens the path to Redis. Everything else in-cluster — another
namespace's database, an undeclared service — is denied at the network
datapath, not by convention.

There is one knob, cluster-wide: how much the *baseline* allows on top of that.

## The three profiles

| Profile | What it allows | When |
| --- | --- | --- |
| `internet` (default) | DNS, same-namespace, the external internet, plus every declared need | The launch default. Applications reach the internet freely; in-cluster reach is gated by what they declare. |
| `internal` | DNS, same-namespace, plus every declared need | No outbound internet. For clusters that must not talk to the outside world. |
| `strict` | DNS plus every declared need | Applications are isolated even from co-located pods in their own namespace. |

**A declared need stays open at every profile.** Tightening the posture never
breaks a dependency an application declared — it removes baseline reach, not
need-derived reach.

## Read and change the posture

```sh
apprafter platform egress show
```

prints the active profile and what it allows. To change it:

```sh
apprafter platform egress set internal
```

The operator re-renders every application's policy within a few seconds. There
is no `PlatformStack` CR to hand-edit, and the value survives Argo CD's
self-heal — see [How it
works](../how-it-works/egress-policy.md#how-the-profile-survives-a-gitops-sync)
for the one case where it does not.

To return to the launch default:

```sh
apprafter platform egress set internet
```

??? note "Verify independently with kubectl"

    Each application has its own policy, named after its deployment. The
    difference between an application that declares a dependency and one that
    does not is visible in the rules:

    ```sh
    kubectl -n demo get ciliumnetworkpolicies

    # an app with needs.pg carries an allow to cnpg-system on 5432
    kubectl -n demo get ciliumnetworkpolicy web-egress -o jsonpath='{.spec.egress}'
    ```

    After `platform egress set internal` the same policy no longer carries the
    `world` rule, while its pg rule is untouched.

## When you tighten the posture

An application that silently relied on reaching an **undeclared** in-cluster
service will lose that reach. This is the intended tightening rather than a
regression, and it is the one thing to plan for before changing the profile.

The fix is to declare the dependency — that is what opens the path, and it is
also what makes the dependency visible to everyone reading the manifest. Where
no `needs` type covers the destination yet, relaxing the profile is the
interim answer.

The same applies the first time an egress-aware operator rolls out onto a
cluster whose applications predate it: DNS, same-namespace, the internet and
every declared need stay open; undeclared cross-namespace reach does not.

## How it works

[Egress derived from declared dependencies](../how-it-works/egress-policy.md)
covers the per-application policy the operator renders, which rules each
profile emits, how to tell a policy drop from a missing listener, and what is
deliberately not covered by the shipped slice.

## Troubleshooting

| Symptom | Likely cause | Fix |
| ------- | ------------ | --- |
| An application cannot reach an in-cluster service it used to reach | the dependency is not declared, and the baseline no longer covers it | Declare it (`needs.pg`, `needs.redis`) — that is what opens the path. Check what the app's own policy allows: `kubectl -n <ns> get ciliumnetworkpolicy <app>-egress -o jsonpath='{.spec.egress}'`. |
| An application cannot reach the internet | the profile is `internal` or `strict` | `apprafter platform egress show`; set `internet` if outbound access is intended. |
| Two applications in one namespace cannot reach each other | the profile is `strict`, which drops the same-namespace baseline | Either declare the dependency, or move to `internal`. |
| A connection times out and it is unclear whether policy or the service is at fault | both look identical from inside the pod | Ask the datapath: `hubble observe --pod <ns>/<app> --to-namespace <target-ns> --verdict DROPPED`. A `DROPPED` verdict is the policy; no flow at all is usually DNS or a missing listener. |
| `apprafter platform egress set` warns that Git owns the field | an infrastructure repository declares the profile | Change it in the repository — Argo CD will otherwise revert the CLI's value on the next sync. |

## Prerequisites

- A Tier-1 cluster provisioned with `apprafter bootstrap-all` (see the
  [Quickstart](quickstart.md)). Cilium is the platform's CNI on every tier, so
  the policy machinery is always present.
- For the verification blocks above only, a kubeconfig, and — to read datapath
  verdicts — the `hubble` CLI (the `nix develop` shell ships `cilium-cli`).

## Related

- [ADR 0045 — needs → NetworkPolicy egress](../adr/0045-needs-networkpolicy-egress.md)
- [Declaring dependencies — the `needs` block](../dev-guide/application-cue.md#declaring-dependencies-the-needs-block)
- [Postgres from a declared dependency](postgres.md),
  [Redis from a declared dependency](redis.md)
- [Platform management](platform-management.md)
