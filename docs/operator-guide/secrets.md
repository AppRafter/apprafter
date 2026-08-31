---
description: "Sealing a value into a cluster with apprafter secret seal, listing what is sealed and where, replacing a value and seeing which applications it touches."
---

# Secrets

Sealing a value into the cluster is an **operator** task. The CLI
encrypts it against the in-cluster controller's public certificate and
holds no key that can decrypt it, so the plaintext never enters a
manifest, a repository or the CLI's output.

Developers do not run these commands. They write a
`secret: "<name>/<key>"` reference in their manifest and read the result
— see [Secrets](../dev-guide/secrets.md) in the developer guide.

!!! info "What this tool is, and what it is not"

    `apprafter secret` is a **Tier-1 primitive**. SealedSecrets stands in
    for OpenBao on a single node that has no KMS to auto-unseal one, which
    is a deliberate trade recorded in [ADR 0007](../adr/0007-tier-1-sealedsecrets-tier-2-openbao.md).

    It has **no fine-grained access control and no audit trail**. Anyone
    able to seal here already holds every credential in the cluster, so
    the tool draws no boundary and does not pretend to. Tier 2 and above
    replace it with OpenBao, and the tier upgrade migrates what is sealed.

    On Tier 1 the operator and the developer are usually the same person.
    Where a team separates those roles, the separation is enforced by who
    holds a kubeconfig that can create `SealedSecret` objects — not by
    anything inside this command.

## The namespace is the whole guide {#the-namespace}

`apprafter secret seal` defaults to `--namespace apprafter-system`.
That default is correct for *platform* credentials and wrong for an
application secret. A secret an app reaches through a
`secret: "<name>/<key>"` reference must be sealed into **the namespace
the application runs in** — the operator looks for it there and nowhere
else.

Getting this wrong produces no error at seal time. The command succeeds,
the controller unseals a perfectly healthy `Secret`, and the app never
becomes ready with `AppRafter phase: EnvSecretMissing`. The condition
message names the namespace it searched, so the mistake is visible the
moment anyone looks.

!!! warning "A sealed secret cannot be moved or renamed"
    The namespace and the name are mixed into the encryption itself —
    the sealing scope is the literal string `<namespace>/<name>`. A blob
    sealed for `apprafter-system/checkout-secrets` will not decrypt as
    `shop/checkout-secrets`, and no edit changes that. Fixing a
    mis-sealed secret means sealing it again from the original
    plaintext. There is nothing to copy, so keep the value until you
    have confirmed it arrived.

The application's namespace is the `metadata.namespace` of its manifest —
the same value `apprafter app add` uses as the destination.

## Seal a value

```sh
apprafter secret seal checkout-secrets \
    --from-literal stripe-api-key=sk_live_51H... \
    --namespace shop
```

```text
sealedsecret/checkout-secrets applied to namespace shop
```

One `SealedSecret` can hold several keys — repeat `--from-literal`, and
pass **all** of them in a single command:

```sh
apprafter secret seal checkout-secrets \
    --from-literal stripe-api-key=sk_live_51H... \
    --from-literal webhook-signing-secret=whsec_9f... \
    --namespace shop
```

The pair is split on the **first** `=` only, so a value may itself
contain `=` and may be empty. A literal without any `=` is rejected
before the cluster is contacted:

```text
× --from-literal expects KEY=VALUE, got `NOEQ`
```

Two flags round the command out:

- `--type` sets the resulting `Secret`'s type (default `Opaque`) —
  `kubernetes.io/dockerconfigjson` for a registry credential, for
  instance.
- `--stdout` prints the `SealedSecret` YAML instead of applying it, for
  committing to a configuration repository. It still contacts the
  cluster — the controller's public certificate comes from there — but
  writes nothing to it.

The value is a command-line argument, so it lands in your shell history
like any other. There is no file-based input; if that matters, use
whatever your shell offers to suppress the entry.

## See what is sealed, and where

```sh
apprafter secret list
```

```text
Sealed secrets in every namespace:

  NAMESPACE         NAME               SEALED                     KEYS
  apprafter-system  backup-s3          2026-08-14T09:12:44+00:00  S3_ACCESS_KEY_ID, S3_SECRET_ACCESS_KEY
  shop              checkout-secrets   2026-08-31T11:04:02+00:00  stripe-api-key, webhook-signing-secret
```

Key **names** only — nothing is decrypted and no `Secret` is read for
its contents. The names come from the `SealedSecret`'s own
`encryptedData` map.

It searches every namespace by default, deliberately unlike `seal`: you
reach for a listing precisely when you do not know where something was
sealed, and `seal`'s `apprafter-system` default is the single likeliest
reason a secret is in the wrong place. Narrow it with `--namespace`.

`SEALED` is when this CLI last sealed the object. A dash means it was
sealed before that record existed, or applied by other means. It is
**provenance, not attestation**: the value is self-reported by the
machine that ran the command, so it answers "when did this last change"
and authenticates nothing.

## Replace a value

Re-seal under the same name. **Sealing replaces every key — it does not
merge**, so pass all the keys the secret should end up with, not just
the one that changed:

```sh
apprafter secret seal checkout-secrets \
    --from-literal stripe-api-key=sk_live_NEW... \
    --from-literal webhook-signing-secret=whsec_9f... \
    --namespace shop --yes
```

The command then names what it touched:

```text
sealedsecret/checkout-secrets applied to namespace shop

  2 applications in 'shop' resolve this secret: api, checkout
  Their running pods keep the PREVIOUS value: an environment variable
  from a secret is resolved once at pod start and never re-read. They
  pick this value up when they next restart.
```

That list matters because **a secret is not owned by one application**.
Nothing stops several applications in a namespace resolving the same
one, so re-sealing is rarely as local as it looks.

On a terminal the CLI stops and asks before replacing. In a script there
is no prompt to answer, so the command **errors instead of overwriting
silently**; `--yes` is how you say replacement is what you mean.

### Making a rotation take effect

Re-sealing changes the stored value. It does not restart anything, so
pods already running keep the value they started with — indefinitely, if
nothing else restarts them. **This matters most in the case you least
want it to:** re-sealing is what you do to revoke a leaked credential,
and on its own it revokes nothing on the pods still serving traffic.

That is a decision rather than an oversight. Rolling automatically would
mean one seal restarting an unknown set of applications, possibly other
teams', which is why the platform shows you the set instead of acting on
it. The drift is visible from the application side too —
`apprafter app status` marks pods that started before the secret last
changed.

Until a first-class verb exists for it, roll the workload yourself after
a rotation that must take effect:

<!-- docs: check=none reason=known-broken since=v0.2.51 — no first-class verb rolls a workload yet; the platform now SHOWS the drift (app status marks stale pods) but cannot act on it, tracked as D6 -->
```sh
kubectl -n shop rollout restart deployment -l apprafter.io/application=checkout
```

## Remove it

```sh
apprafter secret remove checkout-secrets --namespace shop
```

This deletes the `SealedSecret` and the `Secret` the controller unsealed
from it, so you do not have to remember that there are two objects. It
is idempotent — removing something already gone is not an error — and it
prompts unless `--yes` is passed. Like `seal`, it defaults to
`apprafter-system`, so an application secret needs `--namespace`.

Removing a secret an app still references does not take the app down
immediately: the operator stops updating that app and reports
`EnvSecretMissing`, while the pods already running keep serving. The
next pod to start cannot, because the reference is required rather than
optional — so the app fails at its next restart, deploy or node move
rather than at the moment you deleted the secret.

## Platform credentials, and why the default exists

Not every secret is an application secret. Credentials the *platform*
uses live in `apprafter-system`, which is why that is the default — and
for the two you are likely to meet, a dedicated command seals them for
you, so you should not reach for `secret seal` at all:

- **A private repository or registry** —
  [`apprafter repo creds add`](../dev-guide/private-repos-and-registries.md)
  seals the token and registers it as a credential the operator applies
  to every matching app.
- **Off-site backup** — [`apprafter backup enable`](backup-restore.md)
  seals the object-store credentials as part of enabling the schedule.

## Why seal, rather than create a `Secret` by hand

The operator's check is satisfied by any `Secret` carrying the key, so a
hand-made one does technically work. Two things you lose by taking that
route:

- **The input stops being safe to keep.** A sealed value can be
  committed, reviewed and re-applied; a hand-written secret-creation
  command cannot, so the plaintext ends up somewhere unmanaged or
  nowhere at all.
- **It is not in your backup.** `apprafter backup create` captures a
  `Secret` only when a `SealedSecret` of the same name exists beside it.
  An unsealed `Secret` is skipped, so it will not come back on restore —
  and because sealing is bound to the cluster's own key, a restore
  re-seals every captured secret for the target cluster. See
  [Backup & restore](backup-restore.md).
