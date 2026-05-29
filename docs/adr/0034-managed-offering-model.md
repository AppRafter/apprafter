# ADR 0034: Managed offering model and terminology — hosted-management layer, hardware tier vs managed plan

## Status

Accepted (2026-05-29).

## Context

AppRafter ships as an open-source platform that a customer can run end-to-end on their own infrastructure. On top of that platform we are building a managed offering. Two questions had accumulated enough decisions across design discussions to warrant cementing:

1. **What, architecturally, does "managed" mean for AppRafter?** A managed cloud typically operates the customer's control plane on the provider's account, which couples the customer's continuity to the provider. AppRafter's positioning is the opposite: the platform must remain fully functional and autonomous on the customer's own infrastructure, and the managed layer must be a removable convenience rather than a structural dependency.

2. **What do we call the things we have been calling "tiers"?** Two orthogonal axes have both, historically and informally, been called "tier", which has caused recurring confusion in specs, ADRs, marketing copy, and onboarding flows:
   - the **hardware substrate** a cluster runs on (single VDS through to confidential bare metal), formalised as T1–T4 in ADR 0022; and
   - the **degree to which AppRafter operates the cluster for the customer** (hosted UX only, through to AppRafter owning the hosting relationship), described as a three-step roadmap.

These two axes are independent: a customer on any hardware tier can choose any managed plan their hardware supports, and the security switches from ADR 0033 (`strictMode`, `confidential`) are independent of both.

The launch shape is fixed by the managed launch speedrun (`speedrun-plan.md` §0.5): the first and only managed plan at launch is the lightest one, in which AppRafter hosts the management/UX surface and the customer's cluster stays entirely on the customer's own infrastructure. Hardware T1 and T2 are both in the open-source core at launch; the heavier managed plans are post-launch.

This ADR cements the managed offering model and fixes the canonical terminology. It does not introduce new schema; it names and reconciles decisions already in flight.

## Decision

### Architecture: hosted management layer, customer-owned cluster

The managed offering separates a **hosted management/UX layer** from a **customer-owned cluster**:

- **AppRafter hosts** the management/UX surface: the Backstage portal, the Account UI (multi-cluster view, billing, team), the hosted MCP endpoint, and the cross-cluster aggregator. This surface runs on AppRafter's own infrastructure.

- **The customer owns and runs the cluster**: the Kubernetes control plane, the AppRafter operator and CRDs, control-plane storage, all workloads, and all data live on the customer's own infrastructure and remain fully autonomous.

The cluster is a standard open-source AppRafter install. The hosted layer is a management convenience layered on top, never a runtime dependency of the cluster. Cancelling the managed subscription means revoking the cluster's registration: the hosted services disconnect and the open-source cluster keeps running unchanged, serving traffic without interruption. Unlike a hyperscaler managed-Kubernetes offering, where the control plane is operated by the provider, the AppRafter customer cluster stays autonomous and survives the end of the relationship without migration.

### Connection model: outbound agent, no credentials held

The customer cluster connects to the hosted layer through an **outbound `apprafter-agent` connection** (per ADR 0031). The agent runs inside the customer cluster and dials out to the hosted bus; there is **no inbound listener** on the customer side and the customer configures no inbound firewall rules.

AppRafter holds **no customer cluster credentials and no kubeconfigs**. The hosted layer never makes reverse-direction API calls into the customer's Kubernetes API. The customer registers their cluster with a revocable registration token issued through the CLI; the agent presents that token, and all hosted operations are reachable only through the agent channel and the AppRafter CRD abstraction.

What the hosted layer receives is **metadata only** — manifest-apply events, status events, audit events, and opt-in log streams — never the customer's application data plane. The minimal-data-exposure principle that this implies is forwarded to its own dedicated ADR (ADR 0035), which this ADR references but does not define.

### Terminology: "hardware tier" vs "managed plan"

To end the "tier" collision, the two axes get distinct, explicit names. **Neither axis is to be called simply "tier" in new specs, ADRs, schema, UI copy, or onboarding flows.**

- **Hardware tier** — the compute substrate, per ADR 0022. Canonical names: **T1 (Solo)**, **T2 (Team)**, **T3 (Pro)**, **T4 (Regulated)**. The hardware tier describes the substrate only; features such as HA, hard multi-tenancy, and confidential compute are orthogonal layers, not tier-defining (ADR 0022).

- **Managed plan** — how much AppRafter operates for the customer. Canonical names: **Hosted Services**, **Managed Operations**, **Turnkey Cloud**, with an **Enterprise** plan to be defined later (TBD). The managed plan describes the operational relationship only; it is independent of the hardware tier.

A cluster has both a hardware tier and (if managed) a managed plan; the two are chosen independently within hardware constraints.

### Managed plans — responsibility split

The three managed plans differ only in how much of the operational relationship AppRafter takes on. The customer always owns the cluster and its data; the table records where each responsibility sits per plan.

| Responsibility | Hosted Services | Managed Operations | Turnkey Cloud |
|---|---|---|---|
| Kubernetes control plane | customer | customer | customer |
| AppRafter operator + CRDs | customer | customer | customer |
| Workloads + all data | customer | customer | customer |
| Storage + backups (data path) | customer | customer | customer |
| Hosted Backstage portal | AppRafter | AppRafter | AppRafter |
| Account UI (multi-cluster, billing, team) | AppRafter | AppRafter | AppRafter |
| Hosted MCP endpoint | AppRafter | AppRafter | AppRafter |
| Cross-cluster aggregator | AppRafter | AppRafter | AppRafter |
| Abuse handling | customer | AppRafter (automated) | AppRafter |
| Cost monitoring + anomaly alerts | customer | AppRafter | AppRafter |
| Backup orchestration (timing/destination) | customer | AppRafter | AppRafter |
| Infrastructure-provider account + billing | customer | customer | AppRafter |
| Hardware provisioning | customer | customer | AppRafter |

Under all three plans, the customer's cluster, applications, manifests, and data stay on infrastructure the customer can always retain or migrate. The escalating difference is purely operational scope: Hosted Services adds a hosted UX layer; Managed Operations adds automation of provider-account chores while the account stays the customer's; Turnkey Cloud additionally owns the infrastructure-provider relationship. This ADR fixes the structure of the split, not prices.

### Launch scope

**Hosted Services is the launch managed plan.** Hardware tiers T1 and T2 are both in the open-source core at launch (`speedrun-plan.md` §0.5), so a customer can register a single-node (T1) or HA (T2) cluster and attach Hosted Services to it. **Managed Operations** and **Turnkey Cloud** are post-launch and activate on validated customer demand. The **Enterprise** plan is reserved (TBD) and is not part of the launch surface.

### Reconciliation with ADR 0033 deployment modes

ADR 0033 references two cluster deployment modes, **Turnkey** and **Managed Ops**, alongside a **Sovereign** (open-source / customer-managed) pattern, used there to reason about where the KMS and attestation verifier belong. Those modes map onto the managed plans of this ADR as follows:

| ADR 0033 deployment mode | Managed plan (this ADR) | Who has host-level access to the nodes |
|---|---|---|
| Sovereign | open-source self-host, or **Hosted Services** | the customer (AppRafter has none) |
| Managed Ops | **Managed Operations** | the customer's operators |
| Turnkey | **Turnkey Cloud** | AppRafter |

Hosted Services sits on the Sovereign side of ADR 0033's host-access reasoning: the customer's cluster runs on the customer's own infrastructure and AppRafter has no host-level access to the nodes (and, per the connection model above, no cluster credentials at all). Managed Operations corresponds to ADR 0033's Managed Ops mode; Turnkey Cloud corresponds to its Turnkey mode.

The two security switches defined in ADR 0033 — `strictMode` and `confidential` — are **orthogonal to both the hardware tier and the managed plan**. A customer selects them on the Tenant CRD independently; they do not define, and are not implied by, any tier or plan. The host-access reasoning in ADR 0033 (where to place KMS/verifier) keys off the deployment mode, which is what the managed plan determines, but the switches themselves remain a separate axis.

### Open-core split principle

**The platform is fully functional as open source; the managed plans add premium quality-of-life and operations, never a structural dependency — a customer can always leave with the entire cluster intact.** Everything a cluster needs in order to *run* is in the open-source core; the managed plans add conveniences that *improve* operation without *blocking* it, plus capabilities that only exist in a cross-cluster or hosted context. The anti-vendor-lock guarantee is structural, not a promise: because the cluster is a standard open-source install, ending the managed relationship is a registration revocation, not a migration.

## Consequences

- **Easier to position and reason about.** "Hardware tier" and "managed plan" are unambiguous; specs, schema, UI, and onboarding can use them without re-explaining which axis is meant. ADR 0033's deployment modes now have an explicit mapping to the customer-facing plan names.
- **Strong, structural anti-lock story.** Cancellation cannot strand a customer: the open-source cluster keeps running. This is a property of the architecture, available from launch, and it holds across all three managed plans (with Turnkey Cloud additionally requiring an infrastructure move, since AppRafter owns the account there).
- **Minimal data and credential surface.** Holding no customer credentials and receiving only metadata reduces AppRafter's compliance scope and removes a class of credential-theft blast radius. ADR 0035 formalises this.
- **Clear launch boundary.** Hosted Services on hardware T1/T2 is a complete launchable product; the heavier plans are explicitly deferred with demand triggers rather than half-built.
- **Higher first-contact onboarding load.** A customer must understand two axes and complete a multi-step onboarding (provision their own cluster, then register it). This is the cost of keeping the cluster customer-owned; it is mitigated by CLI orchestration and an Account UI walkthrough (`speedrun-plan.md` §7.6).
- **AppRafter operational scope grows per plan.** Managed Operations and Turnkey Cloud take on real operational obligations (automation, and for Turnkey the provider relationship). They are deferred precisely so that operational experience is earned on the lower-risk plan first.

## Alternatives considered

### AppRafter holds customer cluster credentials / makes inbound API calls into the customer cluster

Rejected. An architecture in which the hosted layer stores customer kubeconfigs or provider tokens and reaches inbound into the customer's Kubernetes API would:

- **Break the trivial-exit guarantee.** If management depended on credentials AppRafter holds, the customer's continuity would be coupled to AppRafter, which is exactly the coupling the offering is designed to avoid.
- **Undermine data sovereignty.** Inbound access and held credentials enlarge the data and trust surface and contradict the metadata-only, minimal-exposure stance forwarded to ADR 0035 — directly relevant to the EU-sovereignty segment that requires the customer's infrastructure to stay under the customer's sole control.
- **Concentrate blast radius.** A compromise of held credentials would expose customer clusters directly. The outbound-agent model bounds a hosted-side compromise to operations expressible through the agent and the AppRafter CRD abstraction.

The outbound `apprafter-agent` model (ADR 0031) was chosen instead: it requires no inbound firewall rules, holds no credentials, and disconnects cleanly on token revocation.

### Operating the customer's control plane on AppRafter's account as the default managed model

Rejected as the default. Operating the control plane provider-side is the hyperscaler managed-Kubernetes shape; it couples the customer's cluster lifecycle to the provider and cannot offer cancellation-without-migration. This shape is retained only as the most operated plan (Turnkey Cloud), where AppRafter explicitly owns the infrastructure relationship and the customer has accepted that trade in exchange for not managing an infrastructure account; even there the manifests and data export remain in the customer's hands.

### Keeping a single "tier" word for both axes

Rejected. The collision was the source of repeated confusion across documents and conversations. Two explicit names cost a one-time rename and a glossary entry; the ambiguity cost recurs on every new spec and sales conversation.

## Risks

- **Onboarding friction on the customer-owned model.** Because the customer provisions and owns the cluster, first-run onboarding is longer than a fully hosted competitor's. Mitigation: CLI orchestration (`bootstrap-all`, `doctor`, miette diagnostics) and an Account UI walkthrough; an optional `curl | sh` orchestrator is held in reserve if beta feedback shows friction is blocking (`speedrun-plan.md` §7.6).
- **Agent supply-chain compromise.** The agent runs in the customer cluster with an outbound trust relationship to the hosted bus. Mitigation: agent binaries are signed (cosign), agent permissions are narrow (operations through AppRafter CRDs, not cluster-admin), the customer can revoke the token at any time, and agent operations are visible to the customer's own audit view (ADR 0031).
- **Terminology drift.** New material may slip back to "tier" for the operational axis. Mitigation: this ADR is the canonical reference; reviews flag the bare word "tier" where the managed plan is meant. Accept that legacy documents will be migrated opportunistically rather than all at once.
- **Plan/mode mapping confusion.** Three managed plans here and three deployment modes in ADR 0033 are similar but not identical concepts (operational relationship vs host-access reasoning). Mitigation: the reconciliation table is the single mapping; security switches are repeatedly stated as orthogonal to both axes.

## Owner

Core platform team. Andrey Ryahovskiy (`remryahirev@gmail.com`) convenes reviews and approves amendments. The managed plans land in the managed-services track; the agent and open-source cluster remain in the open-source core.

## Re-evaluation

Re-evaluate when:

- The second managed plan (Managed Operations) is scheduled to ship — confirm the responsibility split and the ADR 0033 mode mapping against implementation reality.
- Turnkey Cloud planning opens — the infrastructure-provider relationship introduces obligations (provider-account abuse handling, VAT, DPA chain) that this ADR records only at the structural level.
- The Enterprise plan is defined — fill in the TBD row and reconcile with any contract-driven deviations from the open-core split.
- A measurable signal contradicts the open-core split — for example, demand for a capability that the principle would place in the open-source core but that proves to require hosted-only infrastructure to deliver.

## References

- `speedrun-plan.md` §0.5 (Hosted Services as the launch managed plan; T1/T2 in the open-source core at launch), §3.1–3.2 (hosted scaffolding and `apprafter-agent` registration), §3.2a (offboarding = revoke registration), §7.6 (onboarding journey and mitigations).
- ADR 0022 — hardware tier model (T1/T2/T3/T4 substrate; features orthogonal to tier).
- ADR 0023 — Kamaji multi-tenancy and Plane A/B separation (hard multi-tenancy is an orthogonal, opt-in layer, not a plan).
- ADR 0030 — CLI target store and credential resolution chain (`auth` stub reserved for the managed Account; per-target credentials never leave the operator's machine).
- ADR 0031 — `apprafter-agent` ↔ hosted-bus protocol (outbound, no inbound listener, registration-token authentication).
- ADR 0033 — tenant security configuration (`strictMode` / `confidential` switches; Turnkey / Managed Ops / Sovereign deployment modes reconciled here as orthogonal to the managed plan).
- ADR 0035 — minimal data exposure (metadata-only constraint forwarded from this ADR).
- This ADR is self-contained; durable strategic context is recorded in `speedrun-plan.md` (cited above).
