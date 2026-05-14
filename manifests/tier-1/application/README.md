# Application CRD tier-1 manifests

`example-crd.yaml` is the deterministic rendering of
`cli-providers::k8s::application_crd::application_crd_yaml()`. It's
the same YAML `apprafter cluster-bootstrap` applies during the
v0.1.22 step — no opt-in required.

`example-app.yaml` is a minimal `kind: Application` manifest. Use it
as a starting point to verify the CRD is registered and OpenAPI v3
validation is active:

```sh
kubectl apply -f example-app.yaml          # → application.apprafter.io/parser created
kubectl get applications.apprafter.io      # → parser in default
```

A deliberately invalid manifest is rejected at admission by the
kube-apiserver alone (no webhook needed for OpenAPI-shape errors).
For example, setting `base.expose.port: 99999` triggers:

```
The Application "parser" is invalid: base.expose.port:
Invalid value: 99999: spec.versions[0].schema.openAPIV3Schema.properties.base.properties.expose.properties.port in body should be less than or equal to 65535
```

Stronger CUE-shaped admission lands with the webhook in v0.1.23.

Refresh `example-crd.yaml` after changing the builder:

```sh
cd cli && \
  nix develop --command cargo run --quiet -p cli-providers --example application_crd_example 2>/dev/null \
  | sed -n '/^# SPDX-License-Identifier/,$p' \
  > ../manifests/tier-1/application/example-crd.yaml
```
