---
description: "Running `apprafter node prep` on a node that needs it, and reading the posture `apprafter node status` reports."
---

# Node preparation

**You do not normally run this.** A node provisioned by `apprafter apply` gets
its control-plane reservations and its swap cushion from cloud-init at first
boot. `apprafter node prep` is the retrofit: a node provisioned before that
shipped, a node whose swap step failed at provision (`apprafter node status`
says so), or a node whose kubelet was too old at the time.

Why a node needs either is
[Node reservations and swap](../how-it-works/node-reservations-and-swap.md).

## Read the posture first

```sh
apprafter node status
```

It reports the node's swap and reservation state, labelling each field with
whether it came from the Kubernetes API or over SSH. The swap step reports one
of these:

| State | Meaning |
|-------|---------|
| Active with size | Swap is on; size and behaviour shown. |
| `eligible, not applied` | Node is ≥1.34 / cgroup v2 but swap was never provisioned — run `apprafter node prep`. |
| `ineligible (<1.34 — upgrade k3s)` | Kubelet is too old for `NoSwap` — upgrade k3s, then re-run `node prep`. |
| `swap skipped by env` | Swap was intentionally skipped at provision time (test hook). |
| `swap step failed at provision` | Bootstrap ran but the swap step failed; the node is up and cushionless. Re-run `node prep`. |
| orphaned `/swapfile` | A `/swapfile` exists but is neither active nor in fstab; `node prep` reconciles it. |


Only the first two rows and the last mean anything is to be done here.

## Applying it

```sh
apprafter node prep            # prompts before restarting k3s
apprafter node prep --yes      # no prompt (for scripted runs)
```

It connects over SSH and applies the reservations and the swap cushion over a
single k3s restart. Re-running it is safe: it is idempotent, and an orphaned
`/swapfile` — present but neither active nor in `/etc/fstab` — is reconciled
rather than treated as an error.

### It restarts k3s, for about thirty seconds

On a single-node cluster that is a brief Kubernetes API outage. Running
workloads keep running (containerd holds them through it) and Argo CD may log
one transient sync failure and self-heal. That is why the command prompts.

If the restart does not come back within the readiness timeout, `node prep`
**rolls the whole step back** and returns the node to its pre-`prep` state — a
bad run never leaves an unstartable node.

### Roll your workloads afterwards

`node prep` warns you to do this, and the warning is not optional: containers
that existed before the run keep their old swap setting until they are
recreated. Roll each through its **managed** restart path — the Postgres and
Redis operators both have one, and deleting a backend pod directly is how you
turn a maintenance step into a failover. The managed paths, and why the window
exists at all, are on
[Node reservations and swap](../how-it-works/node-reservations-and-swap.md#why-an-existing-container-keeps-its-old-setting).

## Where to look next

- [Node reservations and swap](../how-it-works/node-reservations-and-swap.md) —
  what the reservations protect, why pods never swap, and why the tier decides
  the policy.
- [Platform management](./platform-management.md) — managing the platform stack.
- [Recovery](./recovery.md) — the Hetzner rescue-mode runbook if a node is
  wedged.
