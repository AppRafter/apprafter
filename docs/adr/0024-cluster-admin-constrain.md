# ADR 0024: Cluster-admin constrain bundle — defense in depth

## Status

Accepted (2026-05-12).

## Context

One of AppRafter's core security values is minimising cluster-admin intervention in application workloads. The default Kubernetes RBAC model grants cluster-admin god-mode powers: read any secret, exec into any pod, modify any resource. This is fundamentally incompatible with positioning AppRafter as a security-first platform.

A complete cryptographic solution exists (Confidential Containers / CoCo, Phase 6), but it requires specific hardware and is limited to confidential workloads. Defense in depth requires layered mechanisms that reduce cluster-admin power across all workloads, not just confidential ones.

This ADR formalises the bundle of mechanisms that together constrain cluster-admin power, with each layer addressing a different aspect.

## Decision

A defense-in-depth bundle of mechanisms is adopted. No single tool solves the problem; together they significantly reduce cluster-admin's blast radius while maintaining operational viability.

| # | Layer | What it constrains | Phase | Status |
|---|---|---|---|---|
| 1 | Workload identity via SPIFFE | Auth between workloads via X.509 SVID, not cluster RBAC; cluster-admin cannot trivially spoof workload identity | 2.7 | Already in spec |
| 2 | Secrets via OpenBao + workload identity | Secret access via Vault Agent / CSI with SPIFFE check; not `kubectl get secret` | 3.11 | Already in spec |
| 3 | Kamaji TenantControlPlane separation | Tenant-admin ≠ host-admin; host cluster-admin has no automatic kubectl access into tenant TCPs | 3.8 (see ADR 0023) | New |
| 4 | Two-person rule via AccessGrant `approvers` | Host cluster-admin AccessGrants require explicit approval from a second admin | 4.5 | New |
| 5 | JIT cluster-admin access via short-TTL AccessGrant | Emergency host cluster-admin grants TTL'd at 1h; auto-revoke | 4.5b (new) | New |
| 6 | Audit pipeline as code | All cluster-admin actions tagged and routed to immutable JetStream stream | 4.10 | Already in spec |
| 7 | OpenBao audit log | All secrets access logged separately | 3.11 | Already in spec |
| 8 | Confidential Containers (CoCo) | For confidential workloads only — hardware-level memory encryption blocks cluster-admin read | 6 | Already in spec |

## Per-layer detail

### Layer 1 — Workload identity (SPIFFE)

Workloads authenticate to each other and to platform services (OpenBao, ServiceProviders) via X.509 SVIDs issued by SPIRE. Cluster-admin cannot trivially generate or spoof workload identity without compromising SPIRE itself.

This means cluster-admin cannot, for example, run a side pod and have it impersonate `parser-prod` to read `parser-prod`'s database. The SPIRE registration is keyed to pod attestation properties (labels, namespace, service account) that are tamper-evident.

Spec.md §4.4 covers SPIFFE workload identity.

### Layer 2 — Secrets via OpenBao + workload identity

Application secrets are not stored as Kubernetes Secrets that cluster-admin can `kubectl get`. They are stored in OpenBao, accessed at runtime via Vault Agent or Secrets Store CSI Driver, with workload identity (from Layer 1) as the authentication mechanism.

Cluster-admin can still see Vault Agent pods, can `kubectl exec` into them, but extracting credentials requires impersonating the pod's SPIFFE identity, which is constrained by Layer 1.

Spec.md §4.4 covers OpenBao integration.

### Layer 3 — Kamaji TCP separation

Host cluster-admin has no automatic kubectl access into tenant TenantControlPlanes (TCPs). Each tenant has its own kube-apiserver pod with its own credentials. Host cluster-admin must explicitly issue themselves an AccessGrant into a TCP to interact with workloads inside.

This is structural separation, not disciplinary. See ADR 0023.

### Layer 4 — Two-person rule via AccessGrant `approvers`

AccessGrants for host cluster-admin scope require explicit `approvers` field, listing one or more other admins who must approve before the grant becomes active:

```cue
kind: AccessGrant
subject: alice@company.com
scope: {
    cluster: host
    capabilities: ["cluster-admin"]
}
approvers: ["bob@company.com"]
expiry: "30d"
```

AccessGrant reconciler holds the grant in `pending-approval` status until all approvers sign. Approvals are audit-logged.

### Layer 5 — JIT cluster-admin via short-TTL AccessGrant

Emergency cluster-admin grants for host operations use a 1-hour TTL by convention. After expiry, the grant is auto-revoked, kubeconfig becomes invalid, audit is closed.

```cue
kind: AccessGrant
subject: alice@company.com
scope: {
    cluster: host
    capabilities: ["cluster-admin"]
}
approvers: ["bob@company.com"]
expiry: "1h"
purpose: "Emergency: investigate stuck node migration"
```

Backstage shows a prominent "Emergency JIT access active" banner visible to the entire team while the grant is active.

### Layer 6 — Audit pipeline as code

All Kubernetes API server actions are audit-logged. Cluster-admin actions are tagged separately, routed to an immutable JetStream stream `audit.cluster-admin`, retained per compliance policy.

This does not prevent malicious actions; it ensures forensic capability after the fact.

Spec.md §4.10 covers the audit pipeline.

### Layer 7 — OpenBao audit log

OpenBao logs every secret access with the authenticated identity (SPIFFE SVID claims). This complements Layer 6 for secret-access traceability.

### Layer 8 — CoCo for confidential workloads

For applications with `confidential: true`, workload memory is encrypted at the hardware level (SEV-SNP / TDX). Cluster-admin cannot read memory even with full host access. This is the complete cryptographic solution, but it is workload-opt-in and hardware-dependent.

See ADR 0015 for the confidential stack.

## Rationale

### Single mechanism is insufficient

Each layer alone has gaps:
- Layer 1 (SPIFFE) requires SPIRE integrity.
- Layer 2 (OpenBao) requires SPIFFE working.
- Layer 3 (Kamaji) doesn't help if cluster-admin issues themselves a TCP grant.
- Layer 4 (two-person rule) is procedural and can be socially circumvented in small teams.
- Layer 5 (JIT) reduces but does not eliminate windows.
- Layer 6/7 (audit) is forensic, not preventive.
- Layer 8 (CoCo) is hardware-bound and workload-opt-in.

Together, they significantly reduce the realistic attack surface without requiring CoCo for all workloads.

### Procedural layers (4, 5) are weak without structural layers (1, 2, 3)

The two-person rule and JIT TTL only matter if cluster-admin doesn't already have unlimited access. Layers 1-3 provide the structural baseline that makes procedural controls meaningful.

### Forensic capability matters even when prevention fails

Audit logs (Layers 6, 7) don't prevent abuse but enable detection and response. For regulatory compliance scenarios (Tier 4 audience), forensic capability is often a hard requirement independent of preventive controls.

## Consequences

**Positive:**
- Cluster-admin power is meaningfully reduced across all workloads, not just confidential.
- Each layer is independently valuable; loss of one doesn't compromise all.
- Aligns with security-first positioning of the platform.
- Forensic capability supports compliance requirements.

**Negative:**
- Operational complexity increases — admins must work within the constraint bundle.
- Two-person rule and JIT TTL add friction to legitimate emergency operations.
- Implementation spans multiple phases (2.7, 3.8, 3.11, 4.5, 4.5b, 4.10, 6) — completion is incremental.

**Trade-offs:**
- Operational friction traded for security posture.

## Risk

- Admin friction in emergencies leads to disabling layers (e.g. disabling two-person rule for "convenience"). Mitigation: well-designed emergency JIT flow (Layer 5) provides legitimate escape valve that's still audited.
- Bundle complexity confuses users — they don't understand which layer protects what. Mitigation: documentation explicitly maps threats to layers (this ADR is the start).
- CoCo (Layer 8) hardware requirements limit Layer 8 reach. Mitigation: Layers 1-7 apply universally; Layer 8 is the additional protection where hardware allows.

## Owner

Core platform team; Layer 1-2 already in spec, Layers 3, 4, 5 to be added in Phases 3.8, 4.5, 4.5b.

## Re-evaluation triggers

- A new technology supersedes one of the layers (e.g. a hardware mechanism replaces SPIFFE for workload identity).
- Audit/regulatory framework requires controls not covered by the bundle.
- Customer feedback indicates a specific layer is causing unacceptable operational pain — would trigger redesign of that layer (not removal).

## References

- SPIFFE/SPIRE project.
- OpenBao project.
- Confidential Containers project.
- ADR 0015 (Tier 4 confidential — Layer 8 detail).
- ADR 0023 (Kamaji multi-tenancy — Layer 3 detail).
- spec.md §3.4 AccessGrant (Layer 4, 5 detail).
- spec.md §4.4 Secrets (Layer 1, 2 detail).
- spec.md §4.10 Observability (Layer 6 detail).
- spec.md §4.13 Cluster-admin constrain (new section, this ADR's primary spec landing).
