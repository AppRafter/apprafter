# ADR 0038: Tier 2 default is an HA substrate only; hard multi-tenancy via Kamaji is opt-in

## Status

Accepted (2026-05-29).

## Context

ADR 0023 established Kamaji as AppRafter's single hard multi-tenancy mechanism and set its per-tier availability: structurally impossible on hardware T1 (single node), and **default on, opt-out** from hardware T2 upward, with each AppRafter `Tenant` mapping to a Kamaji `TenantControlPlane`. That default-on framing was reasonable when T2 was reasoned about as the entry point for hosting-provider and multi-organisation scenarios, where hard isolation by structure is the point.

The managed launch speedrun (`speedrun-plan.md` §5.7) pulls the hardware T2 substrate forward into the open-source core so that a customer can register either a single-node (T1) or an HA (T2) cluster at launch. In doing so it re-examined what hardware T2 needs to be *at launch* and found a mismatch with the ADR 0023 default:

- The launch hardware T2 customer segment is dominated by single-organisation teams — solo developers growing into a small team, and small teams running one organisation's workloads — not managed service providers or multi-organisation operators. For a single organisation, hard multi-tenancy is not required: standard Kubernetes namespaces, the default-deny `NetworkPolicy`, workload identity, and the Capsule policy layer already separate that organisation's environments and teams adequately.

- Kamaji introduces distributed-systems operational surface — a per-tenant `kube-apiserver` pod, a shared datastore whose availability becomes tenant-control-plane availability, and the corresponding lifecycle and backup concerns (ADR 0023 records these as acknowledged negatives and risks). Carrying that surface as the default on every T2 cluster spends complexity and delivery time on isolation that the launch segment does not need.

- The hardware tiers describe the compute substrate only; HA, hard multi-tenancy, and confidential compute are orthogonal layers, not tier-defining (ADR 0022, reaffirmed as canonical terminology in ADR 0034). "T2 is an HA substrate" and "T2 enables hard multi-tenancy by default" are independent claims, and the speedrun separates them.

The speedrun's T2 substrate is therefore an HA cluster only: a 3-node k3s control plane with embedded etcd and kube-vip for the virtual IP, the standard k3s HA pattern, with etcd handling HA storage. (This is also why the speedrun keeps etcd rather than kine+NATS for launch-scale HA, see ADR 0005 — orthogonal to this decision.) Kamaji and the `Tenant` CRD are deferred to the prioritised post-launch backlog (`speedrun-plan.md` §2.3, §6) and become an opt-in capability rather than a substrate default.

`spec.md` §4.1 currently states the hardware T2 row as "hard multi-tenancy via Kamaji, default". This ADR is the recorded deviation from that statement and the trigger for the spec update it requires.

## Decision

**On hardware T2 the platform default is an HA substrate only; hard multi-tenancy via Kamaji is opt-in, default off.**

- The default hardware T2 cluster is a 3-node k3s control plane with embedded etcd and kube-vip. It provides HA but **not** hard multi-tenancy. A single organisation separates its environments and teams with standard Kubernetes namespaces, the default-deny `NetworkPolicy`, workload identity, and the Capsule policy layer.

- Hard multi-tenancy via Kamaji is enabled explicitly through the PlatformStack control surface: `PlatformStack.spec.values.multitenancy: true`. When this flag is unset or `false` (the default), the cluster runs no Kamaji controller and no per-tenant `TenantControlPlane`; the `Tenant` CRD path is not engaged.

- When `multitenancy: true` is set, **Kamaji remains the single hard multi-tenancy mechanism**, exactly as ADR 0023 decided. This ADR changes only *whether Kamaji is on by default on hardware T2*; it does not introduce any alternative mechanism, reopen the vCluster / HNC / bare-RBAC comparison, or alter the `Tenant` → Kamaji `TenantControlPlane` + Capsule translation. vCluster, HNC, and bare RBAC remain not adopted.

- **This amends ADR 0023.** ADR 0023's per-tier table set hardware T2+ to "Kamaji default, opt-out"; for hardware T2 that entry is now "Kamaji opt-in, default off". Hard multi-tenancy becomes a substrate default only at higher hardware tiers (T3 and T4), where compliance and scale requirements make structural isolation the expected posture. The other rows of ADR 0023's table (T1 structurally unavailable; T3/T4 Kamaji default, with CoCo selective/default per tier) are unchanged.

### Relationship to the orthogonal axes (ADR 0034)

This decision concerns the **hardware tier** axis only. It does not touch the **managed plan** axis (Hosted Services, Managed Operations, Turnkey Cloud, with Enterprise reserved as TBD): a hardware T2 cluster attached to Hosted Services is the launch shape, and its multi-tenancy posture is governed by this flag regardless of plan. It is likewise independent of the ADR 0033 security switches `strictMode` and `confidential`, which are selected on the `Tenant` CRD and remain orthogonal to both the hardware tier and the managed plan; turning hard multi-tenancy off by default on T2 neither implies nor precludes either switch.

The open-core split (ADR 0034) holds: the default HA-only T2 cluster is a complete open-source install, and enabling `multitenancy: true` adds a capability rather than a structural dependency. A cluster can move between the two postures through configuration; cancelling a managed subscription is still a registration revocation, not a migration.

## Consequences

- **Lower launch complexity and faster delivery on hardware T2.** The default T2 cluster ships without the Kamaji controller, the per-tenant control-plane pods, or the shared-datastore lifecycle. The distributed-systems operational surface ADR 0023 records as a negative is not carried on clusters that do not need it.
- **The hardware T2 substrate is honestly "HA, not hard-isolated" by default.** This matches the launch segment (single-organisation teams) and keeps the "T2 = HA substrate" claim independent of multi-tenancy, consistent with ADR 0022 / ADR 0034 terminology. Marketing and onboarding must describe hard multi-tenancy as an opt-in capability, not a property of T2.
- **Single-organisation isolation is unchanged.** Namespaces, default-deny `NetworkPolicy`, workload identity, and Capsule continue to separate environments and teams within one organisation. What is *not* available by default is structural isolation between mutually-untrusting organisations.
- **One toggle, one mechanism.** When hard multi-tenancy is wanted, the path is a single PlatformStack flag and the same Kamaji mechanism ADR 0023 defined — no second mechanism, no new mental model. Operators reason about exactly one hard-MT implementation whether it is on or off.
- **Spec update required.** `spec.md` §4.1 must change the hardware T2 multi-tenancy entry from "Kamaji default" to "HA substrate only; Kamaji opt-in (default off)". The mirror copies of the per-tier framing in §1.8 (solo-tier adoption / migration pathway), §3.9 (`Tenant` CRD availability), the §5 technology-stack table, and Appendix C must be updated to match. Until those edits land, the spec and this ADR disagree; this ADR is the governing decision.
- **A class of customer must opt in deliberately.** A multi-organisation or service-provider customer on hardware T2 must set `multitenancy: true` rather than receiving hard isolation automatically. This is a deliberate cost paid by the segment that needs it, in exchange for not imposing Kamaji on the segment that does not. The re-evaluation trigger below guards against this segment growing large enough to flip the default.

## Alternatives considered

### Keep Kamaji default-on for hardware T2 (ADR 0023 as written)

Rejected for launch. Default-on Kamaji optimises for the multi-organisation / service-provider scenario, which is a minority of the launch hardware T2 segment. It would impose a distributed-systems operational surface and additional delivery time on the majority single-organisation segment that gains nothing from hard isolation, and it would slow the launch the speedrun is organised around. The scenario it serves is real but post-launch (see the re-evaluation trigger), and is fully reachable through the opt-in flag.

### Adopt a lighter soft-multi-tenancy mechanism as the T2 default instead of Kamaji opt-in

Rejected. The default T2 posture already uses Capsule as a policy layer on top of namespaces, default-deny networking, and workload identity — the soft-isolation tools appropriate for a single organisation. Introducing a *different* hard-or-semi-hard mechanism for T2 would reopen the multi-mechanism complexity ADR 0023 explicitly closed by choosing Kamaji as the single hard-MT mechanism. The decision here keeps soft isolation as the default and Kamaji as the one hard-MT escalation, rather than adding a third option.

### Make hard multi-tenancy a per-`Tenant` runtime concern with no substrate flag

Rejected for launch. Kamaji's control plane is a substrate-level installation (a controller plus per-tenant API-server pods backed by a shared datastore), not something that can be toggled purely per `Tenant` without first installing the mechanism. A cluster-level PlatformStack flag that gates whether the mechanism is installed at all is the correct granularity for an opt-in that the launch default leaves off; per-`Tenant` configuration remains available once the mechanism is enabled, exactly as ADR 0023 describes.

## Risks

- **Spec / ADR divergence until §4.1 (and mirrors) are updated.** `spec.md` still states "Kamaji default" for hardware T2. Mitigation: the spec edits in §4.1, §1.8, §3.9, the §5 technology-stack table, and Appendix C are a tracked follow-up to this ADR; this ADR is the governing decision in the interim and is referenced from the edits.
- **A launch customer expects hard isolation by default and does not find it.** A customer who internalised the earlier "T2 = hard multi-tenancy" framing may be surprised that the default T2 cluster is HA-only. Mitigation: onboarding and the hardware-tier description state plainly that hard multi-tenancy is an opt-in capability (`multitenancy: true`), not a T2 default, and the upgrade is a single flag with no migration.
- **The opt-in path lands later than the customer that needs it.** Kamaji and the `Tenant` CRD are in the post-launch backlog (`speedrun-plan.md` §6 item 4), so a service-provider or multi-organisation customer arriving at launch finds the capability planned but not yet shipped. Mitigation: this is exactly the re-evaluation trigger; the first such concrete signal both prioritises the backlog item and tests whether the default should flip. We accept that hard multi-tenancy is not shippable at the instant of the first request.
- **Default drift across hardware tiers.** Hard multi-tenancy is opt-in on T2 but default on T3/T4, so the default posture changes with the substrate. Mitigation: this is intentional and recorded here and (after the update) in `spec.md` §4.1; the per-tier table is the single source of the default, and the mechanism is identical across tiers so only the default differs.

## Owner

Core platform team. Andrey Ryahovskiy (`remryahirev@gmail.com`) convenes reviews and approves amendments. The Kamaji opt-in lands in the post-launch backlog (`speedrun-plan.md` §6 item 4); the default HA-only T2 substrate is in the open-source core at launch.

## Re-evaluation

Re-evaluate when:

- The **first service-provider or multi-organisation hard-isolation request** arrives — a customer hosting mutually-untrusting organisations on one cluster, or otherwise asking for structural tenant isolation on hardware T2. This both prioritises the Kamaji opt-in backlog item and is the signal to reconsider whether hard multi-tenancy should become the T2 default after all.
- Such requests become a consistent share of the hardware T2 segment rather than an occasional one — at which point flipping the T2 default back to Kamaji-on (re-aligning with the original ADR 0023 framing) should be weighed against the single-organisation majority.
- A simpler hard-isolation mechanism than Kamaji emerges with the same guarantees and lower operational overhead — which would be evaluated under ADR 0023's own re-evaluation triggers, not here, since the mechanism choice is ADR 0023's concern.

Otherwise no scheduled re-evaluation.

## References

- `speedrun-plan.md` §5.7 (this deviation: hardware T2 = HA substrate only, Kamaji opt-in via `PlatformStack.spec.values.multitenancy: true`, default off; spec §4.1 update required), §2.1 / §2.3 (T2 substrate pulled into the open-source core; Kamaji + `Tenant` CRD deferred to the post-launch backlog with the MSP / multi-organisation trigger), §6 item 4 (Kamaji + `Tenant` CRD opt-in post-launch ordering).
- ADR 0023 — Kamaji as the single hard multi-tenancy mechanism; Capsule as policy layer (**amended**: the hardware T2 entry moves from "Kamaji default, opt-out" to "Kamaji opt-in, default off"; the mechanism is unchanged).
- ADR 0022 — hardware tier model (substrate only; HA and hard multi-tenancy are orthogonal layers, not tier-defining).
- ADR 0033 — tenant security configuration (`strictMode` / `confidential` switches; orthogonal to both the hardware tier and this multi-tenancy default).
- ADR 0034 — managed offering model and canonical terminology (hardware tier vs managed plan; open-core split; this decision concerns the hardware-tier axis only).
- ADR 0005 — kine+NATS over etcd (the launch T2 substrate keeps etcd for HA storage; orthogonal to this decision).
- ADR 0026 — PlatformStack CRD (the `spec.values.multitenancy` control surface this opt-in uses).
- `spec.md` §4.1 (Compute Substrate, per-tier multi-tenancy column — update required), §3.9 (`Tenant` CRD), §1.8 (solo-tier adoption and migration pathway), §5 (technology-stack table), Appendix C.
