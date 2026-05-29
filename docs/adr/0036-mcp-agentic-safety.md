# ADR 0036: MCP server and agentic-safety model — structural enforcement at the platform boundary

## Status

Accepted (2026-05-29).

## Context

AppRafter exposes a Model Context Protocol (MCP) server so that AI clients (the agent inside an IDE, a coding assistant, a CI bot) can drive the platform — list applications, read status, deploy to staging, scale, and so on. This is a launch-relevant capability for the Hosted Services managed plan: a meaningful part of the target segment (solo developers and small teams whose primary workflow is "an agent builds and deploys for me") expects an AI-native onboarding and operations story. Exposing an MCP surface inherits an attack surface that the wider agentic-tooling ecosystem has demonstrated to be hazardous: over a roughly sixteen-month window the public record accumulated more than a dozen agent-driven destructive incidents across a handful of tools, several MCP-specific CVE classes (DNS rebinding in MCP SDKs, command injection in cluster-management MCP servers, path-traversal scope escape, argument injection and supply-chain RCE chains across wrapped CLIs, and remote-proxy RCE), and tens of thousands of exposed MCP endpoints leaking credentials.

Two prior ADRs frame the relevant defensive primitives but do not address the agentic case specifically:

- **ADR 0024** removes standing cluster-admin and replaces it with a defence-in-depth bundle: SPIFFE workload identity, OpenBao secret delivery, Kamaji control-plane separation, a two-person rule and just-in-time short-TTL grants for elevated host access, and an immutable audit pipeline. There is no standing all-powerful credential for an agent to inherit.
- **ADR 0033** decomposes tenant data protection into two orthogonal switches: `strictMode` (admission deny of `pods/exec`, `pods/attach`, `pods/portforward`; PodSpec immutability; vault-injector delivering secrets to process memory rather than into the Kubernetes API; image-signature enforcement) and `confidential` (CoCo hardware memory encryption with a customer-placed KMS and verifier). These bound what any actor — human or agent — can reach and exfiltrate.

The governing lesson from the incident record is that an agent must be treated as **outside the trust boundary and potentially compromised**. System-prompt rules, project configuration, and model-level safety are all internal to the agent and have been shown to be insufficient — agents have demonstrably violated their own stated rules. Safety therefore has to be enforced **structurally at the platform boundary**, not delegated to model behaviour.

Two distinct attacker models must both be covered:

1. **Agent self-derails (excessive agency).** A non-malicious agent with over-broad credentials and non-deterministic reasoning performs a destructive operation without confirmation.
2. **External prompt injection.** An external attacker injects instructions through untrusted content the agent reads (logs, files, web pages, the descriptions of tools themselves), or exploits a vulnerability in the MCP server itself (command injection, path traversal, DNS rebinding, RCE in a wrapped tool).

A convergent-design observation makes this tractable: the `MigrationPlan` gate (ADR 0012, unified in ADR 0027) was originally designed to protect against a human operator fat-fingering a destructive change. Its control is **actor-agnostic** — "a destructive change requires an out-of-band approval the actor cannot complete itself" — and therefore stops attacker model 1 with no agent-specific machinery. There is no agent-only bypass and no agent-only gate; the same gate stops a human mistake and an agent.

Durable context for the launch framing and the segments this serves is recorded in `speedrun-plan.md` (Hosted Services as the launch plan; MCP-native onboarding as a primary loop for the launch segment).

## Decision

### The MCP server is built-in, on OneBun, validating with ArkType

We will ship a single built-in HTTP(S) MCP server, implemented on OneBun (the in-house NestJS-style Bun framework, consistent with the rest of the hosted services stack) with ArkType schema validation on every tool input. **No third-party MCP servers are connected.** Built-in-only is itself a security decision: it removes the entire class of supply-chain RCE chains, tool-poisoning via untrusted tool descriptions, and reference/community-server vulnerabilities, because there is no untrusted server to poison and no chain of foreign tools to compose into an exploit.

### The MCP surface is AppRafter CRDs, not raw Kubernetes

The server's surface is the AppRafter custom resources (`Application` and the other platform CRDs), **not** the raw Kubernetes API. Every mutation an agent can express is a CRD edit that flows through the operator, the admission webhook, and — where the change is destructive — the `MigrationPlan` gate, with exactly the same guarantees as any other change to the platform. The agent can express "set replicas", "update image", "add an environment variable"; it **cannot** express "delete this volume" or "drop this database" as a direct imperative — that vocabulary does not exist on the surface. Destructive intent expressed against a CRD is gated by the operator and `MigrationPlan` like any other destructive change. This is the primary structural answer to excessive agency: the dangerous operations are simply not expressible as direct, un-gated actions.

### Risk taxonomy and routing

MCP operations are classified into four risk levels, each routed to a control that exists by construction:

| Level | Examples | Control |
|---|---|---|
| **Safe (pure read)** | list applications, get status, get metrics, get logs (subject to read-surface design, below) | none beyond authentication and audit |
| **Reversible write** | scale, restart, redeploy | audit; default agent grant permits |
| **Bounded write** | create a dev environment, deploy to staging | audit; scope-gated; managed plans may add review |
| **Destructive** | delete an application, drop a resource claim, change tier, migrate data | `MigrationPlan` gate — out-of-band human approval, always |

Destructive operations route through the `MigrationPlan` actor-agnostic gate. The gate must cover destructive operations on **core Kubernetes resources** (for example a raw namespace deletion), not only on AppRafter CRDs — otherwise a broad credential could bypass it. This works in concert with ADR 0024, which removes the standing cluster-admin that such a bypass would require. **An agent cannot auto-approve**: approval is out-of-band (at launch, the `apprafter` CLI and Argo CD approve/reject buttons; a Backstage approval surface follows in the post-launch bundle), requires a human OIDC identity, and is structurally outside the channel the agent acts through. Approval is never expressible with an agent token.

### Credentials are AccessGrant-derived, scoped, short-TTL, human-bound

An agent's MCP credential is derived from a human's `AccessGrant`, not minted independently. It carries a label (per device or per integration), a scope no broader than the parent grant (downward propagation only), and a short TTL. The default agent grant is **read plus non-destructive write**; destructive capability is never in the default agent role. The credential is bound to a human identity for accountability — the audit record carries both identifiers, so an action reads as "this human's grant, via this token, in this session". When the parent grant expires or is revoked, all derived agent tokens revoke with it; an individual token can also be revoked alone, so compromise of one device does not invalidate the others. We do not attempt to enforce a credential subset the platform cannot see (a human may hand an agent a kubeconfig entirely outside the MCP channel); instead we compress the superset toward least privilege and gate the dangerous **verbs** regardless of which credential carries them.

### Structural-first, with the residual controls the checklist adds on top

The two attacker models are answered primarily by the design above. On top of the by-construction design, an internal operational security checklist adds a set of residual controls that the structural design does not by itself cover:

- **Transport hardening of the MCP server.** No unauthenticated HTTP transport; DNS-rebinding protection enabled; the endpoint is never on the public internet (default-deny network policy plus the access-plane reachability path), authenticated, and mutually-authenticated where the transport allows. On any path that does shell out (cluster-bootstrap helm/kubectl invocations, not the CRD path, which has no shell surface), commands are built with explicit argument vectors, never by string concatenation; every wrapped tool uses a flag allowlist; every path argument is canonicalised before authorisation.
- **Read-surface design.** Because for an external agent (the typical case — an agent running on a laptop or a vendor cloud) the read phase is the entire data-theft exposure, what a read tool is *permitted to return* is the load-bearing design question. Read tools return operational metadata — status, counts, resource usage, structural shape — and **never** secrets, PII, raw application data, or raw database contents. This is the agentic instance of the minimal-data-exposure principle.
- **Secrets out of reach.** Under `strictMode`, vault-injector places secrets in process memory only, so they never appear in the PodSpec, a ConfigMap, a Kubernetes Secret, or the API surface a read tool could traverse. The agent physically cannot read them.
- **Scope-gating the verbs.** RBAC roles for the MCP agent are distinct from human roles; destructive verbs are absent from the default agent role; blast radius is bounded to a single tenant.

A basic destructive-attempt alert (an ordinary audit event raised when admission or `MigrationPlan` rejects a destructive operation from an agent token) is part of the open-source minimum at near-zero marginal cost; advanced behavioural anomaly detection (recon-pattern detection, unusual-egress detection, behavioural baselining) is a managed-plan premium add-on, since it requires an observability stack to operate.

The full CVE-mapped operational checklist — every attack class mapped to "already covered by construction / residual gap / control / how to verify", with the verified CVE catalogue and the OWASP / MITRE ATLAS / NIST cross-references — is maintained internally and is not reproduced here. This ADR cements the model; that internal checklist is the living operational companion, revisited on each new MCP CVE.

### Relationship to deployment mode and the open-core split

The agentic-safety model is independent of the hardware-tier axis (T1 Solo through T4 Regulated, per ADR 0022, which describes the compute substrate only) and of the managed-plan axis (Hosted Services, Managed Operations, Turnkey Cloud, plus a reserved Enterprise plan, per ADR 0034, which describes the operational relationship only). The `strictMode` and `confidential` switches that bound an agent's reach (ADR 0033) are selected independently on the Tenant CRD and are orthogonal to both axes. In Hosted Services (the launch plan), the customer cluster connects via the outbound `apprafter-agent` (ADR 0031): it dials out to the hosted bus, there is no inbound listener and no firewall change, AppRafter holds no customer cluster credentials and no kubeconfigs and makes no reverse-direction calls into the customer Kubernetes API, and the hosted MCP server reaches the cluster only by proxying through that outbound agent. The structural guarantees in this ADR therefore hold whether the MCP server runs hosted or self-hosted. The platform is fully functional as open source; the managed plans add premium quality-of-life and operations (centralised review, advanced anomaly detection, richer token-rotation policies), never a structural dependency — a customer can always leave with the entire cluster intact, cancellation being a registration revocation rather than a migration.

## Consequences

**Positive:**

- The dangerous operation classes are not expressible as un-gated direct actions. Excessive agency is answered structurally, not by trusting the model.
- One gate (`MigrationPlan`) covers the human fat-finger and the agent identically; no agent-specific bypass exists, and the gate could not be added "just for agents" and later relaxed.
- Built-in-only eliminates the supply-chain, tool-poisoning, and foreign-chain RCE classes outright.
- Credentials are short-lived, scoped, human-bound, and individually revocable; the blast radius of a compromised agent device is one token, bounded to one tenant.
- The same model holds across hardware tiers and managed plans, and across hosted and self-hosted MCP, because it sits on platform primitives that already exist.

**Negative:**

- A built-in-only MCP server forecloses the convenience of plugging in community MCP servers. This is intentional and mirrors the "no community plugins in v1" stance; the trade-off is convenience surrendered for a removed attack-surface class.
- Read-surface design imposes ongoing discipline: every new read tool must be reviewed for what it may return, and a careless tool that returns raw data would silently widen the agent's reach. This is a standing review obligation, not a one-time control.
- Destructive operations always require an out-of-band human approval, which adds friction to legitimately automated destructive workflows. Accepted: this is the same friction ADR 0012 accepts for human operators.

**Neutral:**

- Advanced behavioural anomaly detection is deferred to the managed plans, so open-source self-hosters get the structural guarantees plus the basic destructive-attempt alert, but not behavioural baselining. The structural controls do the heavy lifting; anomaly detection is a defence-in-depth layer, not the primary control.

## Alternatives considered

- **Enforce agent safety via system prompt and model-level safety.** Rejected as the primary control. The incident record shows agents violating their own stated rules; an agent treated as inside the trust boundary is a single prompt injection away from acting with its full credential. Model-level measures are a useful additional layer but cannot be the boundary.
- **Expose the raw Kubernetes API to the agent with RBAC scoping.** Rejected. RBAC alone does not gate conditionally-destructive operations (scaling to zero, changing a storage class) and pushes the entire destructive vocabulary onto the surface; the CRD surface makes the dangerous imperatives unexpressible and routes the genuinely destructive ones through one auditable gate.
- **Connect best-of-breed third-party MCP servers.** Rejected for v1. It reintroduces the supply-chain, tool-poisoning, argument-injection, and chain-RCE classes the built-in-only decision removes. The one-technology-per-slot principle applies here as elsewhere.
- **A separate, agent-specific destructive-approval gate.** Rejected. An agent-only gate is both redundant (the actor-agnostic `MigrationPlan` already covers agents) and a liability (a parallel gate is a candidate for an agent-only relaxation later). One gate for all actors is the safer invariant.
- **Long-lived static agent tokens in a config file.** Rejected. The incident record includes an agent scavenging an unrelated file for a root token; static tokens at rest are exactly the failure mode. AccessGrant-derived, short-TTL, injected credentials replace them.

## Risk

- **A new read tool leaks raw data.** A tool added without read-surface review could return secrets or application data, widening agent reach. Mitigation: read-surface review is a required checklist item for every new MCP tool; `strictMode` vault-injector keeps secrets out of any API surface a tool could traverse even if a tool is over-broad.
- **A destructive path exists outside the modelled operator vocabulary** (an un-modelled CRD field, or a raw-Kubernetes bypass). Mitigation: core-resource coverage in the `MigrationPlan` gate plus ADR 0024's removal of standing cluster-admin; compiler-enforced exhaustiveness over the operator's destructive classification so no operator-level operation can be left unclassified silently. Caveat: exhaustiveness guarantees "no operator operation is unclassified", not "no destructive path exists anywhere" — the raw-Kubernetes residue is held by ADR 0024.
- **External-agent exfiltration after a legitimate read.** For an external agent, data that left as a legitimate API response is exfiltrated from the agent's own environment, where cluster egress controls (Cilium FQDN policies) cannot see it. Mitigation lives entirely in the read phase: read-surface design plus secret-scoping plus `strictMode` ensure the agent never acquires secrets, PII, or raw data in the first place. Cilium FQDN egress control remains load-bearing for the distinct in-cluster workload-exfiltration actor, not for the agent.
- **MCP server transport vulnerability** (DNS rebinding, an injection on a shell-out path). Mitigation: the transport-hardening controls above, plus a CI security smoke gate (static analysis for shell concatenation, path-traversal tests, "destructive without MigrationPlan rejects" tests) and periodic red-teaming against the documented attack classes.
- **Hosted-bus compromise** could enable a man-in-the-middle on MCP operations. It cannot expose customer data, which never leaves the customer cluster (ADR 0031, ADR 0035). The blast radius is operational, not data-confidentiality.
- **Multi-tenant blast radius of the hosted MCP authentication path** is a distinct threat model not fully worked through at launch. We accept this as a known marker for the managed-plan security design and bound agent capability to a single tenant in the interim.

## Owner

Core platform team. Andrey Ryahovskiy (`remryahirev@gmail.com`) convenes reviews and approves amendments. Phase assignment: Phase 4 (managed-plan design), landing before the first MCP operation reaches a real cluster. The transport-hardening and CI security-smoke items are tracked against the MCP server implementation; the destructive-inventory and core-resource gate coverage are tracked jointly with the `MigrationPlan` (ADR 0027) and cluster-admin-constrain (ADR 0024) work.

## Re-evaluation triggers

- A new MCP-specific CVE class emerges that the current controls do not cover. Trigger to update the internal operational checklist and, if structural, revisit this ADR.
- A material update to OWASP Top-10 for Agentic Applications, MITRE ATLAS, or NIST AI agent guidance shifts the consensus on a control.
- Customer demand for third-party MCP servers reaches a level where the built-in-only stance imposes a real cost; would reconsider an audited, signed, scope-gated extension mechanism rather than open plug-in.
- The multi-tenant hosted MCP authentication design is worked through; would land its own ADR and may amend the credential model here.
- Behavioural anomaly detection moves from managed-premium toward an open-source baseline as its operational cost drops.

## References

- ADR 0012 — MigrationPlan as a first-class concept (the human fat-finger gate the agentic model reuses).
- ADR 0024 — Cluster-admin constrain bundle (no standing cluster-admin to inherit; the residual raw-Kubernetes destructive surface is held here).
- ADR 0027 — MigrationPlan unification with scope discriminator (one gate across application and platform scope; core-resource coverage).
- ADR 0031 — `apprafter-agent` ↔ hosted-bus protocol (outbound connection model; hosted MCP reaches the cluster only via the agent; no held credentials).
- ADR 0033 — Tenant security configuration (`strictMode` / `confidential` switches that bound agent reach and exfiltration).
- ADR 0034 — Managed offering model and terminology (hardware-tier vs managed-plan axes; open-core split).
- ADR 0035 — Minimal data exposure (metadata-only constraint; the read-surface design here is its agentic instance).
- ADR 0022 — Tier model clarification (hardware-tier axis: T1 Solo through T4 Regulated).
- An internal, CVE-mapped operational security checklist is the operational companion this ADR cements; it is maintained separately and revisited on each new MCP CVE.
- `speedrun-plan.md` — Hosted Services as the launch plan; MCP-native onboarding for the launch segment.
- spec.md §3.8 (MigrationPlan), §3.4 (AccessGrant), §4.4 (workload identity and secrets), §4.10 (audit pipeline).
