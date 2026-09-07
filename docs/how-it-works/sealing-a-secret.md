---
description: "What a seal produces and what the cluster does with it, why the namespace it was sealed into decides whether an application can read it, and when the value actually reaches a container."
---

# Sealing a secret

What happens between `apprafter secret seal` and an environment variable inside
a running container. The recipe is [Secrets](../operator-guide/secrets.md) in
the operator guide; none of this is needed to run it.

Read it when a seal reported success and the application still reports
`EnvSecretMissing`, or when you replaced a value and the running pods went on
serving the old one.

The decision is
[ADR 0007](../adr/0007-tier-1-sealedsecrets-tier-2-openbao.md): a Tier-1 node
has no KMS to auto-unseal OpenBao with, so Tier 1 ships SealedSecrets as a
deliberate stand-in — a primitive chosen for its footprint, and therefore not
asked to carry an authorization story.

## What a seal produces

The CLI does the encryption itself; no external sealing binary is involved. It
first reads the sealed-secrets controller's public certificate through the
apiserver's service proxy —
`/api/v1/namespaces/apprafter-system/services/http:sealed-secrets-controller:http/proxy/v1/cert.pem`
— so the certificate arrives over the same TLS-authenticated connection every
other cluster call uses rather than over the pod network, and the Service name
is deterministic because the platform-stack component pins it with
`fullnameOverride`.

Each `--from-literal` value is then encrypted with a random, single-use
AES-256-GCM session key, and that session key is wrapped with RSA-OAEP
(SHA-256) against the controller's public key. The two are concatenated — a
two-byte big-endian length, the RSA block, then the GCM ciphertext — and
base64-encoded into the `SealedSecret`'s `encryptedData` map, one entry per
key. Only the controller's in-cluster private key reverses that, so the CLI
cannot decrypt what it has just produced.

The object it writes is a `SealedSecret` (`bitnami.com/v1alpha1`) whose
`template` block records the name, the namespace and the type (`Opaque` unless
`--type` says otherwise) that the unsealed `Secret` will take. The command adds
two annotations of its own: `apprafter.io/sealed-by`, read from `USER` or
`USERNAME` and `unknown` when neither is set, and `apprafter.io/sealed-at`, the
current time. Those are stamped by the command rather than by the builder the
restore path shares, so a machine re-seal during a restore is not attributed to
whoever triggered it.

`--stdout` prints the object instead of applying it. It still reads the
certificate from the cluster, because that is where the key is, but writes
nothing back — and it skips the existing-secret check, which only matters when
something is about to be replaced.

`apprafter secret list` reads key **names** out of the `SealedSecret`'s own
`encryptedData` map. Nothing is decrypted and no `Secret` is read for its
contents, which is why the listing is safe to run anywhere.

## What the cluster does with it

The sealed-secrets controller runs in `apprafter-system`, installed by the
platform stack at sync wave -8 — ahead of the operator at wave 0, so the CRD
and the controller exist before anything is sealed against them. It watches
`SealedSecret` objects, decrypts each one with its private key, and writes an
ordinary Kubernetes `Secret` under the name and namespace the `template` block
carries.

The `SealedSecret` is the source of truth for that `Secret`. Deleting the
`Secret` alone achieves nothing — the controller writes it back from the
ciphertext it still holds. That is why `apprafter secret remove` deletes the
`SealedSecret` first, taking the `Secret` the controller owns with it, and only
then deletes the `Secret` by name as well, which covers a plain one that was
never sealed. Both deletes ignore a missing object, so the command is
idempotent.

## Why the namespace decides whether a reference resolves {#the-scope-binding}

The namespace binds twice, independently, and either binding alone would be
enough to make a mis-sealed value unreadable.

**The ciphertext is bound to it.** The RSA-OAEP label is the literal string
`<namespace>/<name>` — the same `namespace + "/" + name` the controller
computes on the other side. A different namespace, or a different name, is a
different label, and RSA-OAEP decryption rejects it. That is what makes a
mis-sealed blob unmovable: there is no edit to make, because the scope is
inside the encryption rather than beside it.

**The lookup is bound to it too.** The operator renders a
`secret: "<name>/<key>"` reference into a `valueFrom.secretKeyRef` marked
non-optional, and a `secretKeyRef` names a `Secret` in the pod's own namespace
— Kubernetes has no cross-namespace form. Before rendering anything, the
Application controller reads the referenced `Secret` through a client scoped to
the `Application`'s own namespace.

When that read comes back empty the controller **stops**. It sets
`status.phase` to `EnvSecretMissing`, emits a `Ready=False` condition with
reason `EnvSecretMissing`, and requeues in 30 seconds without rendering or
applying any child object. On a first deploy that means no pods at all; on an
update the previously applied pods keep running the previous spec.

The condition message separates the two ways a reference can fail and names the
namespace it searched, because sealing into the wrong one is the likeliest way
to arrive here:

```text
env STRIPE_KEY → secret "checkout-secrets/stripe-api-key": no Secret
"checkout-secrets" in namespace "shop"
```

```text
env STRIPE_KEY → secret "checkout-secrets/stripe-api-key": Secret
"checkout-secrets" exists in namespace "shop" but carries no key
"stripe-api-key" (it carries: stripe_api_key, webhook-signing-secret)
```

Key names are listed; values never are.

One consequence sits at the other end of the process: the namespace has to
exist **when you seal**, not later when Argo CD creates it. The CLI applies the
`SealedSecret` with an ordinary apply, and the apiserver refuses a namespaced
object whose namespace is absent.

## When the value is read {#when-the-value-is-read}

Once, when the container starts. The plaintext never enters the pod template —
what is rendered there is a `secretKeyRef` — and the kubelet resolves it at
container start. Kubernetes does not re-read an environment variable
afterwards, so re-sealing replaces what is stored and changes nothing about a
process that is already running.

The operator does read the values, but only to hash them. In the same pass that
checks each reference exists, it feeds each `(secret, key, value)` triple into
a SHA-256 — sorted by secret and key, with every field's length mixed in — and
drops the values; they are never logged, stored or returned. The hash lands in
`status.envConfig.digest`, and `status.envConfig.changedAt` moves **only when
the digest moves**, which is what makes it a drift boundary rather than a
heartbeat. There is no digest while an application binds no `secret:`
references, or while any one of them is unresolved.

That digest is deliberately not on the pod template. Putting it there would
roll the Deployment on every change — an automatic restart of every application
that resolves the secret, and a secret is not owned by one application, so the
set is not knowable to whoever sealed it. The drift is therefore made visible
instead of acted on: `apprafter app status` flags any pod whose start time
precedes `status.envConfig.changedAt`. Both sides are compared at whole
seconds, because a pod's start time is serialised truncated to the second while
`changedAt` carries nanoseconds; comparing them raw marked every freshly
deployed pod as stale.

The signal is not instant. The Application controller watches applications,
resource claims and migration plans — not `Secret`s — and a healthy reconcile
requeues after 60 seconds, so a re-seal shows up in the digest on the next pass
rather than at the moment the controller unseals it.

??? note "Reading the digest directly"

    ```sh
    kubectl -n <namespace> get application.apprafter.io <app-name> \
      -o jsonpath='{.status.envConfig}'
    ```

    Spell `application.apprafter.io` out in full: AppRafter's CRD and Argo CD's
    `applications.argoproj.io` share the plural `applications`, so a bare
    `application` is ambiguous.

The seal closes the loop from the other side. After applying, the command lists
the applications in that namespace whose `spec.base.env` or `spec.environments`
bind this secret, and says plainly that their running pods keep the previous
value. That lookup is best-effort: if the applications cannot be listed it
prints nothing, because failing a seal that has already happened would be worse
than staying quiet.

## What the platform does not do {#what-it-does-not-do}

**It does not rotate anything.** Re-sealing *is* the rotation, and it restarts
nothing. No value expires on its own, and nothing issues a short-lived or
per-workload credential — SealedSecrets has no dynamic-credential mechanism,
which [ADR 0007](../adr/0007-tier-1-sealedsecrets-tier-2-openbao.md) records as
a known cost of the Tier-1 choice rather than a gap to be filled there.

**It draws no access boundary and keeps no audit trail.**
`apprafter.io/sealed-by` is self-reported by the machine that ran the command
and editable by anyone who can seal, so it is provenance and not attestation:
it answers "when did this last change, and roughly by whom" and authenticates
nothing. Anyone able to seal already holds every credential in the cluster.
Where a team does separate operator from developer on Tier 1, [ADR 0007](../adr/0007-tier-1-sealedsecrets-tier-2-openbao.md) puts the
separation in Kubernetes RBAC — who holds a kubeconfig that can create
`SealedSecret` objects — rather than anywhere inside this command.

**What replaces it.** [ADR 0007](../adr/0007-tier-1-sealedsecrets-tier-2-openbao.md) puts OpenBao at Tier 2 and above, where a
secret's revision is observable and the identity making a change is
authenticated — the two properties whose absence is why an automatic restart is
not the Tier-1 default, and whose presence would make an *offered* restart
reasonable. Tier upgrades are not implemented in this release:
`apprafter upgrade-tier` validates its target and prints the move it would
make, without changing anything. The same ADR describes a second Tier-1
delivery mode — the ciphertext inline in the `Application` manifest, which
would turn a credential change into a reviewable spec diff that the approval
gate already inspects — as coherent, considered, and deferred.

## See also

- [Secrets](../operator-guide/secrets.md) — the operator recipe: sealing,
  listing, replacing and removing a value.
- [Secrets](../dev-guide/secrets.md) — the developer half: writing the
  `secret:` reference, and reading `EnvSecretMissing` when it does not resolve.
- [Per-environment deploy](per-environment-deploy.md) — why two environments in
  two namespaces need the same value sealed twice.
- [How a restore replays a backup](how-a-restore-works.md) — why a restore
  re-seals captured material against the target cluster instead of copying it.
- [How the approval gate works](the-approval-gate.md) — the gate that covers a
  manifest edit, and which a re-seal deliberately does not pass through.
- [ADR 0007](../adr/0007-tier-1-sealedsecrets-tier-2-openbao.md) — why
  SealedSecrets at Tier 1, and why sealing is an operator surface rather than a
  developer one.
