---
description: "The AppRafter object model at a glance — which custom resource each role owns, and the order to read them in."
---

# Concepts

> **Status:** stub. Each concept gets its own page in phase 8.2.

The AppRafter object model — the CRDs that developers and operators
interact with directly. See `spec.md` §3 for the complete
descriptions; the v1alpha1 schemas live in `schemas/v1alpha1/`.

| Concept                  | Owner   | Section in spec.md |
| ------------------------ | ------- | ------------------ |
| `Application`            | Dev     | §3.1               |
| `ServiceProvider`        | Operator| §3.2               |
| `ResourceClaim`          | Operator (auto-generated) | §3.3 |
| `AccessGrant`            | Operator| §3.4               |
| `ExternalSurface`        | Operator| §3.5               |
| `ServiceProviderPlugin`  | Community | §3.6             |
| `Infrastructure`         | Operator| §3.7               |
| `MigrationPlan`          | Operator (auto-generated) | §3.8 |

## Reading order

1. Start with **`Application`** — the unit of deployment.
2. Then **`ServiceProvider` + `ResourceClaim`** — how `needs`
   resolve to backing services.
3. Then **`AccessGrant`** — how humans get into the cluster.
4. Then **`ExternalSurface`** — what the platform manages outside
   the cluster.
5. **`MigrationPlan`** is the safety net for destructive changes.
