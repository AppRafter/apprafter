# ADR 0037: Managed control plane infrastructure — dogfooded on AppRafter, rescue-cluster recovery

## Status

Accepted (2026-05-29).

## Context

ADR 0034 cemented the managed offering model: AppRafter hosts a management/UX layer (the Backstage portal, the Account UI, the hosted MCP endpoint, and the cross-cluster aggregator) while each customer's cluster stays a fully autonomous open-source install on the customer's own infrastructure. That ADR fixed *what* the hosted layer is and *how* it connects to customer clusters (the outbound `apprafter-agent`, ADR 0031). It deliberately left open *where and how the hosted layer itself runs* — which substrate, which domain, how it is backed up, and how it is recovered if it fails.

That gap matters because the hosted layer is a control surface for paying customers. Its properties have direct consequences:

- An outage of the hosted layer does not stop customer workloads — the cluster is autonomous by design (ADR 0034) and keeps serving traffic — but it does remove the Account UI, the hosted portal, and the MCP endpoint, which is a poor experience for a customer paying for managed convenience.
- Operating the hosted layer on a conventional managed cloud would be a straightforward choice, but it would sit awkwardly beside the platform's central claim. AppRafter exists to operate Kubernetes workloads well on commodity (chiefly Hetzner) hardware; running its own commercial surface somewhere else would be a tacit statement that the platform is not good enough to run the company's own business-continuity-critical software.

Three concrete decisions had accumulated enough discussion across the launch planning (`speedrun-plan.md` §5.5) to warrant cementing here:

1. **Where the managed control plane is hosted.** The launch planning settled on dogfooding: AppRafter Cloud runs on the AppRafter Platform, on AppRafter's own Hetzner hardware. This is both an operational choice and a public trust signal — "we run AppRafter on AppRafter".

2. **What the managed domain is.** Earlier internal notes drifted between two candidate domains; the launch planning resolved this so that onboarding flows, the agent endpoint, certificate provisioning, and marketing copy all reference one name.

3. **How the hosted layer is backed up and recovered.** Dogfooding introduces a recursive dependency: a platform bug can break customer clusters *and* AppRafter's own ability to operate the fix, because the tooling that would fix it runs on the same platform. The launch planning paired this risk with an out-of-band recovery path (the rescue cluster) plus staging and canary discipline.

The trust boundary established in ADR 0034 carries into this ADR. AppRafter holds **no customer Hetzner credentials and no customer cluster credentials** (ADR 0034; the `auth` slot reserved for the managed Account in ADR 0030 governs the operator's own login, not held customer tokens). This narrows but does not eliminate the security obligation on the hosted layer: it still holds customer **metadata** — account records, cluster registrations, billing data, audit events, and the registration tokens that authenticate agents — so host security of the managed control plane remains a first-class concern.

This ADR cements the hosting, domain, backup/DR, and recovery decisions for the managed control plane itself. It does not change the connection model, the open-core split, or the terminology, all of which are fixed in ADR 0034 and reused verbatim here.

## Decision

### Hosting: dogfooded on AppRafter's own hardware-tier-2 substrate

The managed control plane — the Account UI backend, the hosted Backstage portal, the hosted MCP endpoint, and the agent bus that terminates the `apprafter-agent` connections — runs **on the AppRafter Platform, on AppRafter's own Hetzner hardware tier-2 substrate**. The hosted layer is operated as standard AppRafter `Application` workloads on a cluster that AppRafter provisions and owns through the same open-source platform shipped to customers. The public statement of this is "AppRafter Cloud runs on AppRafter Platform".

Hardware tier-2 (T2 — Team, per ADR 0022 / ADR 0034) is the substrate because it is the HA-capable open-source-core substrate at launch (`speedrun-plan.md` §0.5): a multi-node cluster rather than a single node, so the hosted layer does not have a single-VDS failure domain. The choice is the **hardware tier** axis only; it does not imply or require any particular managed plan, and the `strictMode` / `confidential` security switches (ADR 0033) remain orthogonal and are selected independently on the Tenant CRD if used.

This is dogfooding in the precise sense of ADR 0033's wording: AppRafter Cloud is an AppRafter deployment that AppRafter operates on its own infrastructure, using the same tooling shipped to customers. It is not a separate component stack.

### Domain: `apprafter.dev`

The managed control plane is served under **`apprafter.dev`**. This is the canonical managed domain: the Account UI, the hosted portal, the hosted MCP endpoint, and the agent-bus endpoint that agents dial out to are all reachable under it, and onboarding flows, certificate provisioning, and customer documentation reference it. Customer application subdomains are delegated separately (`speedrun-plan.md` §3.6) and are not governed by this decision.

### Trust boundary: no customer credentials, host security still matters

Consistent with ADR 0034, the managed control plane holds **no customer Hetzner credentials and no customer cluster credentials or kubeconfigs**, and makes **no reverse-direction API calls** into customer Kubernetes APIs. Customer clusters reach it only through the outbound `apprafter-agent` (ADR 0031), authenticated by a revocable registration token, and what the hosted layer receives is metadata only.

Because AppRafter holds no customer infrastructure credentials, the KMS/verifier placement question that ADR 0033 keys off host access does not arise for the customer's data path here — Hosted Services sits on the Sovereign side of ADR 0033's host-access reasoning, and the customer holds host access to their own nodes. However, the hosted layer **does** hold customer **metadata** (accounts, cluster registrations, billing records, audit events, registration tokens). Host security of the managed control plane is therefore a first-class obligation: a compromise of the hosted layer cannot reach into customer clusters (it holds no credentials to do so), but it could expose metadata and could abuse the agent channel within the bounds the agent permits. The hosted cluster is hardened with the same platform mechanisms available to any AppRafter cluster, and registration tokens are revocable per cluster from the Account UI (ADR 0031), bounding the blast radius of a leaked token to a single customer cluster.

### Backup and DR for the control plane itself: external-S3 backups

The managed control plane's own state is backed up using the **external-S3 backup mechanism** — the same mechanism the open-source platform ships for customer clusters (`plan.md` item 4.12). Because the hosted layer is dogfooded, its backups are not a bespoke pipeline: AppRafter Cloud is backed up the way any AppRafter cluster is, to an external S3-compatible target outside the hosted cluster's own failure domain. This keeps the recovery path for AppRafter Cloud built from the same primitives customers use and tested by the same code path.

### Out-of-band rescue cluster for platform-wide outages

A separate **rescue cluster** provides an out-of-band recovery path for a platform-wide outage of the managed control plane. The rescue cluster is deliberately **not managed through the AppRafter UI or MCP** and is not itself a dogfooded AppRafter deployment in the operational sense: it is a plain Kubernetes cluster with direct `kubectl` access and breakglass credentials, holding emergency tooling and backup-orchestration access. Its purpose is precisely to break the recursive dependency — if a platform bug or outage removes AppRafter's ability to operate AppRafter Cloud *through* AppRafter, the rescue cluster is the path that does not depend on the broken platform.

The rescue cluster is scoped to recovery of AppRafter Cloud itself. It is a distinct runbook case from the general disaster-recovery documentation the platform ships for customers; it is the operator-side breakglass for the company's own hosted layer.

### Release engineering discipline around the dogfood

To keep the dogfood from becoming a single asymmetric failure point, the managed control plane is changed through a staged pipeline:

- A **staging AppRafter** environment — itself AppRafter-managed, isolated from customer-facing production — receives platform and hosted-layer changes first, including synthetic load exercising the heavy scenarios that early real traffic will not (`speedrun-plan.md` §5.5).
- **Canary rollout** lands platform changes on a small fraction of customer clusters before a fleet-wide rollout, so a regression surfaces on a bounded population rather than all at once.
- The **rescue cluster** retains a manual override for the cases the staged pipeline does not catch.

These are operational requirements of the dogfooding decision, not optional polish: the cost of a destructive control-plane regression is asymmetric, because a customer who was sold "we run AppRafter on AppRafter" judges a control-plane failure more harshly than a customer of a conventional managed service would.

## Consequences

**Positive:**

- **Trust signal is structural, not asserted.** "AppRafter Cloud runs on AppRafter Platform" is a verifiable property a customer can see (status page, uptime), and it is the strongest validation pattern for the segment whose decision-makers know the architecture directly (small teams and EU-sovereignty customers with hands-on technical leadership). Putting the company's own business continuity on the product is a claim competitors who run their commercial surface on a hyperscaler cannot make symmetrically.
- **One operational surface.** The hosted layer is operated with the same tooling, runbooks, and backup mechanism as customer clusters. New platform capability benefits the dogfood automatically; dogfood operational experience feeds back into the platform.
- **Recovery built from shipped primitives.** External-S3 backup of AppRafter Cloud reuses the customer-facing backup path (`plan.md` 4.12), so the recovery path is exercised by the same code customers run.
- **Bounded credential surface.** Holding no customer credentials means a hosted-layer compromise cannot pivot into customer clusters; the worst case is metadata exposure and agent-channel abuse within the agent's permitted operations.

**Negative / harder:**

- **Recursive dependency is real.** A platform bug can degrade customer clusters and AppRafter Cloud at the same time, and can remove the in-band ability to fix it. This is the defining hazard of the decision; it is mitigated, not eliminated (see Risks).
- **Operational tax paid up front.** Staging AppRafter, canary rollout, and the rescue cluster are required before the managed launch, not deferred. They are additional infrastructure and additional runbooks to maintain.
- **Metadata host-security obligation.** Even without customer credentials, the hosted layer holds account, billing, registration-token, and audit metadata and must be hardened and operated accordingly; this is a continuing compliance and operations burden.

**Neutral:**

- The domain `apprafter.dev` is fixed for the managed surface; customer application subdomain delegation is a separate concern (`speedrun-plan.md` §3.6) and unaffected.
- The hosting decision is on the hardware-tier axis only; it neither selects nor implies a managed plan or a security switch.

## Alternatives considered

### Host the managed control plane on a hyperscaler / conventional managed cloud

Rejected for launch. Running AppRafter Cloud on a third-party managed cloud would sidestep the recursive-dependency hazard, but at the cost of the central trust signal and of operational coherence: the company would be operating its commercial surface on infrastructure it tells customers they do not need. The recursive-dependency hazard is real but addressable with the rescue cluster, staging, and canary discipline; the loss of the dogfooding signal and the divergence of operational surfaces are not as cheaply recovered. Retained as a fallback only if the dogfood proves operationally untenable at launch scale (a Re-evaluation trigger below).

### Single-node (hardware tier-1) substrate for the hosted layer

Rejected. The hosted layer is a control surface for paying customers; a single-VDS failure domain is an inappropriate availability posture for it. Hardware tier-2 is the HA-capable open-source-core substrate at launch and is the minimum reasonable substrate for AppRafter Cloud.

### Bespoke backup/DR pipeline for the control plane

Rejected. A hand-built backup path specific to the hosted layer would diverge from the customer-facing path and would be tested only by AppRafter, defeating part of the dogfooding value. Reusing the shipped external-S3 mechanism (`plan.md` 4.12) keeps the recovery path on the same well-exercised code.

### Make the rescue cluster a managed AppRafter deployment

Rejected, by definition. If the rescue cluster were operated through the AppRafter UI/MCP, it would inherit the very dependency it exists to break. It is intentionally a plain Kubernetes cluster with direct `kubectl` and breakglass access, outside the managed surface.

### Defer the rescue cluster / staging / canary to post-launch

Rejected. The asymmetric cost of a destructive control-plane regression — borne against the explicit dogfooding promise — means the recovery and staged-rollout machinery must exist before customers depend on the hosted layer, not after the first incident proves it was needed.

## Risks

- **Recursive dependency: a platform bug breaks customer clusters and the in-band ability to fix them simultaneously.** This is the primary risk created by dogfooding. Mitigation is layered and accepted as the cost of the decision: (1) the out-of-band **rescue cluster** with direct `kubectl` and breakglass access, not managed through AppRafter, holding emergency tooling and backup-orchestration access; (2) **staging AppRafter**, isolated from customer production, receiving changes first, with synthetic load exercising heavy scenarios ahead of real traffic; (3) **canary rollout** of platform changes to a small fraction of customer clusters before fleet-wide. The hazard is reduced to a bounded, recoverable failure mode, not removed.
- **Hosted-layer compromise exposes customer metadata.** AppRafter holds no customer credentials, so a compromise cannot pivot into customer clusters, but account/billing/registration/audit metadata and registration tokens live in the hosted layer. Mitigation: the hosted cluster is hardened with the same platform mechanisms available to any AppRafter cluster; registration tokens are per-cluster and revocable from the Account UI, bounding a leaked token to one customer cluster; the agent channel limits hosted-side reach to operations the agent permits (ADR 0031).
- **Rescue-cluster drift.** A breakglass path that is never exercised rots — credentials expire, tooling falls out of date, the runbook stops matching reality. Mitigation: the rescue cluster is part of the release-engineering requirement (it must hold a current manual override), and its recovery runbook is exercised rather than assumed. We accept that periodic drills are an ongoing operational cost.
- **Backup target shares a failure domain.** If the external-S3 backup target were operationally entangled with the hosted cluster, a single failure could take both. Mitigation: backups go to an S3-compatible target outside the hosted cluster's failure domain, consistent with `plan.md` 4.12 practice for customer clusters.
- **Single hosted region at launch.** The launch hosted layer assumes a single region (consistent with ADR 0031's single-region bus assumption). A regional outage degrades the managed surface fleet-wide, though customer clusters keep running. We accept this at launch; multi-region failover for the hosted layer is a scaling concern revisited on geographic distribution of the customer base.

## Owner

Core platform team. Andrey Ryahovskiy (`remryahirev@gmail.com`) convenes reviews and approves amendments. The managed control plane and the rescue-cluster runbook land in the managed-services track; the platform that hosts the dogfood, the external-S3 backup mechanism, and the `apprafter-agent` remain in the open-source core.

## Re-evaluation

Re-evaluate when:

- **The dogfood proves operationally untenable at launch scale** — for example, if the recursive-dependency hazard materialises in a way the rescue cluster + staging + canary do not adequately bound. Trigger to reconsider the hyperscaler-hosting fallback for the managed control plane.
- **The customer base distributes geographically** enough that a single hosted region is a material availability constraint. Trigger to plan multi-region failover for the hosted layer (and reconcile with ADR 0031's multi-region-bus open item).
- **A heavier managed plan ships** (Managed Operations, then Turnkey Cloud). Each adds operational obligations on AppRafter's side; confirm the hosting, backup, and rescue-cluster decisions still hold for the larger operated surface.
- **A rescue-cluster drill or a real incident reveals a gap** in the breakglass path, the backup mechanism, or the staged-rollout discipline. Each finding becomes a patch and a regression-guard before the next milestone, per the platform's phase-closure discipline.

## References

- `speedrun-plan.md` §5.5 (managed control plane infrastructure decisions — dogfooding host, domain, backup/DR, rescue cluster), §0.5 (Hosted Services as the launch managed plan; hardware T1/T2 in the open-source core at launch), §3.4 (hosted MCP endpoint), §3.6 (customer application subdomain delegation, distinct from the managed domain), §7.6 (onboarding journey). This ADR is self-contained; the cited sections record durable context, not a dependency on temporary strategy documents.
- `plan.md` item 4.12 (external-S3 backup mechanism reused for the control plane's own backups).
- ADR 0022 — hardware tier model (T1–T4 substrate; features orthogonal to tier).
- ADR 0030 — CLI target store and credential resolution chain (the `auth` slot reserved for the managed Account governs the operator's own login; per-target credentials never leave the operator's machine).
- ADR 0031 — `apprafter-agent` ↔ hosted-bus protocol (outbound-only, no inbound listener, revocable registration token; the agent bus terminates on the hosted control plane decided here; single-region bus assumption at launch).
- ADR 0033 — tenant security configuration (`strictMode` / `confidential` switches orthogonal to hardware tier and managed plan; host-access reasoning for KMS/verifier placement, on whose Sovereign side Hosted Services sits).
- ADR 0034 — managed offering model and terminology (hosted management layer vs customer-owned cluster; no customer credentials held; metadata-only; canonical hardware-tier and managed-plan terminology used here verbatim).
