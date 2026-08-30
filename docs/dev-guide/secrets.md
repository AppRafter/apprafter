---
description: "Sealing an application secret into the namespace its app runs in, binding it to an env-var, and reading EnvSecretMissing when the binding does not resolve."
---

# Secrets

An application secret — a payment API key, a webhook signing secret, a
token for a third-party service — is sealed into the cluster with
`apprafter secret seal` and bound to an environment variable from your
manifest. The plaintext never enters your manifest, your repository or
the CLI's output: the CLI encrypts it against the in-cluster
controller's public certificate and holds no key that can decrypt it.

**One decision determines whether any of that works: the namespace.**

## The namespace is the whole guide {#the-namespace}

`apprafter secret seal` defaults to `--namespace apprafter-system`.
That default is correct for *platform* credentials and wrong for an
application secret. A secret your app reaches through a
`secret: "<name>/<key>"` reference must be sealed into **the namespace
the application runs in** — the operator looks for it there and
nowhere else.

Getting this wrong produces no error at seal time. The command
succeeds, the controller unseals a perfectly healthy `Secret`, and the
app never becomes ready:

```text
AppRafter phase: EnvSecretMissing
```

!!! warning "A sealed secret cannot be moved or renamed"
    The namespace and the name are mixed into the encryption itself —
    the sealing scope is the literal string `<namespace>/<name>`. A
    blob sealed for `apprafter-system/checkout-secrets` will not
    decrypt as `shop/checkout-secrets`, and no `kubectl` edit changes
    that. Fixing a mis-sealed secret means sealing it again from the
    original plaintext. There is nothing to copy, so keep the value
    until you have confirmed it arrived.

### Which namespace is that?

Your application's namespace is the `metadata.namespace` of its
manifest — the same value `apprafter app add` uses as the destination
namespace (its wizard preselects the manifest's, and `--namespace`
overrides it). If neither says otherwise, it is `apprafter`.

For an app that is already registered, read it rather than assume it:

```sh
apprafter app status checkout
```

```text
Application argocd/checkout
  project:       apps
  repo:          https://github.com/my-org/checkout
  revision:      main
  path:          /
  destination:   shop
  environment:   (base)
  sync state:    Synced
  health:        Healthy
```

`destination:` is the namespace to seal into — `shop` here. The
examples below use it throughout.

## Seal a value

```sh
apprafter secret seal checkout-secrets \
    --from-literal stripe-api-key=sk_live_51H... \
    --namespace shop
```

```text
sealedsecret/checkout-secrets applied to namespace shop
```

One `SealedSecret` can hold several keys — repeat `--from-literal`,
and pass **all** of them in a single command:

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
- `--stdout` prints the `SealedSecret` YAML instead of applying it,
  for committing to a configuration repository. It still contacts the
  cluster — the controller's public certificate comes from there — but
  writes nothing to it.

The value is a command-line argument, so it lands in your shell
history like any other. There is no file-based input; if that matters
to you, use whatever your shell offers to suppress the entry.

## Reference it from the manifest

An `env` value is either a literal string or a reference. A
`secret: "<name>/<key>"` reference names the `Secret` before the `/`
and the key inside it after:

```cue
// apprafter/Application.cue
package apprafter

import v1alpha1 "apprafter.io/schemas/v1alpha1"

app: v1alpha1.#Application & {
    metadata: {
        name:      "checkout"
        namespace: "shop"       // ← the namespace you sealed into
    }
    spec: base: {
        image:    "ghcr.io/my-org/checkout:1.4.0"
        replicas: 1
        expose: {
            port:    8080
            network: "internal"
        }
        env: {
            LOG_LEVEL:  "info"
            STRIPE_KEY: secret: "checkout-secrets/stripe-api-key"
        }
    }
}
```

The operator turns each reference into a Kubernetes `secretKeyRef` on
the container, so the plaintext is injected by the kubelet at pod
start and appears in no rendered manifest. **You choose the env-var
name** — `STRIPE_KEY` above has nothing to do with the key
`stripe-api-key`; the reference is what connects them.

The same form works inside a per-environment override
(`spec.environments.<env>.env`). A reference always resolves in the
namespace of the deployment being reconciled, so two environments
deployed into two namespaces need the value sealed twice, once into
each — and such a manifest leaves `metadata.namespace` out so that each
deployment can take its own. See [Deploying more than one
environment](environments.md).

Claim references — `claim.pg.url` and friends, which bind a
provisioned backend's connection fields — are a different mechanism
with no sealing step at all. Both are covered together in
[Writing Application.cue](application-cue.md#referencing-claims-and-secrets).

## Check that it arrived

```sh
apprafter app status checkout
```

A healthy app prints `AppRafter phase: Ready`. Anything else and the
phase names the reason — `EnvSecretMissing` is the one this page is
about, and [When it goes wrong](#when-it-goes-wrong) takes it from
there.

??? note "Listing the sealed keys directly"

    `apprafter secret` has no listing subcommand yet, so confirming *which keys*
    landed means reading the unsealed `Secret` — key names only, never
    values:

    ```sh
    kubectl -n shop get secret checkout-secrets \
        -o go-template='{{range $k, $v := .data}}{{$k}}{{"\n"}}{{end}}'
    ```

    ```text
    stripe-api-key
    webhook-signing-secret
    ```

    `NotFound` here means nothing was unsealed into `shop` at all.

## Replace a value

Re-seal under the same name. **Sealing replaces every key — it does
not merge**, so pass all the keys the secret should end up with, not
just the one that changed:

```sh
apprafter secret seal checkout-secrets \
    --from-literal stripe-api-key=sk_live_NEW... \
    --from-literal webhook-signing-secret=whsec_9f... \
    --namespace shop
```

On a terminal the CLI stops and asks first:

```text
A secret named 'checkout-secrets' already exists in 'shop'. Sealing
REPLACES its keys (it does NOT merge — keys not in this command are
dropped). Continue? [y/N]
```

In a script there is no prompt to answer, so the command **errors
instead of overwriting silently**; pass `--yes` when replacement is
what you mean.

!!! warning "Known gap: re-sealing does not reach the running pods"

    The rendered Deployment still points at the same `Secret` and the
    same key, so **nothing about the workload changed** and no rollout
    is triggered. Pods already running keep the value they started
    with, for as long as they keep running — indefinitely, if nothing
    else restarts them.

    **This matters most in the case you least want it to.** Re-sealing
    is what you do to revoke a leaked credential, and on its own it
    revokes nothing on the pods still serving traffic. The application
    stays `Ready` throughout, because from the platform's side nothing
    happened.

    Until the platform rolls the workload for you — tracked as a defect
    — force it after any rotation that must take effect:

    <!-- docs: check=none reason=known-broken since=v0.2.51 — the workaround for a tracked defect: re-sealing does not reach running pods, so this stands until the operator rolls the workload itself -->
    ```sh
    kubectl -n shop rollout restart deployment -l apprafter.io/application=checkout
    ```

## Remove it

```sh
apprafter secret remove checkout-secrets --namespace shop
```

This deletes the `SealedSecret` and the `Secret` the controller
unsealed from it, so you do not have to remember that there are two
objects. It is idempotent — removing something that is already gone is
not an error — and it prompts unless `--yes` is passed. Like `seal`,
it defaults to `apprafter-system`, so an application secret needs
`--namespace`.

Removing a secret an app still references does not take the app down
immediately: the operator stops updating that app and reports
`EnvSecretMissing`, while the pods already running keep serving. The
next pod to start cannot, because the reference is required rather
than optional — so the app fails at its next restart, deploy or node
move rather than at the moment you deleted the secret.

## When it goes wrong {#when-it-goes-wrong}

There are two failure surfaces and they are separated by *when* they
fire.

### A malformed reference is rejected on the way in

The shape of the reference — a lowercase DNS-1123 `Secret` name, a
`/`, then a key of `[-._a-zA-Z0-9]` characters — is enforced when the
`Application` is admitted, before anything is stored:

```text
Application is invalid: spec.base.env.STRIPE_KEY: secret ref
"checkout-secrets" is malformed; expected "<name>/<key>" (a DNS-1123
Secret name, a '/', then a key matching [-._a-zA-Z0-9]+)
```

Argo CD surfaces this as a failed sync on the app, and nothing about
the running workload changes.

**`apprafter app validate` does not catch this.** It checks your
manifest against the schema, and the schema types a `secret` reference
as a string — a value with no `/` in it is a perfectly good string.
Local validation catches typos in field *names*; the reference's
internal shape is checked by the cluster.

### An unresolvable reference stops the reconcile

If the reference is well-formed but nothing answers it, the operator
sets the app's phase to `EnvSecretMissing` and stops there. It does
**not** render or apply children while a reference is unresolved: on a
first deploy that means no pods at all, and on an update it means the
previous pods keep running the previous spec until the reference
resolves.

`apprafter app status <name>` shows the phase. The message naming the
variable reads:

```text
env STRIPE_KEY → secret "checkout-secrets/stripe-api-key": Secret
"checkout-secrets" not found or missing key "stripe-api-key"
```

??? note "Reading the condition directly"

    ```sh
    kubectl -n shop get application.apprafter.io checkout \
        -o jsonpath='{.status.conditions[?(@.type=="Ready")].message}'
    ```

    Write `application.apprafter.io` in full: AppRafter's own CRD
    shadows Argo CD's `applications.argoproj.io` on the short name, so a
    bare `application` is ambiguous.

### Telling a wrong namespace from a wrong key

That message covers both causes with one sentence — "not found **or**
missing key" — and does not distinguish them.

**Start with the namespace, because that is the likely one.** `seal`
defaults to `apprafter-system`, which is right for platform credentials
and wrong for an application secret, so sealing into the wrong namespace
is the most common way to arrive here. If that is what happened, seal it
again into the app's namespace and delete the stray copy — sealed
secrets are bound to the namespace they were sealed for, so re-sealing
is the fix rather than moving:

```sh
apprafter secret seal checkout-secrets \
    --from-literal stripe-api-key=sk_live_51H... \
    --namespace shop
apprafter secret remove checkout-secrets --namespace apprafter-system --yes
```

**If it was already in the right namespace**, the name resolved and the
key did not. A key spelled `stripe_api_key` in the secret and
`stripe-api-key` in the manifest fails exactly as a missing secret does.
Fix whichever side is wrong and the operator picks it up on its next
pass, with no further action.

!!! note "Known gap: telling the two apart needs `kubectl`"

    `apprafter secret` has no listing subcommand, so the platform raises a
    question — "not found *or* missing key" — that it gives you no way
    to answer. Tracked as a defect. Until it lands:

    <!-- docs: check=none reason=known-broken since=v0.2.51 — the workaround for a tracked defect: there is no `apprafter secret list`, so the two questions EnvSecretMissing raises have no first-class answer -->
    ```sh
    # where is it sealed?
    kubectl get sealedsecrets --all-namespaces

    # what keys does it actually carry?
    kubectl -n shop get secret checkout-secrets \
        -o go-template='{{range $k, $v := .data}}{{$k}}{{"\n"}}{{end}}'
    ```

    Appearing in no namespace at all means it was never sealed; go back
    to [Seal a value](#seal-a-value).

## Platform credentials, and why the default exists

Not every secret is an application secret. Credentials the *platform*
uses live in `apprafter-system`, which is why that is the default — and
for the two you are likely to meet, a dedicated command seals them for
you, so you should not reach for `secret seal` at all:

- **A private repository or registry** —
  [`apprafter repo creds add`](private-repos-and-registries.md) seals
  the token and registers it as a credential the operator applies to
  every matching app.
- **Off-site backup** —
  [`apprafter backup enable`](../operator-guide/backup-restore.md)
  seals the object-store credentials as part of enabling the schedule.

## Why seal, rather than create a `Secret` by hand

The operator's check is satisfied by any `Secret` carrying the key, so
a hand-made one does technically work. Two things you lose by taking
that route:

- **The input stops being safe to keep.** A sealed value can be
  committed, reviewed and re-applied; a `kubectl create secret`
  invocation cannot, so the plaintext ends up somewhere unmanaged or
  nowhere at all.
- **It is not in your backup.** `apprafter backup create` captures a
  `Secret` only when a `SealedSecret` of the same name exists beside
  it. An unsealed `Secret` is skipped, so it will not come back on
  restore — and because sealing is bound to the cluster's own key, a
  restore re-seals every captured secret for the target cluster. See
  [Backup & restore](../operator-guide/backup-restore.md).
