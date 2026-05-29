# ADR 0031: `apprafter-agent` ↔ hosted-bus protocol — gRPC streaming with Rust agent

## Status

Accepted (2026-05-29).

The gRPC-streaming-on-launch decision (gRPC streaming over HTTP/2 with TLS, Rust agent, Bun host) is ratified.

## Context

AppRafter's managed offering comprises three managed plans (ADR 0034): Hosted Services, Managed Operations, and Turnkey Cloud. The launch managed plan is Hosted Services (`speedrun-plan.md` §0.5): we host the Backstage portal, Account UI, MCP server, and supporting infrastructure on our side; customer clusters live on the customer's own infrastructure and remain fully autonomous open-source installs. The two sides communicate via a long-lived connection initiated from the customer cluster.

This communication channel — designated `apprafter-agent` ↔ hosted-bus — has the following requirements:

- **Outbound-only from customer cluster.** Customer must not configure inbound firewall rules; the target operators are not expected to manage cloud-firewall or VPS-level iptables configuration. The agent initiates the connection; the hosted side never reaches into the customer's Kubernetes API.
- **Bi-directional message flow over a single connection.** Customer → hosted: status updates, audit events, deploy notifications, log streams (opt-in). Hosted → customer: MCP-initiated operations (deploy, scale, restart), `MigrationPlan` creation triggers, configuration push (token rotation).
- **Persistent and resilient.** Long-lived connection with automatic reconnect on transient network failures. Status cache on hosted side absorbs short disconnects without surfacing them as customer-visible failures.
- **Strong authentication and authorization.** Each agent carries a customer-scoped registration token (issued during cluster registration, revocable from the Account UI). All operations are auditable to a (customer, cluster, session) tuple.
- **Resource-frugal in the customer cluster.** Tier 1 substrate is a single `cpx22` (4 GB RAM, 2 vCPU) on Hetzner; the agent should consume a small fraction of those resources, leaving the substrate available for the customer's own workloads.
- **Type-safe across language boundaries.** Customer-side runtime and hosted-side runtime are different. Manual JSON schema synchronization across two stacks is error-prone in API evolution; the project's experience with schema drift has already motivated CUE adoption for configuration (ADR 0029). A similar discipline applies to the agent protocol.
- **Compatible with a future transition to NATS-backed control plane.** ADR 0028 and the kine+NATS deferred item (`plan.md` 3.2, `speedrun-plan.md` §2.3 bucket C) describe a path where the audit log becomes replayable via JetStream. The launch protocol must not preclude a migration to NATS-based transport, but should not force NATS infrastructure on day one when it is not yet in the OSS-core scope.

Three protocol candidates were evaluated: WebSocket, gRPC streaming, and a NATS client. Agent-side runtime candidates were Rust (matching the existing `apprafter` operator) and OneBun/TypeScript (matching the host-side stack).

## Decision

The `apprafter-agent` ↔ hosted-bus channel uses **gRPC streaming over HTTP/2 with TLS**. The agent is implemented in **Rust** (shipped as part of the `apprafter` operator binary or as a sibling binary in the same Cargo workspace). The hosted-side server is implemented in **TypeScript on Bun**, using the official `@grpc/grpc-js` library (or `nice-grpc` as a wrapper).

A single `.proto` file in the AppRafter monorepo is the source of truth for the schema. Rust types are generated via `tonic-build`; TypeScript types are generated via `ts-proto`. Both generators run in CI and are part of the build artifact validation.

### Why gRPC streaming

gRPC over HTTP/2 is the standard cloud-native pattern for agent ↔ control-plane communication. The Kubernetes ecosystem itself uses it (kubelet to API server, controller-runtime informers). The protocol supplies:

- **Bi-directional streaming** as a first-class primitive (`tonic::Streaming` on the Rust side, gRPC server streams on the Bun side). The single connection multiplexes events upstream and commands downstream using HTTP/2 stream multiplexing; no application-level framing is needed.
- **Strong typing** via protobuf 3, with code generation on both sides from one schema file.
- **Interceptor pattern** for cross-cutting concerns: the registration token is validated in one place via a metadata interceptor, audit logging hooks into the same point.
- **Mature tooling**: `grpcurl` for manual debugging, proto reflection for runtime introspection, `tonic` (Rust) and `@grpc/grpc-js` (Node/Bun) are both production-grade.
- **Outbound-only operation**: the agent dials the hosted endpoint; the connection then carries traffic in both directions over the same TCP connection. No inbound listener on the customer side.

### Why Rust agent (not OneBun)

The agent runs on customer hardware, which is finite. Approximate resource footprints:

- **Rust binary**: 10–20 MB statically linked binary; 20–50 MB resident memory under typical control-plane load. Compiles to a single binary; no runtime dependencies on the cluster.
- **OneBun (Bun + TypeScript bundle)**: 80–150 MB on disk (Bun runtime + dependencies); 100–200 MB resident memory.

In a Tier 1 `cpx22` (4 GB RAM total), the OneBun path consumes 2.5–5% of cluster RAM purely for control surface. The Rust path consumes 0.5–1%. The substrate exists to run the customer's workloads, so the control surface should remain a small fraction of available RAM.

The Rust agent also reuses the existing `apprafter` operator's toolchain: `kube-rs` for Kubernetes client work, `tonic` for gRPC, `serde` for serialization, `tokio` for async runtime. Whether the agent ships as a subcommand of the `apprafter` operator binary or as a sibling binary in the same Cargo workspace is an implementation detail; both options use the same dependency tree and CI pipeline. No new language runtime, no new security surface in the customer cluster, no new release process.

### Why Bun host (not Rust)

The hosted side runs the Account UI backend, Backstage backend, MCP server, and supporting services — collectively the OneBun stack the project has standardized on. The agent endpoint is one more service in this stack and benefits from sharing the runtime, deployment, and operational patterns of its neighbors. Rewriting the hosted side in Rust to match the agent would impose a polyglot operational burden that does not pay for itself; the host side is comfortable in TypeScript, and the agent ↔ host gRPC boundary is exactly the kind of seam where two languages can meet without friction.

### Schema management

A single `.proto` file lives under `apprafter/proto/agent_bus.proto` and defines:

- `AgentBus` service with three RPCs:
    - `Register(RegistrationToken) returns (AgentIdentity)` — initial handshake; the agent presents its registration token and receives a per-session identity.
    - `StreamEvents(stream AgentEvent) returns (stream HostAck)` — long-lived bidirectional stream for customer → hosted events with acknowledgments.
    - `ReceiveCommands(AgentIdentity) returns (stream HostCommand)` — long-lived server-streaming RPC for hosted → customer commands. Identity is passed once; commands flow until the stream closes.
- Message types for events (manifest applied, deploy status, audit event, log line, heartbeat), commands (deploy, scale, restart, MigrationPlan create, config push), and acknowledgments.

CI runs `tonic-build` to produce Rust types, `ts-proto` to produce TypeScript types. Both outputs are checked in as generated artifacts to make the API surface visible in code review; the generators run in CI to verify drift has not crept in. Breaking changes to the schema are explicit at code review time on both sides because compilation fails until generated types are updated.

### Authentication and connection lifecycle

The registration token is presented as gRPC metadata on every RPC call (most importantly on `Register`). A server-side interceptor validates it against the customer registry, resolves the customer + cluster identity, and attaches the identity to the request context for downstream handlers.

Connection lifecycle:

1. Agent dials hosted endpoint on first start.
2. Agent calls `Register(RegistrationToken)`; receives `AgentIdentity`.
3. Agent opens `StreamEvents` (writes events upstream, reads acknowledgments) and `ReceiveCommands` (reads commands downstream) concurrently, both gated by the identity.
4. On unexpected disconnect: agent retries with exponential backoff (jittered, capped at ~60s). Hosted side caches last-known status per agent to absorb gaps shorter than a configurable threshold.
5. On token revocation (customer cancels managed subscription): hosted side closes streams with a specific gRPC status code; agent stops retrying and the customer's cluster continues running OSS-only.

Heartbeat is implicit in the HTTP/2 stream lifecycle (keepalive frames). No application-level heartbeat is needed for liveness detection on the launch protocol; observability of the connection state can be added later if metrics show it is needed.

## Alternatives considered

### WebSocket

Bun has native WebSocket support, and the team has more direct experience with WebSocket than with gRPC. The trade-off is that WebSocket provides a transport, not a typed RPC framework. Building a typed bi-directional message system on top of WebSocket requires designing the message framing, the request/response correlation, the type schemas, and the code generation pipeline ourselves. The drift between hand-written JSON schemas and Rust/TS types is exactly the failure mode the project has already chosen CUE to avoid; replicating it for the agent protocol would be a regression in API discipline.

The simplicity advantage of WebSocket evaporates when the protocol has to grow: the first time a new event type is added without all consumers updating in lockstep, the manual schema drift surfaces as a runtime error rather than a compile-time error. gRPC moves that failure to compile time on both sides.

### NATS client

A NATS-based agent would be a natural fit if NATS were already in the substrate. The kine+NATS migration (`plan.md` 3.2) and the replayable audit log story (a structural moat in the launch positioning) both lean toward NATS as transport. However, on launch:

- Hosted side has no NATS server in scope — it would have to be added as new infrastructure purely to support the agent protocol.
- Customer side has no NATS server in scope — running a NATS client requires the JetStream account/stream provisioning that `needs.jetstream` (`plan.md` 2.5, bucket D in the speedrun) would normally handle, but `needs.jetstream` is dropped for launch.
- The NATS pub/sub semantic is awkward for request-response patterns that the MCP layer needs (`scale_app` returns a result; `delete_app` waits for MigrationPlan approval). Synthesizing request-response on top of pub/sub is possible but is more machinery than gRPC's native RPC semantics.

A migration from gRPC to NATS becomes possible (and attractive) once kine+NATS lands as control plane storage and JetStream is part of the OSS-core substrate. The migration cost is approximately 1–1.5 weeks of full-time work to swap the transport layer; the proto schema and most application logic carry over.

### OneBun agent in customer cluster

If both sides ran OneBun, type sharing would be trivial through shared TypeScript modules — no proto codegen, no two-language boundary. The cost is the resource footprint analysed above (2.5–5% of Tier 1 customer RAM for the Bun runtime). For a managed offering whose Tier 1 substrate is explicitly a single `cpx22`, this is a visible and persistent cost the customer pays before any of their workloads run. The Rust agent is in low single-digit MB of memory; the difference is meaningful enough at this scale to dominate the type-sharing convenience argument.

A secondary consideration is operational simplicity in the customer cluster: the existing `apprafter` operator is a single Rust binary. Adding a Bun runtime as a second managed component doubles the surface that has to be patched, monitored, and reasoned about during outages.

## Consequences

**Positive:**

- Type safety end-to-end via protobuf, with compile-time errors on schema drift on both sides.
- Minimal resource footprint in customer clusters (single Rust binary).
- Single-language operator stack in the customer cluster (Rust only).
- Outbound-only design satisfies the "no firewall changes" requirement structurally.
- Standard cloud-native protocol with mature tooling on both sides.
- Migration path to NATS is open without rewriting application logic — only the transport layer changes.

**Negative:**

- Bun host side needs to add `@grpc/grpc-js` (or `nice-grpc`) as a dependency and integrate protobuf code generation into the TypeScript build. Approximately 2–3 days of one-time setup work.
- Protobuf schema discipline must be maintained: backward compatibility on field changes, version negotiation if message types diverge between agent and host releases.
- gRPC over HTTP/2 occasionally interacts badly with corporate proxies that misimplement HTTP/2; this is rarely a customer-cluster problem (the agent dials directly out to our endpoint) but can affect managed-host development environments.

**Trade-offs:**

- Two languages on the build pipeline (Rust + TypeScript) and one schema language (protobuf) is a small operational overhead compared to a single-language stack on either side. The trade is justified by the asymmetry of constraints — host wants OneBun convenience, agent wants Rust efficiency.
- Future NATS migration costs approximately 1–1.5 weeks of work. That cost is paid only if/when audit replayability is prioritized as a launch differentiator.

## Risk

**Main risk:** schema drift in the generated TypeScript types if the `ts-proto` codegen step is skipped during a hurried fix. Mitigation: CI fails if generated types are out of sync with the `.proto` source; PR review checklist includes "generated types regenerated."

**Secondary risk:** `@grpc/grpc-js` is a Node-native library; while it works on Bun today, future Bun versions could regress compatibility. Mitigation: pin a known-good combination during launch preparation; consider migrating to a Bun-native gRPC implementation if one matures (e.g., `connect-es` over HTTP/2 directly, which Bun supports natively). The fallback is to run the hosted gRPC server on Node.js compatibility mode inside Bun, which Bun supports.

**Tertiary risk:** agent supply-chain compromise. Mitigation: agent binaries are signed with `cosign` (per the OSS-core release pipeline); customer's `apprafter cluster register` command verifies the signature before deploying the agent. Customer can revoke the registration token at any time; revocation cuts the channel immediately.

## Owner

Core platform team. The agent and the hosted-bus server are co-owned during initial implementation; on stabilization, the agent ships as part of the OSS `apprafter` operator release and the hosted-bus server ships as part of the managed-services repository.

## Re-evaluation triggers

- **NATS-backed control plane lands in OSS-core.** When `plan.md` 3.2 (kine+NATS migration) ships and JetStream becomes part of the substrate, re-evaluate switching the agent ↔ host transport from gRPC to NATS. Expected to coincide with prioritizing the replayable-audit-log capability.
- **Bun-native gRPC implementation matures.** If a first-class Bun gRPC library appears (e.g., via WebTransport or HTTP/2 native bindings), evaluate dropping `@grpc/grpc-js`. The proto schema is unaffected; only the host-side client/server library changes.
- **OneBun resource footprint shrinks dramatically.** If Bun's runtime overhead drops below ~30 MB resident in a hardened production build, reconsider OneBun for the agent — type sharing convenience would then outweigh the resource cost.
- **Schema evolution pain.** If protobuf's evolution model proves too restrictive in practice (e.g., field deprecation cycles cause two-week development blocks), re-evaluate. The most likely fallback is `bufbuild` tooling additions rather than a protocol change.

## Still open

- **Agent binary distribution model.** Two options: (a) ship as a separate binary `apprafter-agent` alongside the operator; (b) ship as a subcommand `apprafter operator agent`. Option (a) is simpler operationally; option (b) reuses the single-binary release artifact. Decision deferred to implementation (`speedrun-plan.md` §3.2).
- **Audit log retention on hosted side.** The 90-day audit-retention target for the managed plan applies to events flowing over this channel. The storage backing (PostgreSQL row store, ClickHouse if §2.3 bucket C lands, NATS JetStream if NATS migration lands) is out of scope for this ADR.
- **Multi-region hosted bus.** The launch managed plan assumes a single hosted region. Multi-region failover for the hosted bus is out of scope; if customer base distributes geographically, this becomes a managed-offering scaling concern.

## References

- ADR 0034 — managed offering model and terminology (the three managed plans; Hosted Services as the launch managed plan; hardware-tier vs managed-plan terminology).
- `speedrun-plan.md` §0.5 (Hosted Services as launch tier), §3.1 (hosted multi-tenant SaaS scaffolding), §3.2 (`apprafter-agent` + registration), §5.6 (this ADR landing point).
- ADR 0033 — tenant security configuration (orthogonal `strictMode` / `confidential` switches).
- ADR 0028 (Platform-stack distribution — CUE source, dual-channel publishing; precedent for codegen discipline).
- ADR 0029 (CUE compilation via Argo CD CMP; precedent for schema source-of-truth pattern).
- [tonic — Rust gRPC implementation](https://github.com/hyperium/tonic).
- [@grpc/grpc-js — official Node gRPC](https://github.com/grpc/grpc-node).
- [ts-proto — TypeScript codegen from protobuf](https://github.com/stephenh/ts-proto).
- [Argo CD `Application` Helm source](https://argo-cd.readthedocs.io/en/stable/user-guide/helm/) — reference pattern for outbound agent design.
