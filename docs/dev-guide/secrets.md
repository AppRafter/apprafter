---
description: "Binding an application secret to an env-var from your manifest, confirming it resolved, and reading EnvSecretMissing when it did not."
---

# Secrets

An application secret — a payment API key, a webhook signing secret, a
token for a third-party service — is referenced from your manifest by
name and key. The plaintext never enters your manifest or your
repository: an operator seals it into the cluster, and the kubelet
injects it into your container at pod start.

**You do not seal it yourself.** Sealing is an operator task with its own
page — [Secrets](../operator-guide/secrets.md) in the operator guide. On
a Tier-1 cluster the operator is often the same person as the developer,
so that page may well be your next stop; it is separate because the
decisions on it (which namespace, who else consumes this, when to roll)
belong to whoever runs the cluster.

What you need from them is two facts: **the secret's name** and **the
key inside it**. Both are visible with `apprafter secret list`.

## Reference it from the manifest

An `env` value is either a literal string or a reference. A
`secret: "<name>/<key>"` reference names the `Secret` before the `/` and
the key inside it after:

```cue
// apprafter/Application.cue
package apprafter

import v1alpha1 "apprafter.io/schemas/v1alpha1"

app: v1alpha1.#Application & {
    metadata: {
        name:      "checkout"
        namespace: "shop"       // ← the namespace it was sealed into
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
the container, so the plaintext is injected by the kubelet at pod start
and appears in no rendered manifest. **You choose the env-var name** —
`STRIPE_KEY` above has nothing to do with the key `stripe-api-key`; the
reference is what connects them.

The same form works inside a per-environment override
(`spec.environments.<env>.env`). A reference always resolves in the
namespace of the deployment being reconciled, so two environments
deployed into two namespaces need the value sealed twice, once into each
— and such a manifest leaves `metadata.namespace` out so that each
deployment can take its own. See
[Deploying more than one environment](environments.md).

Claim references — `claim.pg.url` and friends, which bind a provisioned
backend's connection fields — are a different mechanism with no sealing
step at all. Both are covered together in
[Writing Application.cue](application-cue.md#referencing-claims-and-secrets).

## Check that it arrived

```sh
apprafter app status checkout
```

A healthy app prints `AppRafter phase: Ready`, and the `Secrets` section
lists what it resolves:

```text
AppRafter phase: Ready

Secrets (shop/checkout):
  ENV         SECRET/KEY                        (SCOPE)
  STRIPE_KEY  checkout-secrets/stripe-api-key   (base)
```

That section is the app-side half of the same index
`apprafter secret list` reads the other way round — it shows what *this*
application consumes, where the listing shows where things are sealed.

### Pods running an older value

`app status` marks in yellow any pod that started **before** this
application's secrets last changed:

```text
Workload pods (shop, app.kubernetes.io/name=checkout):
  NAME                      READY  STATUS   RESTARTS  AGE
  checkout-7d9f4b8c-2xk4t   1/1    Running  0         3h  ← old config
```

An environment variable sourced from a secret is resolved once when the
container starts and is never re-read, so a pod older than the change is
still serving the previous value. Nothing is broken — the pod is healthy
— but it is not running what the secret now says. Restarting the
workload is what picks the new value up; ask whoever rotated it, since
the decision of when to roll belongs with them.

## When it goes wrong {#when-it-goes-wrong}

There are two failure surfaces and they are separated by *when* they
fire.

### A malformed reference is rejected on the way in

The shape of the reference — a lowercase DNS-1123 `Secret` name, a `/`,
then a key of `[-._a-zA-Z0-9]` characters — is enforced when the
`Application` is admitted, before anything is stored:

```text
Application is invalid: spec.base.env.STRIPE_KEY: secret ref
"checkout-secrets" is malformed; expected "<name>/<key>" (a DNS-1123
Secret name, a '/', then a key matching [-._a-zA-Z0-9]+)
```

Argo CD surfaces this as a failed sync on the app, and nothing about the
running workload changes.

**`apprafter app validate` does not catch this.** It checks your manifest
against the schema, and the schema types a `secret` reference as a string
— a value with no `/` in it is a perfectly good string. Local validation
catches typos in field *names*; the reference's internal shape is checked
by the cluster.

### An unresolvable reference stops the reconcile

If the reference is well-formed but nothing answers it, the operator sets
the app's phase to `EnvSecretMissing` and stops there. It does **not**
render or apply children while a reference is unresolved: on a first
deploy that means no pods at all, and on an update it means the previous
pods keep running the previous spec until the reference resolves.

`apprafter app status <name>` shows the phase, and the message says which
of the two things went wrong.

**The secret is not where the app looks.** Almost always this is the
namespace: `secret seal` defaults to `apprafter-system`, which is right
for platform credentials and wrong for an application secret.

```text
env STRIPE_KEY → secret "checkout-secrets/stripe-api-key": no Secret
"checkout-secrets" in namespace "shop"
```

`apprafter secret list` shows where it actually is. Because sealing is
bound to `<namespace>/<name>`, the fix is to seal it again into the
right namespace rather than move it — an operator task.

**The secret is there and the key is not.** The message lists the keys it
does carry, which usually makes the answer obvious:

```text
env STRIPE_KEY → secret "checkout-secrets/stripe-api-key": Secret
"checkout-secrets" exists in namespace "shop" but carries no key
"stripe-api-key" (it carries: stripe_api_key, webhook-signing-secret)
```

An underscore where the manifest has a hyphen fails exactly as a missing
secret does. Fix whichever side is wrong — your reference or the sealed
key — and the operator picks it up on its next pass, with no further
action.

??? note "Reading the condition directly"

    ```sh
    kubectl -n shop get application.apprafter.io checkout \
        -o jsonpath='{.status.conditions[?(@.type=="Ready")].message}'
    ```

    Write `application.apprafter.io` in full: AppRafter's own CRD shadows
    Argo CD's `applications.argoproj.io` on the short name, so a bare
    `application` is ambiguous.
