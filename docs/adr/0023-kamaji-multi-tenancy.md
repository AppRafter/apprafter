# ADR 0023: Kamaji as hard multi-tenancy mechanism; Capsule as policy layer

## Status

Accepted (2026-05-12).

## Context

AppRafter's security values (CoCo, OpenBao, workload identity, default-deny network) imply that multi-tenancy should provide hard isolation by structure, not soft isolation by policy. Multiple tenant scenarios were identified:

- **Per-environment isolation** within one customer (dev/staging/prod of the same team).
- **Multi-team isolation** within one organisation (several teams of one customer).
- **Multi-customer isolation** (MSP scenario from Andrey-A audience: one AppRafter instance hosting multiple unrelated customers).
- **Managed Ops Plane A/B separation** (we trust ourselves for management plane; we do **not** trust ourselves for customer data plane).
- **Turnkey customer cluster hosting** (one AppRafter HQ provisions and operates clusters for many customers).

Four mainstream Kubernetes multi-tenancy mechanisms were considered:

| Tool | Type | Cluster-admin scope | Insider attack surface |
|---|---|---|---|
| Capsule | Soft (policy CRD) | Host cluster-admin = god | High (shared API server) |
| vCluster | Hard (per-tenant k3s control plane in pod) | Vcluster-admin ≠ host-admin | Medium (syncer mediates) |
| Kamaji | Hard (per-tenant kube-apiserver pod, shared datastore) | Tenant-admin ≠ host-admin | Low (separate API server pod) |
| HNC | Soft (namespace hierarchy) | Host cluster-admin = god | High |

The initial framing was "Capsule as foundation, vCluster as opt-in for hard cases". That framing was reconsidered after security threat model recalibration: Capsule alone fails the MSP scenario, vCluster has higher insider attack surface than Kamaji, and operating two mechanisms (Capsule + vCluster) adds complexity without security gain.

## Decision

**Kamaji** is the single hard multi-tenancy mechanism. **Capsule** is a layered policy enforcement layer inside each tenant. **vCluster, HNC, and bare RBAC** are not adopted.

### Per-tier behaviour

| Tier | Hard multi-tenancy | Capsule policy layer | Note |
|---|---|---|---|
| **T1** | Not possible (single-node) | Default, opt-out | Soft mt only: Capsule + default-deny NetworkPolicy + workload identity. Solo founders migrate to T2 with same manifests to get hard mt. |
| **T2+** | Kamaji default, opt-out | Default, opt-out | Per AppRafter Tenant = Kamaji TenantControlPlane. |
| **T3** | Kamaji + selective CoCo for sensitive workloads | Default | CoCo orthogonal opt-in. |
| **T4** | Kamaji + CoCo default for confidential workloads | Default | Maximum isolation. |

### Datastore

**Default: integrated Postgres via CloudNativePG.** Kamaji's datastore is a `ResourceClaim` on the `pg-integrated` provider. This is the same primitive applications use; Kamaji eats its own dogfood:

```cue
platformServices: {
    kamaji: {
        required: true
        minTier: 2
        datastore: {
            type: pg
            selector: {tier: integrated}
        }
    }
}
```

Backups via CNPG continuous backup automatically cover Kamaji state. HA on CNPG cluster gives Kamaji HA datastore for free.

**Research item: kine+NATS as Kamaji datastore.** Kamaji officially supports MySQL/Postgres/etcd. kine supports a NATS backend. Combining kine+NATS as a backend for Kamaji is not officially validated. Experimental research deferred to Phase 7+ or v2.

### AppRafter `Tenant` CRD

A new top-level CRD wraps Kamaji + Capsule under AppRafter's CUE schema:

```cue
kind: Tenant
name: blockchain-team
controlPlane: {
    replicas: 2
    datastore: {selector: {tier: integrated}}
}
owners: [
    {from: accessGrant, subjects: ["alice@team.com", "bob@team.com"]}
]
policies: {
    quotas: {cpu: 100, memory: 200Gi}
    allowedRegistries: ["harbor.platform.local"]
    allowedRuntimeClasses: ["kata"]
}
```

Operator translates this to:
- Kamaji `TenantControlPlane` (control plane lifecycle).
- Capsule `Tenant` resource inside the TCP (policy enforcement).
- AccessGrant subjects → cluster-admin **within TCP only**, never on host cluster.

### Plane A/B structural separation (managed scenarios)

The Kamaji architecture provides this automatically:

- **Plane A (operator side):** host cluster + Kamaji controller + monitoring + Backstage. Operator has cluster-admin on host.
- **Plane B (customer data side):** tenant TCPs + their workloads. Operator has **no kubectl access** into TCPs without explicit AccessGrant from customer.

This is structural, not disciplinary. The MSP scenario (one customer's employees should not affect another customer or break the cluster) is closed automatically: a customer employee receives credentials for their TCP only, physically cannot kubectl into host or other TCPs.

## Rationale

### vCluster's syncer is an insider attack surface

vCluster operates by syncing resources between the host cluster and the vCluster's API server through a syncer process. The syncer has permissions in both clusters. A compromised syncer (or a malicious actor with access to it) becomes an insider attack against all vClusters served by that syncer.

Kamaji's tenant control plane is a separate kube-apiserver pod. There is no equivalent syncer process. Cleaner separation.

### vCluster's single-node compat is not worth multiplying mechanisms

vCluster could in principle run on T1 single-node. Kamaji structurally cannot (no separate worker nodes). The temptation was to use vCluster for T1 + Kamaji for T2+. But this doubles the mental model and operational complexity.

T1 is the "simplifications for cost" tier (per principle 1.8). Not having hard multi-tenancy there is consistent with that simplification. Solo founders are expected to migrate to T2 if they want hard multi-tenancy, with the same manifests.

### Cozystack validates the Kamaji choice

Cozystack uses Kamaji in production for hosting-provider scenarios. While AppRafter targets a different audience, the production validation of Kamaji at scale is reassuring.

### Datastore through ResourceClaim is elegant

Kamaji's datastore being managed via the same `ResourceClaim` mechanism applications use means:
- One backup story (CNPG continuous backup).
- One HA story (if CNPG is HA, Kamaji datastore is HA).
- One observability story (PG metrics applicable to Kamaji datastore).
- Same lifecycle as any other platform consumer of PG.

## Consequences

**Positive:**
- Single hard multi-tenancy mechanism (Kamaji) — one mental model.
- Plane A/B structural separation works automatically (no discipline required).
- MSP scenario closed by structure, not by policy.
- Capsule provides defense-in-depth on top of Kamaji isolation.

**Negative:**
- T1 users do not get hard multi-tenancy (acknowledged simplification).
- Kamaji adds operational complexity (separate datastore, control plane pods to manage).
- Existing plan.md Phase 3.8 (vCluster optional) and Phase 5.5 (vCluster for tenant separation) need rework.

**Trade-offs:**
- Operational simplicity (one mechanism) traded for tier completeness (T1 doesn't get hard mt).

## Risk

- Kamaji project loses maintenance support (Clastix is the primary maintainer). Mitigation: Kamaji is permissively licensed; fork is possible. Cozystack's continued use provides community pressure for maintenance.
- Postgres failure cascades to Kamaji (all tenant control planes affected). Mitigation: CNPG HA + automated failover. Acknowledge that PG cluster availability becomes tenant control plane availability.
- kine+NATS as Kamaji backend (research item) fails — staying on PG is the fallback.

## Owner

Core platform team; Phase 3.8 (Kamaji install) and Phase 3.9 (Tenant CRD operator integration).

## Re-evaluation triggers

- Kamaji project pivots or loses maintenance.
- AppRafter target audience shifts toward T1-only single-tenant users (would deprioritise hard mt).
- A simpler alternative to Kamaji emerges with same isolation guarantees and lower operational overhead.

## References

- Kamaji project: https://kamaji.clastix.io
- Capsule project: https://capsule.clastix.io
- Cozystack production usage of Kamaji.
- ADR 0014 (Cozystack — Kamaji pattern borrowed).
- ADR 0016 (Hetzner+AWS native — bootstrap problem disqualified Crossplane).
- ADR 0022 (Tier model — T1 simplifications).
- ADR 0024 (Cluster-admin constrain — Kamaji TCP separation is a layer in the bundle).
- spec.md §3.9 Tenant CRD (new section).
- spec.md §4.1 Compute Substrate (per-tier multi-tenancy column).
- Managed Strategy §9 Multi-tenancy MSP.
