---
description: "What the platform treats as a destructive change, how to see a pending plan, and how to approve or reject one."
---

# Migration plans

Some changes — to a user application, to a `SourceCredential`, or to the
platform stack itself — are held for approval instead of being applied. The
previous version keeps running until you act.

Why the gate exists and why the three scopes behave differently is
[How the approval gate works](../how-it-works/the-approval-gate.md). The
developer-facing half — the thirteen manifest edits that hold, written as edits
rather than as trigger names — is
[When a change needs approval](../dev-guide/when-a-change-needs-approval.md).

## What counts as destructive

The `MigrationController` classifies changes into five risk levels,
ordered by severity — the number is the `severity` each entry in the
plan's `spec.changes[]` carries beside its `classification`:

| Classification      | Sev | Examples                                                   |
| ------------------- | --- | ---------------------------------------------------------- |
| `safe`              | 0   | Env-var literal edits, replica count changes, label updates. |
| `requires-restart`  | 1   | Losing a public hostname, scale-to-zero, major Argo CD bump. |
| `breaking`          | 2   | Kubernetes minor upgrade, Cilium major version change, narrowing a `SourceCredential`. |
| `data-migration`    | 3   | Storage-class change, `needs.pg` removal.                  |
| `security-boundary` | 4   | Anything that changes **what gets pulled or who can reach it**: an image *repository* move, publishing a new public hostname, going public, wiring in a new `secret:` reference. |

Only `requires-restart` and above create a `MigrationPlan`. Safe
changes are applied immediately.

`security-boundary` is the class to know: it is the most severe, it
outranks a co-occurring `data-migration` for the plan's headline, and
several of the edits that carry it *look* additive. A plan whose
`spec.changes[]` holds more than one candidate lists them all, so a
severe op cannot ride along behind a benign-looking primary.

Platform-stack specific triggers (applied when diffing a
`PlatformStack` upgrade):

- Any diff classified as `requires-restart`, `data-migration`, or
  `breaking` in the chart's compatibility metadata. Those four (with
  `safe`) are the whole vocabulary on that side — the chart's
  `#ChangeClass` has no `security-boundary`, which is an
  application- and credential-scope class only.

### Application triggers

For an application edit, the diff is taken between the last applied
spec and the new spec, each evaluated under its own environment, so a
change in one environment gates only that environment's deployment.
Thirteen edits gate. Read the `security-boundary` half twice: several
of them are things people think of as additions.

| The edit | Trigger | Class |
| --- | --- | --- |
| Removing a `needs.*` dependency — the backing claim and its data are garbage-collected | `needs-removal` | `data-migration` |
| `expose.network` `public` → non-public (`internal` / `vpn`) — external reachability withdrawn | `network-visibility-change` | `requires-restart` |
| A publicly-routed app **loses** a hostname (removed, or swapped for another) | `domain-change` | `requires-restart` |
| `replicas` N → 0 — a deliberate outage | `scale-to-zero` | `requires-restart` |
| Removing an env key whose value is a reference (`claim.…` or `secret:"name/key"`) | `env-ref-removal` | `requires-restart` |
| `expose.network` non-public → **`public`** — the app is now on the public Gateway | `network-visibility-escalation` | `security-boundary` |
| A public app **gains** a hostname — including its first, and including a second one beside an existing one | `public-hostname-add` | `security-boundary` |
| A public app's `expose.port` changes — the public route now targets something else | `public-port-retarget` | `security-boundary` |
| The image **repository** changes (the path, not the tag) — a different pull source | `image-path-change` | `security-boundary` |
| `imagePolicy.resolve` `off` → anything else on a tag-referenced image — a pinned reference goes back to floating | `image-policy-relaxation` | `security-boundary` |
| An env key becomes a `secret:"name/key"` reference — from absent, from a literal, or from a `claim.…` reference | `env-secret-ref-add` | `security-boundary` |
| An env key stops being a reference and becomes a literal | `env-ref-downgrade` | `security-boundary` |
| An env key's `secret:` reference is re-pointed at a different secret | `env-secret-ref-retarget` | `security-boundary` |

A hostname swap `{a}` → `{b}` on a public app fires **two** — `a` was
lost (`domain-change`) and `b` was gained (`public-hostname-add`) —
and the plan's headline is the more severe of the two.

Approving a `needs-removal` puts the backing claim on the same seven-day
retention path `apprafter app remove` uses. The order is deliberate: the
new spec is applied first, then the claim this application no longer
declares is deleted, and the provisioner's finalizer writes a
`RetainedClaim` snapshot on the way out — so the clock starts at approval,
not at the edit. Nothing is deleted while the plan is still pending.

These edits do **not** gate — they auto-apply, and the soft-destructive
ones emit a `SoftDestructiveChange` Kubernetes Event you can see with
`kubectl get events`:

- Adding a `needs`, or an env var that is a **literal** or a
  `claim.…` reference. (An added `secret:` reference gates — the row
  above.)
- Any hostname or port edit on a **non-public** app: nothing is routed,
  so nothing changes externally.
- Scaling from zero (0 → N) or down to a non-zero count.
- Removing an env *literal* (a plain string, not a reference).
- Changing only the image **tag** on the same repository — the operator
  resolves the tag to a digest and rolls it out automatically.
- Turning `imagePolicy.resolve` **off** (pinning the tag verbatim) —
  that is hardening, not relaxation.
- Changing `needs.*.size` — storage is expansion-only, so a shrink is
  rejected at the provisioner layer.
- Changing `needs.*.selector` — deferred while a single provider tier
  ships; the edit produces the same `(type, name)` key on both sides,
  so no candidate is raised.

### SourceCredential triggers

**Narrowing a `SourceCredential` is gated, and it pauses derivation.**
Removing a covered `repoPrefix` or registry `host` — including dropping
a whole `git` or `registry` half — raises a `coverage-removal` trigger
classified `breaking`, and the controller creates a plan **before**
either derivation half runs.

While it is pending, the credential reports
`status.phase: AwaitingMigrationApproval` with `Ready=False` /
`MigrationPending=True` (both messages name the plan), and the
previously derived, **wider** Secrets are deliberately left in place —
so in-flight applications keep cloning and pulling. Nothing is derived
from the narrowed spec until you act:

```sh
apprafter migration list                     # the plan, in the credential's namespace
apprafter migration approve <plan-name>      # derive with the narrowed coverage
```

Re-widening the spec also clears it: the stale plan is garbage-collected
and derivation proceeds. There is no reject — see below.

Adding coverage, creating a credential, and rotating the material
(`apprafter repo creds rotate`, which replaces the sealed material and
leaves the spec untouched) are **not** destructive and do not gate.

## Acting on a plan

## CLI surface

```sh
# List MigrationPlans across ALL namespaces, with namespace, name,
# scope, classification, and current phase. Platform-scope plans
# live in apprafter-system; application-scope plans live in the
# application's own namespace; sourcecredential-scope plans live in
# the credential's namespace.
apprafter migration list

# Approve a plan. The namespace is resolved automatically from the
# listing (pass -n <namespace> to disambiguate). MigrationController
# transitions it through executing → completed.
apprafter migration approve <plan-name>

# Reject a plan (platform scope only). Reverts spec.pin to the
# previous value. Application- and sourcecredential-scope plans have
# no reject — revert the change in Git, or re-widen the credential.
apprafter migration reject <plan-name>
```

??? note "Reading and approving a plan without the CLI"

    The status-subresource patch is the authoritative approval — the CLI
    and the Argo CD button both perform it — so this is a genuine
    break-glass path rather than a second, weaker one. It is here for
    when the CLI is unavailable, not as an everyday alternative:
    approving by hand skips nothing, but it also records nothing about
    who approved beyond the Kubernetes audit log.

    Use the plan's own namespace — `apprafter-system` for platform and
    sourcecredential scope, the application's namespace for application
    scope:

    ```sh
    kubectl get migrationplans -A
    kubectl describe migrationplan <plan-name> -n <namespace>

    kubectl patch migrationplan <plan-name> -n <namespace> \
        --type merge -p '{"status":{"phase":"approved"}}'
    ```

## Approval surfaces — today and later

Two approval surfaces ship today:

- **CLI** — `apprafter migration list/approve/reject` and direct
  `kubectl` access to the `MigrationPlan` CR.
- **Argo CD UI** — a Lua-script resource action ("Approve") on the
  `MigrationPlan` node, plus a Degraded health signal on the affected
  resource. A pending **platform** plan surfaces under the
  platform-stack tree; a pending **application** plan
  surfaces on the app node in the user's own Argo application tree,
  and the app's health goes Degraded with an "awaiting MigrationPlan
  approval" message. Click "Approve" on the plan node to
  approve without leaving the Argo CD console.

Later approval surface (not yet shipped):

- **Backstage** — a MigrationPlan queue view across both scopes with
  approve buttons, surfacing the risk breakdown, estimated downtime,
  and data-volume information from the plan. Follows in the post-launch
  portal bundle.

## Where to look next

- [How the approval gate works](../how-it-works/the-approval-gate.md) — the
  scopes, why only one can be rejected, where a plan lives, and its lifecycle.
- [When a change needs approval](../dev-guide/when-a-change-needs-approval.md) —
  the developer's view of the same gate.
- [Platform management](./platform-management.md) — upgrade strategy, and the
  conditions under which a destructive platform diff is created.
