# ADR 0015: Tier 4 confidential stack — orthogonal opt-in

## Status

Accepted (2026-05-12). Refines the implicit Tier 4 definition that was present in spec.md §4.1 (rev.5).

## Context

Spec.md §4.1 (rev.5) described Tier 4 as "Tier 3 + nodes with SEV-SNP or TDX. Kata-CC as runtimeClass". This conflated two distinct concerns:

1. **Tier definition** — what substrate the platform runs on (single VDS, multi-node, bare metal, hyperscaler).
2. **Confidential containers feature** — whether workloads run with hardware-level memory encryption.

After tier model clarification (ADR 0022), Tier 4 was redefined as "external hyperscalers (AWS/GCP/Azure) for regulatory compliance", decoupled from confidential compute. This ADR formalises the confidential containers feature as an orthogonal opt-in.

## Decision

Confidential containers are an opt-in Application-level feature, available on any tier where the hardware supports the required instruction set extensions. They do not define a tier and are not tier-restricted.

Hardware availability per tier:
- **T1 (single VDS):** opt-in **if** the VDS host CPU supports SEV-SNP or TDX and the hypervisor passes through the necessary primitives. Realistically rare for entry-level VDS.
- **T2 (3+ nodes):** opt-in **if** any node in the cluster supports the required hardware. Usually unavailable on small CCX/dedicated hardware.
- **T3 (bare metal):** opt-in **if** SEV-SNP-capable AMD EPYC or TDX-capable Intel Xeon Scalable 4th-gen+ is provisioned.
- **T4 (hyperscalers):** opt-in **if** instances with TDX/SEV-SNP are selected (AWS C8i/M8i/R8i, Azure DCadsv5/DCedsv5, GCP Tau VMs).

## Rationale

### Tier should describe substrate

Tier model is a function of compute substrate (VDS / multi-node / bare metal / hyperscaler). Confidential is a function of node hardware. Conflating them creates artificial constraints (e.g. "I want regulated compute on bare metal" should be possible without being labelled Tier 4).

### Manifest portability

`confidential: true` in an Application manifest should resolve to the appropriate runtime class on whatever tier supports it, not require tier coordination. Operator behaviour:
1. Check `Infrastructure` for nodepool with confidential-capable hardware (labelled `compute.confidential: tdx` or `sev-snp`).
2. If found → schedule pod on that nodepool with `runtimeClass: kata-cc`.
3. If not found → admission webhook rejects with "no confidential-capable nodes available".

### Threat model — explicit

Confidential containers (CoCo project + Kata-CC runtime + SEV-SNP/TDX hardware) protect against:
- Cloud provider administrator with root on the host.
- Other tenants on the same physical machine (cross-tenant attacks via shared CPU/memory).
- Physical access to the host (cold boot, DIMM extraction).

They do **not** protect against:
- The application's own cluster administrator (who can still kubectl exec into pods, modify Application manifests, etc.). For this, ADR 0024 cluster-admin constrain bundle applies.
- Side-channel attacks on the CPU itself (Spectre/Meltdown class). Hardware vendor mitigations apply.

## Stack components

1. **Talos Linux** on confidential-capable hardware.
2. **Kata Containers** with the **Confidential Containers (CoCo)** project for the runtime layer.
3. **Attestation flow** via SPIFFE/SPIRE workload identity + remote attestation service.
4. **Encrypted memory** at hardware level (SEV-SNP on AMD; TDX on Intel).
5. **TEE per pod** — each pod runs in its own Trusted Execution Environment.

## Application opt-in

```cue
confidential: {
    required: true
    attestation: required
    keyRelease: "via attestation-service"
}
```

When `confidential: true`:
1. Operator verifies presence of confidential-capable nodepool in cluster.
2. Operator sets pod's `runtimeClass: kata-cc` and adds nodepool affinity.
3. Operator emits attestation event to audit log.
4. Workload identity (SPIFFE) is extended with attestation report claim.
5. Secrets access (via OpenBao) requires attestation-verified workload identity.

## Consequences

**Positive:**
- Tier model stays clean (substrate only).
- Confidential opt-in works wherever hardware supports, not just T4.
- Application manifest remains portable across tiers (only resolves differently based on available hardware).

**Negative:**
- Hardware availability becomes a deployment-time validation concern (operator must check before scheduling).
- T1/T2 users may misunderstand and request confidential without supported hardware; clear error messages required.

**Trade-offs:**
- Decoupling adds operator complexity (per-pod scheduling logic with hardware check) in exchange for cleaner tier model and feature portability.

## Risk

- Hardware availability mismatch — Application declares `confidential: true`, no hardware present, scheduling fails. Mitigation: pre-flight validation via admission webhook returns clear error.
- Attestation flow bugs — incorrect attestation could allow non-confidential workload to access confidential secrets. Mitigation: integration testing on every release.

## Owner

Core platform team; CoCo integration in Phase 6.

## Re-evaluation triggers

- If TDX/SEV-SNP becomes ubiquitous on consumer VPS hardware (Tier 1 viable for confidential), the per-tier hardware availability table should be updated.
- If Kata-CC project pivots away from CoCo, runtime choice may need reconsideration.

## References

- CoCo project: https://github.com/confidential-containers
- ADR 0022 (Tier model clarification — decoupling rationale).
- ADR 0024 (Cluster-admin constrain — complementary protection for non-confidential workloads).
- spec.md §4.1 Tier 4.
- spec.md §3.1 Application.confidential.
