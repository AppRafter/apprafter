---
description: "How the platform does what the guides ask it to do — the mechanism behind each operator recipe, for when you need to know."
---

# How it works

The operator and developer guides tell you what to run. These pages tell you
what happens when you do.

Nothing here is needed to operate AppRafter. Reach for a page when a guide's
outcome surprised you, when you are diagnosing something the troubleshooting
tables do not cover, or when you are changing the platform itself and need to
know what the current design is before you move it.

Each page names the code it describes, so a claim here can be checked rather
than taken on faith.

- [Declared Postgres dependencies](needs-pg.md) — how `needs.pg` becomes a
  database: the claim, the scheduler, the lazily-created shared cluster, the
  connection Secret, and the phased drop after the grace window.
- [Declared Redis dependencies](needs-redis.md) — how `needs.redis` becomes an
  isolated logical database on a shared pool, what the per-claim credential
  enforces, why channels are the one exception, and the flush after the grace
  window.
- [Declared disk dependencies](needs-disk.md) — how `needs.disk` becomes a
  mounted volume, why that volume has no owner and what the retention model
  rests on, and what single-writer storage forces on a rollout.
- [Egress derived from declared dependencies](egress-policy.md) — the per-app
  CiliumNetworkPolicy, which rules each profile emits, and how to tell a policy
  drop from a missing listener.
- [GitOps and the CUE plugin](gitops-and-the-cue-cmp.md) — what happens
  between `git push` and a running Deployment: the discovery probe, what the
  plugin sidecar writes into your checkout, and why each manifest is exported
  on its own.
- [How the platform upgrades itself](platform-upgrades.md) — why the CLI is
  not in the upgrade path, where a chart version comes from, and the four
  reasons an upgrade can be waiting.
- [Cross-application shared volumes](cross-application-shared-volumes.md) — why
  sharing is opt-in, and why the namespace is the boundary on Tier 1.
- [How a restore replays a backup](how-a-restore-works.md) — the order, and the
  two invariants that order protects.
- [Per-environment deploy](per-environment-deploy.md) — how an override merges
  onto the base, and why the environment belongs to the deployment.
- [The image digest](the-image-digest.md) — why a re-pushed tag rolls at all,
  and what happens when the registry cannot be read.
- [How the approval gate works](the-approval-gate.md) — why a change is held
  instead of applied, why only platform-scope plans can be rejected, and where
  a plan lives.
- [Node reservations and swap](node-reservations-and-swap.md) — what the
  control-plane reservations protect, why pods never swap, and why the tier
  decides the policy.
- [The target store on disk](the-target-store.md) — what
  `apprafter target add` writes and where, and the order every later command
  resolves a credential in.
- [Right-sizing an application's requests](resources-and-autoscaling.md) — the
  VerticalPodAutoscaler the platform renders, why the Deployment and the pod
  permanently disagree, the cluster-wide floors and ceilings, and how to tell a
  dead controller from a healthy quiet one.

## Why this section exists

[ADR 0058](../adr/0058-public-surfaces-are-written-for-their-reader.md) makes a
guide a recipe: its main flow carries `apprafter` commands and genuinely
external tools, and everything else routes by role. Mechanism is one of those
roles, and this is where it goes. Before that decision the explanations lived
inline, interleaved with the steps — which made short procedures read as long
ones and made the platform look like it demanded Kubernetes fluency to use.
