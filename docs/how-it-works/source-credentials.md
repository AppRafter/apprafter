---
description: "What one registered credential is turned into, how a repository or an image gets matched to it, and why narrowing one pauses everything it derives."
---

# Source credentials

What the platform builds out of the single token you register, and how it
decides which repositories and which images that token is for. The recipe is
[Private repos & registries](../dev-guide/private-repos-and-registries.md);
registering a credential needs none of this.

Read it when a clone or a pull failed on a repository you believe is covered,
when `apprafter repo creds show` reports something other than `True`, or when
an edit to a credential appears to have done nothing at all.

The decision is [ADR 0039](../adr/0039-source-credential.md): one object that
carries coverage and no secret material, from which the operator derives every
materialisation. The consequence to hold on to while reading is that none of
the objects below are yours to maintain. They are outputs, and the credential
is the only input.

## What one credential becomes

`apprafter repo creds add` writes exactly two objects, both in
`apprafter-system`: a `SealedSecret` named `srccred-<name>-material` holding
your token, encrypted against the in-cluster controller's public key, and a
`SourceCredential` named `<name>`. The `SourceCredential` carries coverage
only — `spec.git.repoPrefixes` and `spec.registry.hosts` — and points at the
material through `spec.git.backend.sealedSecretRef`. The token is never in it.

The sealed-secrets controller decrypts the sealed blob into an ordinary Secret
of the same name, and the two keys the operator reads from it are `username`
and `password`. A missing or empty `username` becomes `git`, which is what
lets a token-only credential authenticate — Basic auth with an empty username
is rejected outright by the hosts this talks to.

From that one input the SourceCredential controller derives:

| Derived object | Where it lands | What reads it |
| --- | --- | --- |
| `srccred-<name>-repo-<n>` — one per entry in `repoPrefixes`, labelled `argocd.argoproj.io/secret-type: repo-creds` | the `argocd` namespace | Argo CD's repo-server, when it clones |
| `srccred-<name>-dockercfg` — one `dockerconfigjson` covering every entry in `hosts` | `apprafter-system` | the Application controller — it makes the per-namespace copy from this, and authenticates the tag→digest lookup with it |
| `srccred-<name>-pull` — a copy of that one | every workload namespace that pulls through the credential | the kubelet, through the Deployment's `imagePullSecrets` |

The split between the last two is deliberate. The canonical Secret belongs to
the credential; the copy belongs to the applications. The Application
controller makes the copy while rendering a workload whose image the credential
covers and sets `imagePullSecrets` to it in the same pass, so the attachment is
re-decided on every reconcile rather than being a one-time wiring.

A copy is shared: it is named after the credential, not the application, so
several applications in one namespace use one object. Each consuming
application adds a **non-controlling** `ownerReference` to it, which makes
Kubernetes reference-count it — the copy is collected when the last consumer
goes, not the first. Deleting the credential is what withdraws every
derivative: a finalizer sweeps every Secret labelled
`apprafter.io/source-credential: <name>` cluster-wide, in namespaces the
controller cannot enumerate any other way, before the object is allowed to go.

## How a repository or an image is matched to a credential {#prefix-matching}

Nothing in your application manifest names a credential. Two independent
prefix matches decide, performed by two different components.

**The git side is Argo CD's match, not the platform's.** The operator writes
one `repo-creds` Secret per prefix with its `url` set to that prefix,
normalised: a missing scheme becomes `https://`, and a trailing slash comes
off, so `github.com/my-org/` is stored as `https://github.com/my-org`. Argo CD
then picks the credential for a clone by prefix-matching the repository URL
against its registered entries ([ADR 0039](../adr/0039-source-credential.md)).
The platform's part ends at writing the Secret correctly.

**The registry side is the operator's own match.** While rendering an
application it takes the image reference, strips any tag or digest —
`ghcr.io/acme/api:v2` and `ghcr.io/acme/api@sha256:…` both reduce to
`ghcr.io/acme/api` — and looks for a `SourceCredential` in `apprafter-system`
one of whose hosts covers it. Coverage is a prefix match **on a path
boundary**: `ghcr.io/acme` and `ghcr.io/acme/` both cover `ghcr.io/acme/api`,
and neither covers `ghcr.io/acmecorp/api`. The first match in the API listing of
`apprafter-system` wins — two credentials over the same path are not merged,
one is simply used, and which one is not something you get to order. Only `apprafter-system` is searched, which is where
`apprafter repo creds add` puts them.

Inside the derived `dockerconfigjson` the `auths` map is keyed by the registry
**hostname** — the first path segment of each host entry — so a credential
covering two paths on `ghcr.io` contributes one entry, and a pod pulls with one
credential per registry.

The registry host you never typed comes from an inference made once, at
registration, and only for GitHub: a `--url-prefix` under `github.com/<org>`
also registers the registry host `ghcr.io/<org>/`, lowercased, because image
references are lowercase even when the organisation name is not. Every other
git host produces a git-only credential — which is why the recipe tells you to
register a non-GHCR registry with its own explicit prefix.

## What `GitValid` actually asserts {#gitvalid}

Each half reports a `*Present` condition and — once the material has resolved — a `*Valid` one. While the material is still missing, the `*Valid` condition is absent rather than `Unknown`, because there is nothing to probe with.

`GitPresent=True` (reason `Derived`) means the material was readable and the
`repo-creds` Secrets were written. It reads `False` with reason
`MaterialMissing` while the sealed blob has not been unsealed yet — normal for
the first seconds after registration; the controller retries every 15 seconds
until the material appears and settles to every 60 once it has.

`GitValid` is a live probe, and its subject is not the prefix. A prefix names
an organisation, and there is nothing there to authenticate against — so the
controller finds every **representative** repository the prefix actually covers by
reading the repository URL of every Argo CD Application in the cluster and
keeping the ones under the prefix, then makes a git smart-HTTP request against
one with the credential's Basic auth and a ten-second ceiling. `GitValid`
therefore means *this credential can serve the applications that depend on it*,
not *this prefix exists*.

Three verdicts, mapped conservatively on purpose:

- **`True`, reason `Reachable`** — a representative repository answered 2xx.
- **`False`, reason `AuthRejected`** — a representative repository answered
  401 or 403. An explicit rejection is the only thing that produces a false
  verdict. A `404` is **not** one: GitHub and GitLab answer 404 for a private
  repository a token cannot read, and that falls in the `Unknown` bucket
  below — so an Argo CD `404` on a repository you can browse yourself is
  compatible with a healthy `GitValid`.
- **`Unknown`, reason `Unverified`** — everything else: no Argo CD Application
  references a covered prefix yet, the host was unreachable, or it answered
  404 or 5xx. A network failure can never be reported as a bad credential.
  The condition message separates "nothing to probe yet" from "probed and
  could not reach the host" in words.

Across several representatives an explicit rejection wins over any success, so
one bad prefix is not hidden by a good one. `status.lastValidated` advances
only on `True` or `False` — on a cluster with no outbound access it stays at
the last time the credential was genuinely proven, rather than creeping forward
on passes that proved nothing.

`RegistryValid` has the same shape against a representative **image**: every
image an Application declares — the base one and any per-environment override —
that falls under a covered host, probed with a scoped registry v2 token
exchange. Again only an authentication failure is `False`.

`GitValid` is the half the registration gate reads. `RegistryValid` is reported for the operator's own use; nothing in the CLI consumes it.
`apprafter app add --coverage-gate confirmed` refuses an `https` repository
unless some credential covers it **and** reports `GitValid=True`. The default, `present`, is not a gate at all: the application registers either
way, and the CLI prints a notice afterwards only when no credential declares a
covering prefix **and** an anonymous probe finds the repository is not public.
That is the right default precisely because `Unknown` is the honest steady
state on a cluster whose operator has no egress to validate anything.

## Narrowing coverage while an application depends on it {#narrowing}

Removing a prefix or a host — including dropping a whole `git` or `registry`
half — is the one credential change the platform will not simply apply.

The controller keeps the last spec it successfully derived from in
`status.lastAppliedSpec` and diffs every incoming spec against it. Entries
present in that baseline and absent from the new spec are a `coverage-removal`
change, classified `breaking`. Everything else — creating a credential, adding
a prefix, rotating the material, which does not touch the spec at all — is not
destructive and derives immediately. A credential that has no baseline yet
cannot narrow, so a first derivation never gates.

When the diff is non-empty the controller creates a `sourcecredential`-scope
`MigrationPlan` named `<credential>-migration-<timestamp>` in the credential's
own namespace, owned by the credential so it cascades on delete — and then
**derives nothing**. Both halves stop, not only the narrowed one, and the
previously derived wider Secrets are left exactly as they were. That is the
point of pausing rather than applying: an application under the removed prefix
keeps cloning and pulling while a human is still deciding. The gate is on the
object, not on the command — a `kubectl patch` against the credential trips it
identically to the CLI.

What this looks like from outside is worth knowing, because it does not look
like an error. The credential reports `status.phase: AwaitingMigrationApproval`
with `Ready=False` and `MigrationPending=True`, both messages naming the plan.
Those two conditions **replace** the per-half ones for as long as the pause
holds, so `apprafter repo creds show` prints a summary line built from
conditions that are no longer there:

```text
  Status:         pending
    - Ready=False (MigrationPending) paused awaiting approval of MigrationPlan apprafter-system/acme-migration-1757030400
    - MigrationPending=True (MigrationPending) coverage-narrowing gated by MigrationPlan apprafter-system/acme-migration-1757030400
```

Approving the plan lets the controller derive both halves from the narrowed
spec, re-stamp the baseline, and delete the plan — in that order, so a crash
part-way through re-enters and finishes instead of losing the gate. Re-widening
the spec is the other way out: the destructive delta vanishes, the now-stale
plan is deleted, and derivation resumes. It is the *only* other way out,
because a `sourcecredential`-scope plan cannot be rejected — see
[How the approval gate works](the-approval-gate.md). If the plan itself ends in
`failed`, the credential reports `MigrationFailed=True` and stays paused until
that plan is deleted by hand.

One residue survives approval. The git half's Secrets are addressed by
position — `srccred-<name>-repo-0`, `-repo-1`, and so on — and each derivation
writes the positions the current list has. Shortening the list therefore leaves
the trailing Secret in `argocd` holding whatever it last held; it is collected
when the credential is deleted, not when its prefix is. The registry half has
no equivalent, being a single Secret rewritten with only the surviving hosts.

`e2e/sourcecredential-migration-walk.sh` exercises this chain end to end on a
local cluster: seed, narrow, observe the pause with the wider Secrets
byte-identical, approve, and re-derive.

## See also

- [Private repos & registries](../dev-guide/private-repos-and-registries.md) —
  the recipe, and the GitHub token type both halves have to satisfy.
- [Migration plans](../operator-guide/migration-plans.md) — approving the plan
  a narrowing raises, and the other changes that gate.
- [How the approval gate works](the-approval-gate.md) — why a credential-scope
  plan is approve-only.
- [The image digest](the-image-digest.md) — the same derived credential is what
  lets the operator read a private registry to resolve a tag.
- [GitOps and the CUE plugin](gitops-and-the-cue-cmp.md) — what Argo CD does
  with the repository once it can clone it.
- [ADR 0039](../adr/0039-source-credential.md) — the decision: config-only
  credential object, sealed material, operator-derived outputs.
