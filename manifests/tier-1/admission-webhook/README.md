# admission-webhook tier-1 manifest example

`example.yaml` is the deterministic rendering of
`cli-providers::k8s::admission_webhook::admission_webhook_yaml(
"ghcr.io/apprafter/admission-webhook:placeholder")`. It's the same
five-document YAML `platform-cli cluster-bootstrap` applies when
`Infrastructure.spec.admissionWebhook.image` is set.

The webhook validates `apprafter.io/v1alpha1` `Application` objects
on CREATE + UPDATE, beyond what the OpenAPI v3 CRD already does:

- `spec.base.image` (or every `spec.environments[*].image`) must be set.
- Environment names must be DNS-1123 labels.
- `env` keys must match `^[A-Z_][A-Z0-9_]*$`.

To use it, you build the operator image first (see
[`operator/README.md`](../../../operator/README.md)), push it to a
registry, and then add to your Infrastructure manifest:

```cue
spec: admissionWebhook: image: "ghcr.io/<you>/admission-webhook:1.0.0"
```

cert-manager handles cert rotation: the `Certificate` resource
re-issues every 60 days (default), the `kubernetes.io/tls` Secret
gets the new key/cert, and the
`cert-manager.io/inject-ca-from` annotation on the
`ValidatingWebhookConfiguration` keeps `caBundle` in sync — no
restart of the webhook pod is needed for cert rotation, but the
mounted Secret will be re-read on next start.

Refresh `example.yaml` after changing the builder:

```sh
cd cli && \
  nix develop --command cargo run --quiet -p cli-providers --example admission_webhook_example 2>/dev/null \
  | sed -n '/^# SPDX-License-Identifier/,$p' \
  > ../manifests/tier-1/admission-webhook/example.yaml
```
