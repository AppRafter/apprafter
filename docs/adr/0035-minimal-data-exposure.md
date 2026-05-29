# ADR 0035: Minimal Data Exposure — managed services see metadata only, never customer data

## Status

Accepted (2026-05-29).

## Context

ADR 0034 cemented the managed offering model: AppRafter hosts a management/UX layer while the customer owns and runs the cluster, its workloads, and all of its data. The customer cluster connects through an outbound `apprafter-agent` (ADR 0031) that dials out to the hosted bus — no inbound listener, no firewall rules — and AppRafter holds no customer cluster credentials, no kubeconfigs, and makes no reverse-direction API calls into the customer's Kubernetes API. ADR 0034 stated that what the hosted layer receives is metadata only, and forwarded the principle behind that statement to this ADR.

The forces that make this worth cementing as its own decision, rather than leaving it implicit in the connection model:

- **Compliance scope is set by what data crosses the boundary, not by policy promises.** A managed layer that physically receives customer data — application records, log contents, message payloads — is in scope for the customer's data-protection obligations regardless of how carefully it handles that data. A managed layer that receives only operational metadata is in a materially different position. The architectural choice precedes and bounds every compliance argument; if the boundary is not specified, individual features will erode it one convenience at a time.

- **Several managed features look, naively, like they want customer data.** Cost monitoring, backup orchestration, log aggregation, an audit trail, and AI/insights each have an obvious-but-wrong implementation in which bytes of customer data flow to the hosted side. Each has a correct implementation in which only metadata flows. Without a stated constraint, the obvious implementation wins by default during feature design.

- **The EU-sovereignty segment treats this as a gating requirement.** Customers whose own clients demand that personal data stay on infrastructure under the customer's sole control cannot accept a managed layer that ingests that data. The same property is useful to solo developers, small teams, and managed-service providers (MSPs), but for the sovereignty segment it is an entry condition for evaluation. Designing the constraint in from the start of managed work raises the floor for every segment at once; see `speedrun-plan.md` for the durable framing of the launch audience and the managed product shape.

This ADR fixes the data-exposure boundary as a hard architectural constraint and gives feature design a single decision rule to test against. It does not introduce new schema.

## Decision

**Managed services see metadata only; customer data never crosses from the customer cluster to AppRafter. This is a hard architectural constraint, not a feature.** "Customer data" means the contents of the customer's data plane: application records, the contents of logs, the payloads of messages and streams, the bytes stored in databases and object storage, and any application-level event describing what an end user did. "Metadata" means operational facts about the cluster and its resources: resource metrics, manifest-apply and status events, volume and throughput counters, structural descriptions of resources, and AppRafter's own operations audit. The constraint binds every managed plan (Hosted Services, Managed Operations, Turnkey Cloud) and every hardware tier (T1–T4); it is orthogonal to the `strictMode` and `confidential` switches of ADR 0033, which it neither requires nor relaxes.

The constraint is realised feature by feature. Each managed feature is designed so that only metadata crosses the boundary defined by the outbound agent (ADR 0031):

- **Cost monitoring** sees resource metrics — CPU, memory, storage, and network usage attributed to a pod or an application — not the contents of any data those workloads process. A query that a workload runs against a database is, to cost monitoring, a unit of CPU and I/O on a named resource; its subject matter never appears.

- **Backup orchestration** coordinates timing, retention, and destination; the bytes move customer-side through the data plane's own native replication mechanisms (for example, a managed PostgreSQL operator streaming directly to the customer's object storage). AppRafter schedules and observes the operation as metadata — that a backup ran, when, how large, whether it succeeded — and the backup contents never transit the hosted layer.

- **Log aggregation** defaults to logs staying in the customer cluster. The customer chooses what, if anything, to forward and where. The hosted side, by default, sees volume metrics about logs (rate, size, error counts) and not their contents; forwarding contents off-cluster is an explicit, customer-controlled opt-in, not a default behaviour of the managed layer.

- **The managed audit log** records operations metadata — manifest applies, status transitions, access-grant operations, the AppRafter operations performed for the customer — and not application-level events, which remain in the customer cluster. AppRafter's audit trail describes what AppRafter and its operators did, not what the customer's end users did inside the customer's applications.

- **AI / insights** run on aggregated metrics and structural metadata (shapes, counts, resource relationships, time series), never on the contents of customer data. Where an AI provider is involved as an optional, opt-in sub-processor, only this aggregated metadata and structural metadata reach it.

### Design rule

**Every managed feature must answer one question before it ships: "what crosses from the customer cluster to us?" If the answer includes customer data, the feature is redesigned until the answer is metadata only.** This rule is applied at design time, not as an after-the-fact audit. A feature whose value genuinely requires customer-data contents to cross the boundary is not built in the managed layer; it is either redesigned to operate on metadata, performed entirely customer-side (the hosted layer only orchestrating, as with backup), or left to the open-source core that runs inside the customer's own cluster.

This rule is the operational complement to ADR 0034's connection model: the agent provides no path for AppRafter to pull customer data, and this rule prevents any managed feature from pushing customer data through the channel that does exist. Where a customer wants strong technical guarantees beyond this architectural boundary — protection against host-access adversaries, including AppRafter staff in plans where AppRafter operates the host — the `confidential` switch of ADR 0033 is the orthogonal mechanism; this ADR's constraint and that switch are independent and complementary.

### Compliance posture

The following is a neutral statement of how the metadata-only boundary interacts with common compliance frameworks. **It is informational and is not legal advice; customers should obtain their own legal and compliance review.**

- **GDPR.** The customer's application is the data controller and the customer's cluster — which the customer operates on their own infrastructure — is where processing happens. Because the contents of personal data do not cross to AppRafter under this constraint, AppRafter's data flow is limited to operational metadata. Customers should assess their own controller/processor analysis against their specific deployment and the metadata that the managed layer receives.

- **SOC 2.** Confining what the managed layer receives to platform metadata reduces the surface of customer-confidential data within AppRafter's control environment, which correspondingly narrows the scope an audit of AppRafter's controls must cover. The specific scope is for the customer and their auditor to determine.

- **HIPAA.** Because protected health information would not physically transit AppRafter's infrastructure under this constraint, the customer's analysis of AppRafter's role may differ from that of a provider that ingests such data; the precise characterisation, and whether any agreement is required, is a determination for the customer's counsel and depends on the customer's specific configuration and use.

The architectural point underlying all three is the same: the metadata-only boundary is a structural property of the system rather than a procedural promise, and it is verifiable. Because AppRafter's operations audit records everything the hosted layer receives, a customer's compliance team can inspect the audit trail and reason about the data flow directly rather than relying on assurances. Unlike a hyperscaler managed-Kubernetes offering, where the provider operates the control plane and the data plane runs on the provider's account, the AppRafter customer cluster — and the customer's data within it — stays on the customer's own infrastructure.

## Consequences

- **Compliance scope is bounded by architecture.** Because customer-data contents never reach the hosted layer, AppRafter's exposure under the customer's data-protection obligations is limited to operational metadata. This is a structural property available from launch and consistent across all managed plans.
- **Feature design gains a single, testable gate.** The "what crosses to us?" question gives every managed feature a clear pass/fail check, preventing scope creep into customer-data ingestion one feature at a time.
- **The boundary is auditable, not merely promised.** AppRafter's operations audit captures what the hosted layer receives, so customers can verify the metadata-only claim against records rather than trusting it.
- **Some otherwise-simple feature designs cost more.** Implementations that would be trivial if customer data could flow to the hosted side (centralised log search over raw contents, server-side analysis of record-level data) must instead be built on metadata, performed customer-side with the hosted layer orchestrating, or declined. This raises the engineering cost of those features and constrains their shape.
- **Customer-side responsibility is explicit.** Data-path operations (backup bytes, log contents, the data plane itself) remain the customer's; the hosted layer coordinates but does not custody them. This is consistent with ADR 0034's responsibility split and the open-core principle that everything required to run the cluster lives in the customer's own cluster.
- **Optional sub-processors stay narrow.** Where an AI provider participates, it receives only aggregated and structural metadata under customer opt-in, keeping the sub-processor relationship correspondingly limited in scope.

## Alternatives considered

### Ingest customer data into the hosted layer for richer managed features

Rejected. Pulling log contents, record-level data, or message payloads to the hosted side would enable richer centralised features (full-text log search, record-level analytics, cross-customer learning on raw data) but would:

- **Enlarge compliance scope structurally.** Any framework analysis would then have to account for customer-data contents inside AppRafter's environment, the opposite of the bounded position this ADR establishes.
- **Contradict the sovereignty guarantee.** The EU-sovereignty segment requires that the contents of customer data stay on infrastructure under the customer's sole control; ingestion breaks that on its face.
- **Concentrate blast radius.** A hosted-side compromise would expose customer-data contents. Confining the hosted layer to metadata bounds such a compromise to operational facts, consistent with the credential-minimisation rationale of ADR 0034.

Richer features are instead pursued on metadata, or performed customer-side with the hosted layer orchestrating.

### Treat metadata-only as a policy/contractual promise rather than an architectural constraint

Rejected. A policy commitment that could be relaxed by a future feature decision provides none of the structural guarantees customers are evaluating. Making the boundary architectural — enforced by the agent connection model and the design rule, and verifiable through the operations audit — is what turns "compliance-friendly" from a marketing claim into a property of the system.

### Gate metadata-only to specific plans or tiers, or to the confidential switch

Rejected. The value of the constraint is that it holds universally; a customer should not have to reason about whether their chosen plan, hardware tier, or security switch changes what data AppRafter can see. The constraint is therefore orthogonal to all of those axes. The `confidential` switch (ADR 0033) addresses a different threat — host-access adversaries — and complements, rather than implements, this constraint.

## Risks

- **Feature pressure to ingest customer data.** A future feature may appear to need customer-data contents to deliver its value. Mitigation: the design rule is mandatory at design time; such a feature is redesigned onto metadata, made customer-side with hosted orchestration, or declined. Accept that some feature shapes are foregone.
- **Definitional drift between "metadata" and "customer data".** Edge cases will arise (rich error messages that embed record fragments, structured logs that carry payload values, metric labels that leak identifiers). Mitigation: treat the contents of customer data as the protected category and label cardinality/free-text as a review item; when in doubt, classify as customer data and keep it customer-side.
- **Opt-in log forwarding misconfigured by a customer.** A customer can choose to forward log contents off-cluster; a misconfiguration could send more than intended. Mitigation: forwarding is off by default, scoped explicitly by the customer, and surfaced in the operations audit so it is visible and reviewable.
- **Compliance statements over-read as legal advice.** The posture section could be mistaken for a formal compliance determination. Mitigation: the not-legal-advice framing is explicit and repeated; customer-specific determinations are left to the customer's counsel.
- **AI/insights metadata indirectly reconstructing sensitive information.** Aggregated metrics and structural metadata could, in principle, allow inference about underlying data. Mitigation: AI/insights operate on aggregated and structural metadata only, AI providers are opt-in sub-processors, and the confidential switch (ADR 0033) remains available to customers whose threat model requires it.

## Owner

Core platform team. Andrey Ryahovskiy (`remryahirev@gmail.com`) convenes reviews and approves amendments. The constraint binds the managed-services track; the open-source core, which runs entirely in the customer's cluster, is unaffected because it does not cross the boundary.

## Re-evaluation

Re-evaluate when:

- A managed feature is proposed whose value appears to require customer-data contents to cross the boundary — confirm whether a metadata-only or customer-side-orchestrated design exists, or whether the feature stays out of the managed layer.
- The hosted MCP read-surface is specified (ADR 0036) — confirm that the read surface returns only metadata over the agent channel and inherits this constraint.
- Managed Operations or Turnkey Cloud planning opens — re-verify that the heavier plans' added automation does not introduce a customer-data flow to the hosted side.
- The optional AI/insights sub-processor relationship changes scope — re-confirm that only aggregated and structural metadata reaches any external provider.
- A compliance framework or its authoritative guidance materially changes such that the metadata-only characterisation needs restating.

## References

- `speedrun-plan.md` (durable context for the managed launch shape, the launch audience, and the hosted-services / agent-registration model that this constraint sits on top of).
- ADR 0031 — `apprafter-agent` ↔ hosted-bus protocol (outbound agent, no inbound listener, no held credentials; the channel this constraint governs).
- ADR 0033 — tenant security configuration (`strictMode` / `confidential` switches; the orthogonal, complementary mechanism for host-access threat models).
- ADR 0034 — managed offering model (hardware tier vs managed plan; metadata-only stance forwarded here).
- ADR 0036 — MCP server and agentic-safety model (inherits and applies this constraint to the MCP read surface).
