# ADR 0033: Tenant security configuration — strictMode and confidential as orthogonal switches

## Status

Accepted (2026-05-27).

## Context

Two prior ADRs touched on workload data protection but left the trust model under-specified:

- **ADR 0015** declared Confidential Containers (CoCo) as an orthogonal opt-in feature, available on any tier with supporting hardware. It defined what CoCo protects against (host operator, cross-tenant attacks, physical access) but left open: *where does the encryption key come from, and who verifies the attestation report?* Without those answers, CoCo provides only physical/host-kernel protection — not protection from the platform operator (AppRafter).

- **ADR 0024** declared an 8-layer defense-in-depth bundle for cluster-admin constraint. Layer 8 (CoCo) was described as "the complete cryptographic solution" — but if AppRafter controls the attestation verifier and the key store, Layer 8 is decorative against AppRafter as adversary.

Customer discussions for Turnkey Cloud (the most operated managed plan, see ADR 0034) surfaced two distinct threat models requiring distinct responses:

1. **Tenant-internal adversary** — small and mid-size companies with strong technical leadership but limited trust in their own ops/devops teams. Want to constrain what cluster admins can see without requiring confidential hardware.

2. **Host-access adversary** — any party with root or hypervisor-level access to the nodes the workload runs on. This includes the obvious case (AppRafter staff in Turnkey, where AppRafter operates the cluster on its own Hetzner account) but also less-obvious ones: a Managed Ops customer whose devops have Hetzner credentials for the cluster's underlying infrastructure; a third party who has compromised admin credentials via phishing, token theft, or CVE-driven privilege escalation. The threat is not "rogue admin" specifically — it is "anyone holding host-level credentials, whether legitimately or not". This framing matters because credential-compromise scenarios are more common in practice than malicious-insider scenarios.

These are independent concerns. A customer may face one, both, or neither. Architecturally they decompose into two orthogonal switches on the Tenant CRD, not into a single graded "trust level". This ADR formalises that decomposition.

A consequence of framing the second concern as "host-access adversary" rather than "AppRafter as adversary" is that the right *placement* of KMS and verifier depends on the deployment mode. In Turnkey, AppRafter has host access; KMS belongs outside AppRafter. In Managed Ops with broad devops access to the underlying infrastructure, the customer's own infrastructure is the host-access surface; KMS belongs outside that. The architectural primitive is "KMS endpoint", a URL. Where to point it is a deployment-mode question, documented in the Patterns section.

## Decision

Tenant security configuration declares two orthogonal switches and a universal configuration surface. Combinations of switch values map to named deployment patterns for marketing and onboarding purposes (documented in the Patterns section below).

### Switch 1: strictMode

**Threat model:** Tenant cluster-admin / devops cannot read or modify workload data, given they cannot also modify code. Trust in AppRafter is assumed.

**Components:**
- `pods/exec`, `pods/attach`, `pods/portforward` admission deny via `ValidatingAdmissionPolicy`.
- PodSpec immutability after creation (no sidecar injection at runtime, no `extraContainers` field in Application CR).
- Capsule + Pod Security Standards (restricted) blocking privileged, hostPath, hostPID, hostNetwork pods in tenant namespaces.
- vault-injector (OpenBao agent injector) delivering secrets to process memory at runtime. Secrets do not appear in PodSpec, ConfigMaps, or Kubernetes Secrets.
- Cosign + sigstore policy-controller enforcing image signatures on Application CR `image:` field.

**Does NOT require:** CoCo runtimeClass, hardware TEE, external KMS, external attestation verifier.

**Cost profile:** near-zero marginal cost. Admission policies and vault-injector are already in the platform stack; this switch toggles enforcement. No additional compute, no per-tenant infrastructure.

**Default:** ON in Turnkey Cloud; opt-in in OSS deployments. Tenant CRD declares `defaults.strictMode.enabled: true | false` and `defaults.strictMode.allowOptOut: true | false` to control per-Application override permissions.

**Pricing:** included in base tier with high probability. Final decision deferred pending operational cost validation but the architectural assumption is "free at base".

### Switch 2: confidential

**Threat model:** Any party with host-level access to nodes (root, hypervisor, physical access) cannot read workload data. This covers AppRafter staff in Turnkey, customer's devops with Hetzner-level access in Managed Ops, and any external attacker who compromises host-level credentials. Trust anchor is hardware (AMD-SP for SEV-SNP, Intel TDX module) plus the customer-chosen attestation verifier and KMS — neither of which the host-access adversary controls.

**Components:**
- CoCo `runtimeClass: kata-cc` on confidential-capable nodepool (per ADR 0015).
- kata-agent policy (upstream CoCo Agent Policy feature, not a fork) bundled with attestation measurements. Default policy denies `ExecProcessRequest`, `WriteStreamRequest`, `ReadStreamRequest`, `UpdateInterfaceRequest`.
- Attestation Agent in workload pod, calling the configured verifier endpoint.
- Data keys released from configured KMS only after verifier confirms attestation against expected measurements.
- Reproducible builds for kata-agent, attestation agent, AppRafter operator components in the trust chain. Expected measurements published per release in Sigstore Rekor.

**Requires:** SEV-SNP or TDX-capable hardware in at least one nodepool; KMS endpoint reachable from cluster; verifier endpoint reachable from cluster.

**Does NOT mandate:** who operates the KMS or verifier. They are URLs with type discriminators. See Patterns section for typical placements.

**Cost profile:** non-trivial. Confidential-capable hardware is more expensive than commodity nodes (Hetzner currently does not offer it directly — requires bare metal on T3 or hyperscaler on T4). kata-cc runtime has measurable overhead vs runc. Each pod start incurs attestation roundtrip. Support burden is higher (attestation debugging, key release troubleshooting).

**Default:** OFF everywhere. Opt-in per Application via `security.confidential.enabled: true` and Tenant-level KMS/verifier endpoints.

**Pricing:** paid add-on with high probability. Final pricing deferred pending operational cost data, but architectural assumption is "premium add-on funded by customers paying for the threat model it covers".

### Universal configuration surface

Available at any combination of switch values:

```cue
security: {
    strictMode: {
        enabled:      bool
        allowOptOut:  bool
    }

    confidential: {
        enabled: bool
        kms: {
            type:           "aws-kms" | "openbao-transit" | "hcp-vault"
            endpoint:       string
            credentialsRef: secretRef
        }
        verifier: {
            type:     "intel-trust-authority" | "anjuna" | "coco-trustee"
            endpoint: string
        }
    }

    imageSigning: {
        publicKeys: [...]
    }

    audit: {
        sink: {
            type:           "s3"
            endpoint:       string
            bucket:         string
            credentialsRef: secretRef
            prefix:         string
        }
    }

    roles: {
        developer: { appCRWrite: [...], accessGrant: {...} }
        operator:  { appCRWrite: [...], accessGrant: {...} }
        viewer:    { appCRWrite: [...], accessGrant: {...} }
    }
}
```

**Image signing**, **audit export**, and **tenant-internal RBAC** are available regardless of which switches are on. Customer with `strictMode: false, confidential: false` can still configure image signing if they want it.

## Named deployment patterns

The following names are used in marketing, onboarding flows, and pricing pages. They are *not* CRD constructs — they describe common combinations of switch values, deployment modes, and URL placements.

The patterns reference two cluster deployment modes (reconciled with the managed plans in ADR 0034):
- **Turnkey** — AppRafter's Hetzner account, AppRafter operates host. AppRafter has host access; customer has only kubectl into their TenantControlPlane.
- **Managed Ops** — customer's Hetzner account, AppRafter operates the cluster via Kubernetes API only (no host SSH, no Hetzner token). Customer's people (CTO and/or devops, depending on their internal access carving) have host access.

| Pattern name | strictMode | confidential | Deployment mode | KMS / verifier placement | Setup workflow |
|---|---|---|---|---|---|
| **Strict Isolation** | on | off | any | n/a | AppRafter pre-configures (Turnkey/Managed Ops); OSS user toggles in Tenant CRD |
| **Turnkey — Confidential** | on | on | Turnkey | KMS and verifier anywhere except AppRafter (AWS KMS + Intel Trust Authority is the v1 default; customer-self-hosted is also valid) | AppRafter sets up URLs and federation credentials |
| **Managed Ops — Confidential** | on | on | Managed Ops with broad customer infra access | KMS and verifier anywhere outside the customer's Hetzner — AppRafter-hosted on AppRafter's own infra is the natural choice; third-party (AWS KMS in a separately-owned customer account, Intel Trust Authority) also valid | AppRafter deploys KMS/verifier on its own dogfooded infrastructure using the same tooling shipped for Sovereign pattern |
| **Sovereign** | on | on | OSS / customer-managed cluster | Customer-deployed OpenBao + customer-deployed Trustee on customer-owned infrastructure | AppRafter ships `platform-cli sovereign-stack deploy`; customer runs it on their own infra |
| **OSS — flexible** | any | any | OSS | Customer-chosen, any combination | Customer configures Tenant CRD directly; architecturally identical to any pattern above |

The cluster does not distinguish patterns at runtime — they differ only in deployment mode and in *who deployed and operates the URLs*. From the runtime's perspective, all confidential patterns are "`confidential: enabled` + here are the endpoints".

### Pattern selection guidance

The shortcut for choosing a pattern is to identify who has host-level access to the cluster's nodes, then place KMS and verifier endpoints in infrastructure that party does not control.

- **Turnkey** → AppRafter has host access. KMS goes outside AppRafter.
- **Managed Ops, narrow customer access** (CTO holds Hetzner creds, devops have only kubectl through the TCP) → effectively Turnkey-shaped: customer holds host access for compliance reasons, but the practical threat to workloads is still tenant-internal. Strict Isolation typically covers this; the customer has built a Turnkey-equivalent for themselves at the access-carving level.
- **Managed Ops, broad customer access** (devops have Hetzner-level access to the underlying infrastructure, common in small teams or organisations running AWS-refugee-style "give devops everything" patterns) → customer's own devops are the host-access threat. KMS belongs outside customer's infrastructure. AppRafter-hosted KMS is the natural choice since AppRafter has the dogfooded infrastructure and (by Managed Ops definition) does not have host access to customer nodes. Particularly relevant for credential-compromise scenarios where the worry is not malicious devops but stolen tokens or attacker-obtained admin access.
- **OSS / Sovereign** → customer controls everything. Customer decides their own host-access topology and places KMS/verifier accordingly.

The principle: **KMS and verifier endpoints belong with parties who do not hold host-level credentials to the cluster's nodes.** The architecture supports placement anywhere; deployment mode determines what "anywhere" rules out.

### Consequences of this decomposition

- **OSS users can achieve full confidential guarantees** by configuring Tenant CRD against any KMS/verifier they choose. No Turnkey-only confidential feature exists.
- **Pattern migration** is configuration, not migration. Customer changes URLs in Tenant CRD; existing Applications re-attest on next reschedule.
- **New KMS or verifier vendors** integrate as additional `type:` discriminators with corresponding protocol adapters. They do not create new patterns.
- **Managed Ops — Confidential is architecturally a Sovereign deployment** operated by AppRafter on AppRafter-owned dogfooded infrastructure, not a separate component stack. Same tooling, different operator.

## strictMode semantics — narrowing default with explicit override

Tenant declares a default + override-permission flag. Application can be more restrictive than Tenant default with no extra permission. Application can be less restrictive only if Tenant declares `allowOptOut: true` (or `allowChange: true` for confidential).

This matches classic security policy hierarchies: higher level sets the ceiling, lower level can narrow freely, loosening requires explicit upward authorisation.

## KMS availability — no novel cache in v1

Standard pattern: whoever holds the KMS runs it in HA configuration.

- AppRafter-operated OpenBao (when used as KMS) — multi-AZ Hetzner deployment.
- AWS KMS — multi-region HA by AWS.
- Customer-deployed OpenBao — `platform-cli sovereign-stack deploy` defaults to 3-node HA across customer-selected availability domains.

**Key lifecycle:**
- Pod attestation at start → KMS releases data key → key lives in TEE memory for pod lifetime.
- Running pod is unaffected by subsequent KMS unavailability.
- Pod death or reschedule requires fresh attestation + key release.
- KMS unavailability blocks new pod starts but never affects confidentiality of data already encrypted.

**The only attack via KMS path is denial of service** — refuse to schedule, simulate unavailability, force pod restarts. Confidentiality is never at risk via this vector because TEE memory protects the in-flight key and cannot be exfiltrated by anyone with host or hypervisor privileges.

**Deferred: measurement-sealed cache.** A cache of measurement-bound encrypted key blobs (via CoCo policy-bound secrets) would allow new pod starts during transient KMS unavailability without compromising the trust model. Deferred to Phase 7+ as performance/availability optimisation. Not justified at v1 scale.

## kata-agent policy — upstream, not fork

Upstream CoCo Agent Policy feature is used. No fork of kata-agent.

- AppRafter ships a default-strict OPA Rego policy denying interactive operations (`ExecProcessRequest`, `WriteStreamRequest`, `ReadStreamRequest`, `UpdateInterfaceRequest`).
- Policy file is data, not code. Its hash is included in attestation measurements via the standard CoCo agent-policy mechanism.
- Customer verifier checks policy hash against expected (published in Sigstore Rekor per release).
- Customers can author custom policies for specific lockdown requirements.

This avoids the ongoing engineering burden of maintaining a kata-agent fork.

## Image signing — Cosign + sigstore policy-controller

Cosign signatures verified by sigstore policy-controller as a `ValidatingAdmissionWebhook`.

- Public keys for tenant's trusted signers declared in Tenant CRD `security.imageSigning.publicKeys`.
- Application CR `image:` field must reference an image with a valid signature against tenant's keys.
- Customer's CI/CD pipeline signs images at build time.

This switch is independent of strictMode and confidential. A customer can enable image signing alone.

## Tenant-internal RBAC

Tenant CRD declares roles scoped to Application CR field-level write permissions:

```cue
roles: {
    developer: {
        appCRWrite: ["image", "env", "needs", "expose", "connects", "resources"]
        accessGrant: { request: true, approve: false }
    }
    operator: {
        appCRWrite: ["replicas", "rollout", "restart"]
        accessGrant: { request: true, approve: true }
    }
    viewer: {
        appCRWrite: []
        accessGrant: { request: false, approve: false }
    }
}
```

The exact field list is illustrative. ADR commits to the principle ("Tenant declares roles; roles scope Application CR field-level write permissions; subject-to-role binding lives in Tenant"), not to the specific schema. Implementation may drift on details.

Enforcement happens in a per-Tenant admission webhook checking the subject's role against attempted Application CR field mutations. Available regardless of switch values.

## Audit sink

S3-compatible bucket is the first supported sink for audit export.

- NATS JetStream → S3 bridge runs in the cluster (operated by AppRafter for Turnkey deployments, by customer for OSS).
- Bucket and credentials declared in Tenant CRD `security.audit.sink`.
- Exported events: Application CR mutations, AccessGrant operations, OpenBao secret accesses, attestation events, kata-agent policy violations, role-scoped admission denials.
- Available at any combination of switch values. Customer with neither switch on can still configure audit export.

Additional sink protocols (Loki, Splunk HEC, Datadog, custom HTTP) added by customer demand.

## Rationale

### Why two orthogonal switches and not three trust levels

The previous framing as three levels (Strict Isolation, Managed, Sovereign) conflated two independent concerns:

1. **What threats the cluster mitigates** — a property of the cluster's runtime configuration.
2. **Who operates the KMS and verifier** — an operational/contractual property orthogonal to the cluster.

When customer discussions probed the boundary between Managed and Sovereign, the actual cluster-side difference was nothing more than the value of two URL fields. The same cluster, the same runtime, the same attestation flow, only the endpoints differed. Coding this as a `mode: managed | sovereign` enum in the schema leaked an operational distinction into the CRD where it does not belong.

The two-switch decomposition matches the actual decision boundaries:

- `strictMode` is the answer to "can we make tenant cluster-admin harmless?". Pure runtime concern. Independent of who operates anything outside the cluster.
- `confidential` is the answer to "can we make host-access adversaries harmless?". Requires hardware + external trust anchors, but its cluster-side effect is determined by switch state alone.
- KMS and verifier are configuration data, not switches.

This also future-proofs against new operational patterns. When a fourth deployment style emerges (e.g., a regulated third-party operator running KMS for a consortium of customers), it's a new combination of URL configurations, not a new ADR.

### Why strictMode is on by default and (likely) free

Marginal cost is near-zero. The admission policies and vault-injector are already in the platform stack for other reasons (defense-in-depth from ADR 0024). The switch toggles enforcement; it does not add infrastructure.

The base tier already includes everything that strictMode requires, just not in enforcement mode. Charging extra for "we enforce the policies that are already running" violates the platform's pricing principle (charge for actual cost drivers). Final pricing deferred for confirmation against operational data, but architectural assumption is "free".

Security-by-default is also stronger marketing: "Turnkey is strict by default" is a clearer positioning than "Turnkey has optional strict mode for $X/mo".

### Why confidential is opt-in and (likely) paid

Cost is real. Confidential-capable hardware is not commodity (Hetzner does not currently offer it on Cloud SKUs; T3 bare metal with EPYC SEV-SNP or T4 AWS C8i are required). Each pod start incurs attestation roundtrip. Support burden is substantially higher than non-confidential workloads (attestation debugging, measurement mismatches, KMS integration issues are entirely new failure modes).

The customer segment paying for confidential is also the segment most willing to pay premium for it (regulated workloads, compliance budgets, paranoid finance/health/defense use cases). Pricing should track cost.

### Why KMS/verifier placement is a principle, not a per-pattern enumeration

Early framing of this ADR fixed placement per named pattern: Managed pattern always used third-party verifier, Sovereign always used customer-deployed. This conflated *who the host-access adversary is* with *which pattern name applies*. The reality is that deployment mode determines the host-access adversary, and KMS/verifier placement must avoid that adversary — not avoid a specific role (e.g., "AppRafter") that may or may not be the adversary in the given mode.

In Turnkey, AppRafter holds host access → KMS belongs outside AppRafter. In Managed Ops with broad customer infrastructure access, the customer's own devops hold host access → KMS belongs outside the customer's Hetzner. AppRafter-hosted KMS in Managed Ops is not "AppRafter cheating on confidential guarantees", it is *exactly correct placement* because AppRafter does not have host access in that mode and the customer's devops do not have AppRafter credentials.

The principle is **"KMS and verifier endpoints belong with parties who do not hold host-level credentials to the cluster's nodes."** Applied per deployment mode, this generates appropriate placement guidance without requiring a new pattern per scenario.

The earlier "third-party only" framing was a special case of this principle for Turnkey, not a universal rule. AppRafter-hosted KMS is wrong in Turnkey (AppRafter is the adversary), correct in Managed Ops Case B (AppRafter is not the adversary), and unavailable in OSS (AppRafter has no infra in the picture).

### Why credential-compromise is part of the threat model

Framing the second switch's threat purely as "AppRafter as adversary" undersells its value. In practice, the more common scenario justifying confidential is credential compromise: a devops engineer's Hetzner token leaked to a public GitHub repo, an admin's session cookie phished, a CVE in a tooling chain that allowed privilege escalation. These do not require malicious intent from anyone; they happen routinely.

In all these scenarios, `confidential: enabled` bounds the blast radius. The attacker with stolen host-access credentials sees encrypted memory and cannot decrypt without also compromising the KMS (separately controlled) and passing attestation (verified by an independent party). This converts credential-compromise from "all data exposed" into "DoS only" — a much more recoverable failure mode.

This matters more than the malicious-insider framing for two reasons: credential compromise is more frequent, and customers think about it more concretely (every team has had a "we leaked a token" incident; few have had a "rogue admin" incident). The threat model in this ADR should reflect that.

### Why audit export is universal and not gated by switch state

Audit export is a reporting feature, not a security boundary. The infrastructure (NATS → S3 bridge) is built once and runs once per cluster. Gating it by switch state would be artificial product slicing without cost basis. Customers with neither switch on may still want audit logs for their own compliance reporting; the bridge serves them at no incremental cost to AppRafter.

## Consequences

**Positive:**
- Schema reflects actual architectural decisions, not operational packaging.
- OSS users can achieve any deployment pattern; no Turnkey-only confidential features.
- Pattern migration is configuration, not architectural change.
- Pricing aligns with cost: free where marginal cost is zero, paid where hardware and support are real.
- New KMS/verifier vendors integrate as protocol adapters, not new ADRs.
- Marketing terminology decoupled from code, evolves without schema churn.
- Threat models are explicit and orthogonal — customers reason about them independently.
- Managed Ops customers get a strong confidential offering with no additional component work — same Sovereign tooling deployed on AppRafter's dogfooded infrastructure.
- Credential-compromise framing makes the value of `confidential` accessible to customers without a "we're paranoid about our cloud provider" framing.

**Negative:**
- Customer onboarding must explain two switches and a placement-decision question instead of a single graded slider. Higher cognitive load on first sale; lower on subsequent expansion.
- Documentation must cover relevant combinations rather than three named bundles. Mitigated by the Pattern selection guide reducing the live decision tree to "where is host access?".
- Implementation surface still substantial: admission policies, tenant RBAC, Cosign integration, Trustee adapter, AWS KMS adapter, verifier vendor adapter, S3 audit bridge, sovereign-stack tooling.

**Trade-offs:**
- Engineering complexity vs verifiable security positioning. Confidential pricing funds the engineering.
- OSS users get full feature parity with Turnkey on the security axis. Intentional — Turnkey's value is operational convenience, not security exclusivity.
- AppRafter takes on operational responsibility for KMS+verifier in the Managed Ops — Confidential pattern. This is a meaningful new obligation (uptime, key custody, audit) but aligned with what we already operate for dogfood. Pricing for that pattern must account for it.

## Risk

- **Third-party verifier vendor exits market.** Mitigation: Sovereign pattern remains as fallback; Tenant CRD reconfiguration only.
- **CoCo Agent Policy upstream stabilises incompatibly.** Mitigation: pinned upstream version per release; fork only as absolute last resort.
- **Customers don't verify the chain in confidential deployments.** Out of platform's control; the platform provides verifiable architecture, customer chooses to use it or not.
- **AWS KMS regional outage.** Mitigation: standard AWS multi-region replication, documented in customer onboarding.
- **Tenant RBAC schema drifts hard during implementation.** Acceptable — ADR commits to principle, not exact fields.
- **AppRafter staff socially engineered or coerced.** strictMode alone does not protect against this; `confidential: enabled` (with KMS not held by AppRafter) does, because it removes our technical ability to comply with such pressure.
- **AppRafter-hosted KMS for Managed Ops becomes a single point of compromise.** If AppRafter's KMS infrastructure is breached, all Managed Ops customers using this placement are affected. Mitigation: AppRafter's KMS deployment uses the same Sovereign-grade hardening (HA, reproducible-build verification, audit), and customers who treat AppRafter itself as a credible adversary should choose Sovereign pattern instead. Pricing/onboarding documentation must surface this trade-off explicitly.
- **Customers misidentify their host-access topology** and choose wrong KMS placement. Mitigation: onboarding includes an explicit "where is host access in your deployment?" question, with downstream placement guidance.

## Owner

Core platform team. Phase assignment:

- **Phase 5** — strictMode switch components and universal surface:
    - Admission policy bundle (`pods/exec` deny, PodSpec immutability, sidecar injection deny).
    - Cosign + sigstore policy-controller installation in platform-stack.
    - Tenant CRD schema for `security.imageSigning`, `security.roles`, `security.audit`.
    - Per-Tenant admission webhook for role-scoped Application CR mutations.
    - NATS → S3 audit bridge component (available at any switch state).

- **Phase 6** — confidential switch components:
    - kata-agent policy authoring and measurement-publishing pipeline.
    - Attestation Agent integration in workload pods.
    - AWS KMS adapter (`type: "aws-kms"`).
    - First verifier adapter (final vendor pick at Phase 6 start: Intel Trust Authority or Anjuna).
    - CoCo Trustee adapter (`type: "coco-trustee"`) for Sovereign pattern.

- **Phase 6 or 7** — Sovereign-pattern deployment UX:
    - `platform-cli sovereign-stack deploy` tooling for Trustee + OpenBao on customer infra.
    - Reproducible build infrastructure for trust-chain components.
    - Sigstore Rekor publishing pipeline for expected measurements.
    - **Managed Ops — Confidential** pattern uses this same tooling, deployed on AppRafter's dogfooded infrastructure rather than on customer infrastructure. No additional component work — only an operational playbook for AppRafter staff to deploy and maintain KMS/verifier on AppRafter Cloud for Managed Ops customers who opt in.

Specific sub-phase numbering recorded in PLAN_CHANGES after this ADR lands.

## Re-evaluation triggers

- A confidential-capable substrate becomes universally available on Tier 1 (current assumption: rare). Would reconsider confidential switch default state.
- AWS releases a managed CoCo attestation verifier comparable to Nitro attestation. Would extend the verifier `type:` enum and possibly recommend AWS-native chain for Managed pattern.
- A second 3rd-party KMS vendor reaches sufficient customer demand (HCP Vault, Azure Key Vault, GCP KMS). Would add to `kms.type` enum.
- Customer demand for an additional named pattern emerges (e.g., consortium-operated trust root for regulated industries). Would add to Patterns section, no schema change.
- Operational cost data on strictMode contradicts the "free" assumption. Would revisit pricing decision.
- Operational cost data on confidential overhead substantially differs from initial estimate. Would revisit pricing tier structure.
- Reproducible-builds standard (SLSA, in-toto attestations) materially shifts; published-measurements mechanism may need update.
- Performance ceiling reached on KMS roundtrip path. Trigger to revisit measurement-sealed cache deferral.

## References

- ADR 0015 — Tier 4 confidential stack — orthogonal opt-in (this ADR extends, does not supersede).
- ADR 0023 — Multi-tenancy via Kamaji (Tenant CRD foundation).
- ADR 0024 — Cluster-admin constrain bundle (Layer 8 trust model formalised here).
- ADR 0025 — PlatformStack and GitOps as control surface (admission policies ship via platform-stack chart).
- CoCo project: https://github.com/confidential-containers
- CoCo Trustee: https://github.com/confidential-containers/trustee
- CoCo Agent Policy (upstream feature in `guest-components`).
- Intel Trust Authority: https://www.intel.com/content/www/us/en/security/trust-authority.html
- Anjuna Confidential Cloud: https://www.anjuna.io
- Sigstore + Cosign: https://www.sigstore.dev
- AWS KMS: https://aws.amazon.com/kms/
- spec.md §3.9 (Tenant CRD), §4.1 (Compute Substrate per-tier), §4.4 (Workload identity), §4.10 (Audit pipeline).
- ADR 0034 — managed offering model and terminology (managed plans, Plane A/B separation, deployment-mode reconciliation).
- ADR 0037 — managed control-plane infrastructure (AppRafter-hosted KMS/verifier on dogfooded infrastructure for the Managed Operations confidential pattern).
