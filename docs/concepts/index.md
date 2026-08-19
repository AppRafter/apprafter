---
description: "The AppRafter object model at a glance — which custom resource each role owns, and the order to read them in."
---

# Concepts

> **Status:** stub. Each concept still needs its own page.

The AppRafter object model — the CRDs that developers and operators
interact with directly. The v1alpha1 schemas in `schemas/v1alpha1/`
are the source of truth for every field.

| Concept                  | Owner   | What it is for |
| ------------------------ | ------- | -------------- |
| `Application`            | Dev     | The unit of deployment. |
| `ServiceProvider`        | Operator| A backing service a `needs` entry can resolve to. |
| `ResourceClaim`          | Operator (auto-generated) | One application's claim on one backing service. |
| `AccessGrant`            | Operator| How humans get into the cluster. |
| `ExternalSurface`        | Operator| What the platform manages outside the cluster — git host, registry, access plane, monitoring, backups. |
| `ServiceProviderPlugin`  | Community | A new service type, shipped as a gRPC plugin sidecar. |
| `Infrastructure`         | Operator| The substrate the platform runs on — provider, nodes, network, firewall, OS image. |
| `MigrationPlan`          | Operator (auto-generated) | The approval gate a destructive change waits behind. |

## Reading order

1. Start with **`Application`** — the unit of deployment.
2. Then **`ServiceProvider` + `ResourceClaim`** — how `needs`
   resolve to backing services.
3. Then **`AccessGrant`** — how humans get into the cluster.
4. Then **`ExternalSurface`** — what the platform manages outside
   the cluster.
5. **`MigrationPlan`** is the safety net for destructive changes.
