# Platform Specification

> **Codename:** `AppRafter`.
> **Domain:** `apprafter.dev`.
> **Status:** Pre-MVP design document, not yet implemented.
> **Audience:** Architecture decisions, contributors onboarding, design rationale.
> **Revision:** 4 (HTTP-first notifications API, platform-only templates, FSL clarification).

---

## 0. Vision

An **opinionated, vertically-integrated Platform-as-a-Service** built on top of Kubernetes, designed to span **the full lifecycle of a backend product** — from a solo founder running on a single €5 VPS to a regulated enterprise running confidential workloads on bare metal.

The same `Application` manifest works across all tiers. The platform itself scales by replacing internal components, not by changing the developer-facing API.

### What we're solving

The middle ground between **PaaS (Fly.io / Railway / Render)** and **vanilla Kubernetes** is empty in open source.

- PaaS solutions are simple to start with but: vendor-locked, expensive at scale, not self-hostable, with their own non-portable abstractions.
- Vanilla k8s is self-hostable, scales, and is portable — but cognitive load is enormous, ecosystem is fragmented, "batteries" are external and incoherent.
- Existing "k8s distributions" (Cozystack, OpenShift, Rancher) target cloud-provider use cases or enterprise-RH ecosystems, not the **single-tenant product team** spectrum.

### Target users

| Tier | Persona | Hardware | Cost |
|------|---------|----------|------|
| **1. Solo** | Solo founder, side-project | 1× VDS (Hetzner CX22+) | €5–20/mo |
| **2. Small team** | 3–10 engineers, growing product | 3× CCX or small dedicated | €50–200/mo |
| **3. Production** | Established product, mid-size eng team | 3–5× dedicated EPYC | €500–2000/mo |
| **4. Regulated** | Compliance/sovereignty needs | AWS C8i (TDX), confidential bare metal | $2000+/mo |

**Critical property:** dev-facing API is identical across tiers. Migration between tiers is a platform operation, not an application rewrite.

---

## 1. Design Principles

### 1.1 One way to do things

Borrowed from Apple. For each architectural slot, the platform commits to **one** technology and rejects pluggability:

- One CNI: **Cilium**
- One config language: **CUE**
- One control-plane storage: **kine + NATS JetStream**
- One workload runtime by default: **containerd** (Kata for Tier 3+, Kata-CC for Tier 4)
- One package of platform services: Postgres, JetStream, ClickHouse, Redis, S3
- One developer portal: **Backstage**
- One GitOps engine: **Argo CD**

The cost of giving up "choice" is repaid in coherent UX, single error model, and an actually buildable UI.

### 1.2 Vertical integration

The platform owns the full stack from OS to developer portal as a single product. There is no "k8s plus a bunch of Helm charts." The platform manifest declares OS, control plane, services, external surface, and access plane in one place.

### 1.3 Vertical scaling — same API, different substrate

The `Application` manifest does not encode tier-specific assumptions. The operator and ServiceProviders adapt the implementation per tier. A solo founder writes the same CUE that an enterprise SRE writes.

### 1.4 GitOps as the only control surface

Every change to the platform is a Git commit. `kubectl apply` to production is an anti-pattern. Manual operations require explicit override and produce loud audit events.

### 1.5 Decl-first

Everything is a typed declarative resource: workloads, infrastructure, access, external surface, secrets, monitoring. No imperative scripts in the happy path.

### 1.6 No vendor-lock at any layer

Every commercial dependency must have a self-hostable open-source equivalent. The platform must be capable of running on a single Hetzner VPS with no external SaaS dependencies (synthetic monitoring excepted, see §6).

### 1.7 Multi-tenant managed services as first-class

Platform services (Postgres, JetStream, ClickHouse, Redis, S3) run as **shared multi-tenant clusters**, not as per-app instances. Apps declare needs; the platform allocates databases / accounts / buckets within the shared substrate. This mirrors public cloud economics on self-hosted infra.

### 1.8 Enterprise practices must not block solo-tier adoption

For every security/operational practice we adopt at higher tiers (mTLS, signed images, OpenBao, Kata containers, confidential compute), Tier 1 must have a **simpler default that still works**, with a clear migration path. A solo founder with limited DevOps experience must be able to bootstrap on a single €5 VDS in under 30 minutes without surrendering the ability to scale up later.

Concrete examples:
- Tier 1: SealedSecrets with public-key in Git (no OpenBao required)
- Tier 2+: OpenBao with auto-unseal via KMS or Shamir
- Tier 1: `containerd` runtime, no Kata
- Tier 3+: Kata default
- Tier 1: SMTP relay for notifications, no advanced routing
- Tier 2+: full notifications service with multi-channel routing

---

## 2. Architecture Overview

```
┌─────────────────────────────────────────────────────────┐
│  UX Layer                                                │
│  Backstage portal + Headlamp ops + Argo CD UI + k9s     │
├─────────────────────────────────────────────────────────┤
│  Application Layer                                       │
│  Application CRD + ResourceClaim + AccessGrant           │
│  (custom Rust operator on kube-rs)                       │
├─────────────────────────────────────────────────────────┤
│  Platform Services (multi-tenant)                        │
│  Postgres │ JetStream │ ClickHouse │ Redis │ S3          │
│  selected via ServiceProvider + selectors                │
├─────────────────────────────────────────────────────────┤
│  External Surface (declarative, in-scope)                │
│  Git host │ Container registry │ VPN/access │ Synth mon  │
├─────────────────────────────────────────────────────────┤
│  Control Plane                                           │
│  k3s/k8s + kine ──► NATS JetStream (event log + KV)      │
│  Cilium + Gateway API + mTLS by default                  │
├─────────────────────────────────────────────────────────┤
│  Compute Substrate                                       │
│  Tier 1: single VDS (k3s)                                │
│  Tier 2: 3× nodes (k3s/k0s HA)                           │
│  Tier 3: Talos + bare metal EPYC                         │
│  Tier 4: TDX/SEV-SNP nodes for confidential workloads    │
└─────────────────────────────────────────────────────────┘
```

---

## 3. Core Concepts

### 3.1 Application

The unit of deployment. Encapsulates workload, exposure, configuration, dependencies, networking, and per-environment overrides — all in a single CUE document.

```cue
kind: Application
name: parser

// Shared base across environments
base: {
    image: ghcr.io/user/parser
    expose: {port: 8080}
    needs: {
        pg: {size: small}
        jetstream: {streams: ["blocks-head"]}
        redis: {}
    }
    env: {
        DATABASE_URL: from: claim.pg.uri
        API_KEY: from: secret("third-party/tron/key")
        JWT_SECRET: from: secret("self/jwt", rotation: 30d)
        LOG_LEVEL: "info"
    }
    connects: {
        egress: {
            external: [
                {host: "api.tron.network", port: 443}
                {host: "*.binance.com", port: 443}
            ]
        }
    }
    autoscale: {
        on: jetstream_lag
        min: 1
        max: 10
    }
}

// Per-environment overrides — CUE unification, no templating
environments: {
    dev: base & {
        replicas: 1
        expose: {public: false, network: vpn}
        env.LOG_LEVEL: "debug"
        needs.pg.selector: {tier: integrated}
    }
    staging: base & {
        replicas: 2
        expose: {public: false, network: vpn}
        needs.pg.selector: {tier: integrated}
    }
    prod: base & {
        replicas: 3
        expose: {public: false}  // internal only via Gateway
        needs.pg.selector: {tier: managed-aws}
        confidential: true
        network: {
            egressIP: {static: true, pool: "third-party-egress"}
        }
    }
}

budget: {dev: nano, staging: small, prod: medium}
```

**Key properties:**

- **Per-environment overrides via CUE unification.** No template strings, no `{{ .Values.image }}`. Just unification — type-safe, IDE-supported.
- **Each environment** is reconciled into a separate namespace (or vCluster on Tier 2+) with its own ServiceProvider selectors, so `dev` can use integrated PG and `prod` can use AWS RDS — same manifest, different physical reality.
- **Promotion between envs** is a platform operation (`platform-cli promote parser staging prod` or Backstage button), not a manifest rewrite.
- **`needs` automatically generates** corresponding network policies — a dev declares `needs.pg`, the operator emits the egress rule. No duplication.

### 3.2 ServiceProvider

A declared backend for a platform service type. Multiple providers can coexist; applications select by labels.

```cue
kind: ServiceProvider
name: pg-integrated
type: pg
backend: cloudnative-pg
labels: {tier: integrated, location: in-cluster}
config: {
    cluster: "platform-postgres"
    nodes: 3
}

---
kind: ServiceProvider
name: pg-aws
type: pg
backend: aws-rds
labels: {tier: managed, location: aws-eu-west-1, compliance: soc2}
config: {
    region: eu-west-1
    instance_class: db.t4g.medium
}
```

### 3.3 ResourceClaim

Generated by the Application operator when an Application declares a `need`. Routes to a matching ServiceProvider.

```cue
// Generated, not authored
kind: ResourceClaim
name: parser-pg
type: pg
selector: {tier: integrated}  // matched by app's `needs.pg.selector`
spec: {size: small}
status: {
    provider: pg-integrated
    connection: secret-ref://workloads/parser/pg-conn
    ready: true
}
```

### 3.4 AccessGrant

Declarative access for a human or external system. Replaces ad-hoc kubeconfig + VPN-credential distribution.

```cue
kind: AccessGrant
subject: alice@company.com
scope: {
    namespaces: ["dev", "staging"]
    capabilities: ["read", "exec"]
    resources: ["pods", "logs", "deployments"]
}
network: {
    routes: ["10.0.0.0/16"]
    services: ["argocd", "grafana"]
}
mfa: required
expiry: "30d"
```

**End-to-end flow when admin commits an AccessGrant:**

1. Argo CD syncs the manifest. AccessGrant operator reconciles.
2. Operator creates a single-use pre-auth key in Headscale (24h validity).
3. Operator creates a corresponding `RoleBinding` in k8s and OIDC group mapping.
4. The platform's notifications service emails the subject:
  - Tailscale install link
  - One-time magic link (expires in 24h) that finalizes auth
  - Description of granted scope and expiry
5. User clicks magic link → SSO flow with MFA → Tailscale device registers in Headscale → user is now in the mesh.
6. User runs `platform-cli login` to obtain an OIDC-backed kubeconfig (8h validity, auto-refresh).
7. Backstage shows current access status, expiry, ability to request renewal.
8. 5 days before expiry: reminder email. On expiry: auto-revoke (Headscale device removed, RoleBinding deleted, OIDC mapping cleared).

All auth events are written to the audit log (OpenBao audit + JetStream stream).

### 3.5 ExternalSurface

Top-level platform manifest declaring the external contour: git host, registry, monitoring, backups, access plane.

```cue
kind: ExternalSurface
git: {
    provider: gitlab-self-hosted
    url: "git.platform.local"
    backups: {to: "s3://backup-bucket", schedule: daily}
}
registry: {
    provider: harbor
    url: "registry.platform.local"
    retention: "30d"
    signing: required
}
access: {
    provider: headscale
    network: "platform-team"
}
syntheticMonitoring: {
    provider: uptime-kuma
    location: "external-vps"  // out-of-band, separate failure domain
    endpoints: [argocd, grafana, registry, vpn]
}
backups: {
    destination: "s3://platform-backup"  // external, never same failure domain
    retention: "90d"
    encryption: required
}
notifications: {
    providers: ["smtp", "slack"]
    smtp: {host: "smtp.example.com", from: "platform@example.com"}
}
```

**Tier-aware co-location:** on Tier 1, ExternalSurface components (git, registry) can run on the same VDS as the cluster, in separate processes/containers. On Tier 2+, they're separate workloads in the cluster. Synthetic monitoring is **always** out-of-band on a separate small VPS — single-node failure cannot blind us.

**Bidirectional monitoring:** the cluster monitors ExternalSurface (uptime kuma watches git host, registry, etc.); ExternalSurface (via the external watcher VPS) monitors the cluster. Mutual self-healing is **not** in scope for v1.0 — too easy to false-positive into automated damage. Manual recovery procedures are documented as code (`kind: DisasterRecoveryPlan`).

### 3.6 ServiceProviderPlugin

Extension point for community-contributed service providers. Built-in providers (Postgres, JetStream, ClickHouse, Redis, S3) ship with the platform; everything else is a plugin.

```cue
kind: ServiceProviderPlugin
name: mysql-percona
type: mysql  // new resource type, becomes available as needs.mysql
implementation: oci://ghcr.io/community/mysql-percona-provider:v1
config: {
    // plugin-specific
    cluster_size: 3
}
```

**Plugin tiers:**

- **Built-in (Rust):** statically linked into operator. Curated by core team. The five canonical types.
- **Community (gRPC sidecar):** OCI image with gRPC server implementing the `ServiceProviderInterface` proto. Language-agnostic. Registered via `ServiceProviderPlugin`.
- **Future (WASM):** when WASI 1.0 stabilizes. Hot-loadable, sandboxed, no per-plugin process overhead.

**Registry:** community plugins published to a public catalog (separate repo). Each plugin is its own repo, can be MIT/BSD/whatever — no copyleft propagation through gRPC interface.

**Why this matters:** the platform team cannot afford to maintain providers for every database/queue/blob-store the world cares about. Community plugins make the platform extensible without bloat.

### 3.7 Infrastructure

Top-level manifest describing the substrate the platform runs on. Declarative, applied by `platform-cli`.

```cue
kind: Infrastructure
provider: hetzner-cloud
nodes: [
    {role: control-plane, type: cx32, count: 3},
    {role: worker, type: ccx33, count: 5}
]
network: {
    privateNetwork: "platform-net"
    floatingIPs: ["egress-tron-api", "egress-binance"]
}
osImage: talos-1.x
```

`platform-cli plan` shows diff. `platform-cli apply` applies. State is stored in Git (encrypted via age/sops).

**Provider model:**

- **Built-in providers:** Hetzner Cloud, Hetzner Robot (bare metal), AWS — implemented as native Rust SDKs for tight integration.
- **Community providers:** any other cloud (OVH, Scaleway, GCP, Azure, DO, vSphere, Proxmox, etc.) implemented as `InfrastructureProviderPlugin` — a CUE→OpenTofu translator that wraps an existing OpenTofu module.

This way the platform team maintains quality on the two main providers, while the community can add any cloud with an existing OpenTofu provider — which is most clouds.

`platform-cli` invokes OpenTofu under the hood for community providers; the user sees only CUE manifests and `platform-cli plan/apply`. State remains in Git.

### 3.8 MigrationPlan

Auto-generated when a reconciler detects a destructive change (selector change for stateful claims, major version upgrades, storage class changes). Pauses execution until explicit human approval.

```cue
// Auto-generated, not authored
kind: MigrationPlan
name: parser-pg-migration-2026-05-05
application: parser
environment: prod
trigger: {
    type: selector-change
    field: needs.pg.selector
    from: {tier: integrated}
    to: {tier: managed-aws}
}
risks: {
    estimatedDowntime: "5–15 minutes"
    dataVolume: "12 GB"
    reversible: false
    requiresFullBackup: true
}
plan: [
    {step: 1, action: "Snapshot source DB to S3"},
    {step: 2, action: "Provision target RDS instance"},
    {step: 3, action: "Restore snapshot to RDS"},
    {step: 4, action: "Pause writes to source DB"},
    {step: 5, action: "Sync incremental changes (WAL replication)"},
    {step: 6, action: "Switch app to RDS"},
    {step: 7, action: "Verify, then archive source DB"}
]
status: pending-approval
approvers: ["alice@company.com"]
```

**Workflow:**

1. Argo CD syncs a destructive change
2. Reconciler creates a `MigrationPlan` instead of applying immediately
3. Backstage shows a prominent **"Pending migration approval — production at risk"** banner with risk breakdown and step-by-step plan
4. Approver (named in `approvers`) reviews, then **approves / rejects / edits** (e.g., adds maintenance window)
5. On approval: dedicated migration runner executes the plan with progress reporting
6. Every step is logged to the audit stream

**What's destructive (triggers MigrationPlan):**

- Selector change for stateful claims (pg, clickhouse)
- Major version upgrade of a platform service (e.g., pg 15 → 16)
- Storage class change
- Any change marked `destructive: true` in the ServiceProvider schema

**What's NOT destructive (auto-applies):**

- Replica count changes
- Expose rule changes
- Env var additions
- Image updates (those are routine deployments via Argo CD)

This turns "oops, blew up prod" into an **explicit gate** with risk visibility and human control.

---

## 4. Layer Specifications

### 4.1 Compute Substrate

**Tier 1 (single VDS):** k3s in single-node mode with embedded NATS as kine backend. 1–2 GB RAM overhead. Cilium in CNI mode, KubeVirt disabled, Kata disabled.

**Tier 2 (3× nodes):** k3s in HA mode, NATS JetStream cluster as kine backend (3 replicas, embedded or as a workload), Cilium with mTLS, vCluster optional for env separation.

**Tier 3 (bare metal):** Talos Linux on dedicated EPYC, full k8s, Cilium with full Gateway API + Hubble, Kata containers as default runtime, KubeVirt enabled, LINSTOR for replicated block storage.

**Tier 4 (confidential):** Tier 3 + nodes with SEV-SNP or TDX. Kata-CC as `runtimeClass`. Apps opt-in via `confidential: true` flag.

**Open question:** at what tier does kine→NATS stop scaling? See §9.

### 4.2 Control Plane Storage — kine + NATS JetStream

Replace etcd with kine, configured with NATS JetStream backend. Benefits:

- Native event log: every resource mutation is a stream message
- External consumers (UI, audit, CDC) subscribe directly to JetStream
- Cross-region replication via JetStream mirroring
- The same NATS cluster serves apps, achieving operational unification

**Constraints:**

- kine implements a subset of etcd v3 API; verify k8s features work end-to-end (admission webhooks, watch behavior on CRDs)
- Update semantics require correct conditional-write handling in NATS KV (kine v0.10+ handles this, must verify on production CRD churn)
- Performance ceiling vs etcd unclear at very large scale (>1000 nodes, >100k objects)

### 4.3 Networking

- **Cilium** as the only CNI, with kube-proxy replacement
- **Gateway API** as the only ingress/egress mechanism (no Service/Ingress/LoadBalancer split exposed to the dev)
- **mTLS by default** between all workloads via Cilium service mesh or SPIRE-issued certificates
- **NetworkPolicy: default-deny** at namespace creation; the Application's `connects` declares allowed flows
- **IPv6 primary**, IPv4 optional

**Connectivity model:**

- Egress to platform services (PG, JetStream, etc.) is **automatically derived** from `needs`. No duplication.
- Egress to external endpoints is declared explicitly via FQDN or CIDR (Cilium FQDN policies).
- Ingress is identity-based: only declared callers (by SPIFFE identity or Application name) can reach a service.
- The full connectivity graph is **observable via Hubble** in Backstage — devs see actual flows, can convert observed traffic into explicit policy with one click ("you tried to call `api.x.y` — add to whitelist?").

**Static egress IPs** for third-party integrations:

```cue
network: {
    egressIP: {
        static: true
        pool: "third-party-egress"
        purpose: "tron-api-integration"
    }
}
```

Implementation per tier:
- **Tier 1:** node IP (already fixed)
- **Tier 2:** Cilium Egress Gateway with floating IP attached to egress nodes
- **Tier 3:** dedicated VLAN with fixed IP block
- **Tier 4 (AWS):** VPC NAT Gateway with Elastic IP

Backstage shows current egress IP per Application with copy-button — easy to whitelist on the third-party side.

### 4.4 Secrets

The platform supports two secret backends, chosen by tier:

**Tier 1 (single VDS, no KMS available): SealedSecrets**

- [Bitnami SealedSecrets](https://github.com/bitnami-labs/sealed-secrets)
- Public encryption key in Git (anyone can encrypt); private key on the cluster (only the controller can decrypt)
- Devs commit `kind: SealedSecret` manifests; controller decrypts and creates k8s Secrets at runtime
- No daemon to operate, no unsealing problem
- **Limitation:** no dynamic secrets, no auto-rotation, no fine-grained ACL — but appropriate for solo-tier
- UI prominently warns: "you're using SealedSecrets — full secret management requires upgrade to Tier 2+ (OpenBao)"

**Tier 2+ or Tier 1 with KMS configured: OpenBao**

- [OpenBao](https://openbao.org/) — open-source MPL-2.0 fork of Vault, Linux Foundation, production-ready as of 2.5.0 (Feb 2026)
- API-compatible with Vault, no commercial restrictions, no enterprise-feature paywall
- 3-node HA cluster as a platform service
- Auto-unsealing: KMS on cloud-tier (AWS KMS / GCP KMS), Shamir share on bare-metal/dedicated
- Workload identity via SPIFFE/SPIRE — pods authenticate to OpenBao with their X.509 identity, receive short-lived secrets
- Dynamic secrets (DB credentials per pod, auto-revoked on pod termination)
- Full audit log fed into the platform event stream (JetStream)

**Common API in Application:**

```cue
env: {
    DATABASE_URL: from: claim.pg.uri          // from ResourceClaim
    API_KEY: from: secret("third-party/x/y")   // from SealedSecrets or OpenBao
    JWT: from: secret("self/jwt", rotation: 30d)  // auto-rotated on OpenBao
    LOG_LEVEL: "info"                          // plain literal
}
```

The operator generates the right injection mechanism based on tier (envFrom for SealedSecrets, Vault Agent / Secrets Store CSI Driver for OpenBao). Devs don't see the difference — they declare needs.

**Migration Tier 1 → Tier 2:** `platform-cli upgrade-tier` includes a step to import existing SealedSecrets into OpenBao and rewrite Application manifests. One-time, non-destructive.

### 4.5 Application Operator

Custom operator written in **Rust** on **kube-rs**. Single reconcile loop, no Crossplane composition layer.

**Responsibilities:**

1. Validate `Application` manifest against CUE schema (admission webhook)
2. Resolve per-environment overrides via CUE unification
3. Resolve `needs` → create `ResourceClaim`s with appropriate selectors
4. Wait for ResourceClaims to be `ready`, collect connection refs
5. Render Deployment + Service + Gateway Route + ScaledObject (KEDA) + NetworkPolicy + EgressIP allocations from the Application
6. Inject credentials via workload identity (SPIFFE), not mounted Secrets
7. Configure secret injection from OpenBao (Vault Agent / CSI driver)
8. Update `Application.status` per environment with traffic, replicas, autoscale state, recent events, current egress IP

**Why custom over Crossplane:** see design rationale in §8.

### 4.6 Platform Services

Six canonical types, each with at least an `integrated` ServiceProvider that runs in-cluster. Anything beyond this list is a community ServiceProviderPlugin.

| Type | Backend (integrated) | Multi-tenancy mechanism |
|------|---------------------|-------------------------|
| `pg` | CloudNativePG | Database + role per claim |
| `jetstream` | NATS cluster | Account per claim, streams scoped |
| `clickhouse` | clickhouse-operator (Altinity) | Database per claim, RBAC |
| `redis` | Dragonfly or KeyDB | DB-namespace per claim |
| `s3` | MinIO or Garage | Bucket + IAM user per claim |
| `notifications` | NATS-backed queue + provider workers | Subjects per claim, isolated DLQs |

#### Notifications — detailed design

Apps send notifications via an HTTP API; the platform delivers via configured channels with retry, DLQ, and audit. NATS JetStream is used internally for the queue/persistence layer but is not exposed to apps directly.

**Application declaration:**

```cue
needs: {
    notifications: {
        channels: ["email", "slack", "telegram"]  // outbound channels this app uses
        inbox: {
            size: 10000        // max queued
            retention: "7d"    // unprocessed messages drop after
            dlq: {
                afterRetries: 5
                alertVia: "telegram"   // escalation channel for DLQ events
                alertTo: "@owner"      // app owner's handle
            }
        }
    }
}
```

**App-side API (HTTP-first):**

```typescript
// Universal HTTP POST — any language with an HTTP client works
await fetch('https://notifications.platform.local/send', {
    method: 'POST',
    headers: {
        'Authorization': `Bearer ${platformToken}`,  // injected via workload identity
        'Content-Type': 'application/json'
    },
    body: JSON.stringify({
        to: 'user@example.com',
        channel: 'email',
        template: 'welcome',  // app-defined template
        data: { name: 'Alice' }
    })
})
```

A thin SDK wrapper is provided for convenience, but apps can integrate via raw HTTP without any platform-specific library — important for solo founders who shouldn't have to learn NATS clients to send an email.

**Architecture under the hood:**

- HTTP request → notifications service authenticates via workload identity (JWT with SPIFFE claims)
- Message is published to NATS JetStream stream `notifications.<app-account>.outbox`
- Channel workers (email/slack/telegram) consume from the stream, deliver, retry with exponential backoff
- After N retries: DLQ stream `notifications.<app-account>.dlq` + escalation alert through configured channel
- All events flow into the platform audit log

**Channels as plugin extension point:**

- Built-in: SMTP (email), Slack API, Telegram Bot API
- Community plugins: Discord, Mattermost, custom webhooks, SMS providers, etc.

**Platform-shipped templates (intentionally limited scope):**

The platform ships only templates needed by the platform itself, not a content library. Templates live in the platform Git repo under `templates/` and can be overridden by deployment.

Built-in templates:
- **AccessGrant lifecycle:** issue (with magic link), renewal reminder (5 days before expiry), expiry, revocation
- **Operational alerts:** DLQ stuck, service down, quota exceeded, MigrationPlan pending approval, backup digest
- **Bootstrap:** cluster initialized

What the platform does **not** ship:
- Welcome emails for apps
- Password resets
- Marketing / newsletter content
- Generic template marketplace

Apps that want welcome emails write their own templates, store them in their own Git repo, and send via the same HTTP API. The platform provides **transport and channels**, not a content library.

**Backstage view:**

- Inbox state per app: pending / sent / failed / DLQ
- DLQ viewer with retry / drop actions
- Alerts: "5 messages stuck in DLQ for parser/prod, last error: SMTP 550"
- Per-channel success-rate dashboards

**Lazy provisioning:**

```cue
platformServices: {
    jetstream: {required: true, minNodes: 3}    // always up (kine + event log + notifications transport)
    clickhouse: {required: true, minNodes: 1}   // always up (platform logs)
    s3: {required: true, minNodes: 1}           // always up (backups)
    openbao: {required: false, minTier: 2}      // Tier 1 uses SealedSecrets
    pg: {required: false, scaleOnDemand: true}  // first claim creates cluster
    redis: {required: false, scaleOnDemand: true}
    notifications: {required: true, minNodes: 1}  // always up (used by AccessGrant, alerts)
}
```

**Tier-aware defaults:** the platform manifest declares per-tier defaults for `size`, `replicas`, retention, etc., so that `needs.pg: {}` produces appropriate sizing without dev intervention. On Tier 1, `pg.size: nano`; on Tier 3, `pg.size: small`.

### 4.7 External Surface

In-scope as first-class platform components, not "set up separately."

- **Git host:** GitLab self-hosted or Forgejo. Container registry can be inside (GitLab Registry / Harbor) or external.
- **CI runners:** GitLab Runner or Woodpecker, deployed as workloads on the same platform.
- **Synthetic monitoring:** Uptime Kuma on a separate small VPS (the only required external dependency) or external SaaS free-tier. Watches platform endpoints from outside.
- **Backup destination:** external S3-compatible (Hetzner Storage Box, Cloudflare R2). Required external — never store backups in the same failure domain.
- **Bootstrap CLI:** `platform-cli init --provider hetzner --tier solo` provisions a fresh cluster from zero. Same CLI handles tier upgrades.

### 4.8 Access Plane

Headscale (self-hosted Tailscale control plane) + Tailscale Operator for k8s. Identity-aware mesh.

- AccessGrant CRD as the only user-facing surface
- Auto-revocation on expiry
- MFA enforced via SSO provider (OIDC)
- Workload identity (SPIFFE) bridges to user identity for end-to-end auditing
- Notifications via the platform notifications service

### 4.9 Build Pipeline

Devs write Dockerfiles. The platform doesn't try to hide them — it provides **transparency and analysis**.

- **Default builders:** BuildKit / Buildah / Kaniko (auto-detected from `Dockerfile`)
- **Auto-checks at build time:**
  - Trivy / Grype (CVE scan)
  - SBOM generation (CycloneDX)
  - Cosign signing (mandatory for prod environments)
  - Image size + layer analysis
  - Cache hit reporting
- **Backstage build report:**
  - Image size + suggestions ("considered multi-stage build?")
  - CVE list (HIGH / MEDIUM / LOW)
  - Embedded secrets detection
  - Build duration, cache efficiency
  - Recommendations (auto-fix where possible)
- **Buildpacks** as opt-in for those who don't want Dockerfiles, but not the default. Devs should understand what's in their image.

### 4.10 Observability

Single OTLP pipeline: every workload exports metrics, logs, and traces via OpenTelemetry.

- **Metrics:** VictoriaMetrics
- **Logs + traces:** ClickHouse (the same multi-tenant cluster used by apps — single substrate, lower overhead)
- **Network observability:** Hubble (Cilium-native, eBPF)
- **Dashboards:** Grafana, with curated dashboards per Application kind shipped by the platform

### 4.11 UX Layer

- **Backstage** as the developer portal. Custom plugins:
  - `Application` view: status per environment, traffic, autoscale state, deploy history, egress IPs (with copy buttons)
  - `ResourceClaim` view: which DB / queue / bucket allocated
  - `AccessGrant` self-service flow
  - `Build report` view: CVE/SBOM/size analysis per image
  - Cost view: per-Application breakdown of platform-service usage
  - Hubble flow visualizer: see actual network flows, convert to explicit policy
  - Software Templates: golden paths for common stacks (NestJS+pg+jetstream, Bun+Hono+ClickHouse, etc.)
- **Headlamp** in-cluster for ops engineers (deeper k8s view)
- **Argo CD UI** for GitOps state
- **k9s** for platform engineers debugging the substrate

### 4.12 Infrastructure Tooling (`platform-cli`)

Single Rust binary that manages the substrate (everything below the cluster). Inspired by Vercel/Heroku UX — solo founders should not have a worse experience than they'd get on a closed-source PaaS.

**Workflow:**

```bash
# Initial provisioning
platform-cli init --provider hetzner-cloud --tier solo --region nbg1

# Apply changes from Git
platform-cli apply

# Show diff before applying
platform-cli plan

# Tier upgrade (e.g., solo → team)
platform-cli upgrade-tier --to team

# User-side login after AccessGrant
platform-cli login
```

**Provider implementation strategy:**

- **Built-in providers** (Hetzner Cloud, Hetzner Robot, AWS) — implemented via native Rust SDKs (`hcloud-rust`, `aws-sdk-rust`). Tight integration, no external binary.
- **Community providers** (OVH, Scaleway, GCP, Azure, DO, vSphere, Proxmox, etc.) — implemented as `InfrastructureProviderPlugin`. The plugin contains:
  - A CUE-to-OpenTofu translator
  - A wrapped OpenTofu module for that provider
  - A state-importer that reads `tofu state` back into the platform's CUE state

`platform-cli` invokes OpenTofu under the hood for community providers; the user sees only CUE manifests and `platform-cli plan/apply`. State remains in Git (encrypted via age/sops). OpenTofu is OSS Linux Foundation MPL-2.0 — aligns with our license stance.

**Why this hybrid:**

- Native SDK gives the best UX where it matters most (the two providers we ship)
- OpenTofu shim leverages the existing terraform-provider ecosystem for everything else (most clouds already have providers, no need to rewrite)
- Pluggable: anyone can ship an `InfrastructureProviderPlugin` for their cloud without touching core platform code

**No Ansible:** Talos is immutable, all node config goes through Talos API. Mutable Linux is not a target substrate.

**Bare metal flow:** for Tier 3+, `platform-cli` orchestrates Talos `talos-bootstrap` (PXE/ISO) + `talm` for manifest generation. Same CLI, abstracted under tier-specific subcommands.

---

## 5. Tech Stack Decisions

| Layer | Choice | Rationale |
|-------|--------|-----------|
| Operator language | **Rust** + kube-rs | Performance, idiomatic for control-plane work, low memory footprint. AI-assisted development closes the productivity gap. |
| Tooling / CLI | **Rust** (or OneBun for non-hot-path) | `platform-cli` in Rust; UI-facing services and Backstage plugins in TS/OneBun. |
| Backstage plugins | **TypeScript** (Backstage native) | No choice; Backstage is React/TS. |
| Config language | **CUE** | Typed, generic, validates schemas, importable. Pkl considered as alternative; deferred. |
| GitOps | **Argo CD** | Mature, open source, the only k8s tool with product-grade UI. |
| CI | **GitLab CI** or **Woodpecker** | Whichever pairs with Git host choice. |
| Container runtime | **containerd** default; **Kata** Tier 3+; **Kata-CC** Tier 4 | Standard for Tier 1–2; isolation for higher tiers. |
| Storage | **Local-path** Tier 1; **LINSTOR** Tier 3+ | Match complexity to tier. |
| OS | **Talos** Tier 3+; standard Linux Tier 1–2 | Talos is overkill for single-VDS; reasonable for bare metal. |

---

## 6. Roadmap

### Milestone M0 — Architecture (current)

- [x] Core principles documented
- [x] Tier model defined
- [x] Initial CUE schemas drafted
- [x] Codename chosen
- [x] Repository structure defined
- [x] License chosen (FSL-1.1-MIT for core, MIT for plugins; see ADR 0001)

### Milestone M1 — MVP single-node ✅

**Target:** working Tier 1 deployment on a single Hetzner CX22, deploying a hello-world Application end-to-end.

- [x] `platform-cli init` provisions k3s + NATS + Cilium + Argo CD + Backstage on fresh VDS (NATS deferred to M2; everything else lands in v0.1.2 → v0.1.20)
- [x] Application CRD + Rust operator (basic: image, expose, no `needs` yet) (v0.1.21 → v0.1.32)
- [x] Argo CD configured, GitOps loop working (v0.1.13 → v0.1.17)
- [x] Backstage with minimal Application plugin (status view) (v0.1.18 → v0.1.20 + v0.1.33 → v0.1.36)
- [x] One golden-path template (Bun HTTP service) (v0.1.37 → v0.1.38, OneBun-based)

### Milestone M2 — Platform Services

**Target:** `needs.pg`, `needs.jetstream`, `needs.redis` working with integrated providers.

- [ ] ServiceProvider CRD
- [ ] ResourceClaim CRD
- [ ] CloudNativePG integration (pg-integrated provider)
- [ ] NATS JetStream provider (account/stream allocation)
- [ ] Dragonfly provider (Redis)
- [ ] Workload-identity-based credential injection (SPIRE setup)
- [ ] Backstage plugin for ResourceClaim view

### Milestone M3 — Multi-node + observability

**Target:** Tier 2 deployment (3 nodes), full observability stack.

- [ ] HA mode bootstrap in `platform-cli`
- [ ] kine + NATS JetStream as control plane storage (replacing default etcd in k3s)
- [ ] OpenTelemetry pipeline default for all workloads
- [ ] ClickHouse provider (logs, traces, app analytics)
- [ ] VictoriaMetrics integration
- [ ] Hubble enabled, dashboards in Grafana

### Milestone M4 — External Surface + Access

**Target:** declarative external contour, AccessGrant working.

- [ ] ExternalSurface CRD
- [ ] GitLab/Forgejo deployable from manifest
- [ ] Harbor registry deployable from manifest
- [ ] Headscale + Tailscale Operator integration
- [ ] AccessGrant CRD + reconciler
- [ ] Backstage self-service flow for AccessGrant

### Milestone M5 — Tier 3 (bare metal)

- [ ] Talos installation flow in `platform-cli`
- [ ] LINSTOR provisioning automation
- [ ] Kata containers as default runtime
- [ ] vCluster for tenant separation
- [ ] Migration path Tier 2 → Tier 3 (data + workloads)

### Milestone M6 — Tier 4 (confidential)

- [ ] Kata-CC runtimeClass + nodepool selectors
- [ ] AWS C8i / M7a integration in `platform-cli`
- [ ] Confidential service providers (where applicable)
- [ ] Attestation flow integrated with workload identity

### Milestone M7 — 1.0

- [ ] Stable CUE schema (semver guarantee)
- [ ] Complete docs site (TechDocs)
- [ ] Reference deployments published
- [ ] Public bootstrap-from-zero benchmark (target: <15 min from `platform-cli init` to first Application live)

---

## 7. Open Questions and Decisions

### Resolved

- **etcd fallback at scale:** **Not planned.** Commit to kine + NATS JetStream. Verify scale ceiling during real production use; address by sharding/optimization rather than reverting.
- **CUE vs Pkl:** **CUE for now.** More mature ecosystem, broader k8s tooling. Reassess in M5+ if Pkl ecosystem catches up.
- **Bootstrap secret distribution:** **Shamir share** for OpenBao (Tier 2+). Explore zero-knowledge-proof approaches in later iterations.
- **Tier 1 secrets:** **SealedSecrets by default**, with prominent UI prompt to migrate to OpenBao at Tier 2+. Don't force solo-tier through full Vault complexity.
- **Notifications service:** **In scope** as a platform service. **HTTP-first API** for apps (NATS JetStream as internal queue/persistence layer, not exposed). Built-in channels: SMTP/Slack/Telegram; others via plugins.
- **Notification templates:** **Platform-only.** Ship templates for AccessGrant lifecycle, operational alerts, and bootstrap. Do not become a SendGrid-style content marketplace. Apps that need their own templates author and version them in their own Git repos.
- **`type:` field in `needs`:** **Removed.** Field name is the type (`needs.pg`, `needs.redis`).
- **Selectors on `needs`:** **Yes**, symmetric with k8s nodeSelector. Default to `tier: integrated`.
- **Build pipeline approach:** **Dockerfile-first with auto-analysis**, not magic. Buildpacks as opt-in.
- **Infrastructure tooling — built-in vs community:** **Hetzner + AWS** as built-in (native Rust SDKs); **everything else** via `InfrastructureProviderPlugin` using OpenTofu shim under the hood. No raw Terraform/Ansible exposed to users.
- **Migration safety:** **MigrationPlan** as a first-class concept. Destructive changes pause for explicit approval, with risk breakdown shown in Backstage.

### License decision

- **Core platform:** **FSL-1.1-MIT** (Functional Source License → MIT after 2 years), modeled on Sentry's approach.
  - Each release carries a 2-year window of commercial-use protection (cloud-vendor SaaS rebranding restricted)
  - 2 years after a given release date, that release auto-converts to MIT (fully OSI-approved)
  - Active development always lives in the FSL window; older releases are continuously freed
  - Bonus property: even if the project is abandoned, the most recent release is always at most 2 years from becoming MIT — a built-in safety net for the community
- **InfrastructureProviderPlugin / ServiceProviderPlugin SDKs and community plugins:** **MIT** from day one — minimal friction for community contribution
- **Business model:** managed offering (à la Akuity → Argo CD), not BSL/non-compete clauses

### Still open

1. **kine + NATS scaling ceiling.** What's the realistic upper bound? At what scale (nodes / objects / CRD churn rate) does sharding become necessary? Empirical question, answer in production.

2. **CUE vs Pkl re-evaluation point.** When and how to formally re-decide. Suggested: M5 (Tier 3 milestone), with a written ADR comparing.

3. **Multi-tenancy isolation choice.** vCluster vs Kamaji vs Hierarchical Namespace Controller for Tier 2+ tenant separation. Each has different cost/isolation trade-offs.

4. **Migration tooling depth.** v1.0: manual MigrationPlan with operator-authored steps. v2.x: ship platform-provided automated migration plans for common cases (PG `tier: integrated → managed-aws`, ClickHouse major upgrade, etc.). Gradual increase in automation.

5. **Cost attribution model.** v1.0: **rough percentages** of cluster CPU/RAM/disk/network per Application. Refine over time toward query-count-based for DBs, byte-published for queues, etc.

6. **Backstage vs custom portal long-term.** Stay on Backstage for v1.0 (faster MVP). If we hit limits, evaluate custom portal on **OneBun + Svelte/React + Tauri** (existing experience). Bridge: keep portal API surface clean so backend can be swapped without losing custom plugins.

7. **WASM plugin readiness.** When does WASI 1.0 + threading + async I/O reach production-grade for our use case? Until then, gRPC sidecar plugins are the standard. ADR when transitioning.

8. **Bidirectional self-healing of External Surface.** v1.0: **monitoring only**. Self-healing is a separate hard problem (false-positives can cause cascading damage). Address in M5+ with explicit DR-as-code procedures.

9. **Codename.** Final naming, affects domain, repo, brand from day one. Candidates: `Cumulus`, `Bedrock`, `Helix`, `Substrate`, `Atlas`. Decision criterion: short, memorable, available domain (.io / .dev), no existing major OSS conflict. Chosen: `AppRafter`

10. **OneBun integration depth.** UI services (Backstage backends, build-report renderer, notifications service) are clear OneBun targets. `platform-cli` itself is Rust (decided). UI layer beyond Backstage — TBD.

11. **Per-environment substrate (federated multi-cluster).** Should `dev` be able to live on a Tier 1 substrate while `prod` lives on Tier 3 — under one Backstage view? Compelling but requires multi-cluster control plane design. **Deferred to v2.x roadmap.**

---

## 8. Design Rationale (for tricky decisions)

### Why custom Rust operator over Crossplane

- **Cognitive load:** Crossplane introduces XR, XRD, Composition, Functions — five abstractions before reaching Kubernetes resources. A Rust operator is just a function returning resources.
- **Performance:** Crossplane runs multiple reconcile loops per claim; custom operator has one. At scale, latency and memory matter.
- **Evolution:** Schema changes in Rust go through `serde` + conversion webhooks (standard k8s upgrade path). In Crossplane, Composition versioning is harder.
- **Debuggability:** Custom operator is debuggable as ordinary Rust code (breakpoints, unit tests, profiling). Crossplane is a black box you instrument from outside.
- **Crossplane's strength** — heterogeneous cloud-resource management — isn't our problem. We have one platform model, not a catalog.

### Why kine + NATS JetStream over etcd

- **Operational unification:** the same NATS that backs kine also backs platform services and apps. One technology to operate.
- **Native event log:** every resource change is a stream message — audit, CDC, time-travel for free.
- **External consumer compatibility:** Backstage plugins, monitoring, and CI tools subscribe to NATS streams directly without polling the API server.
- **Cross-region:** JetStream mirroring gives DR semantics natively.
- **Limitation:** k8s API features that depend on subtle etcd semantics need verification. Tier 4 / very large scale may revert to etcd if needed.

### Why no `type:` field in `needs`

- The set of platform services is finite (six) and opinionated. Adding `type: postgres` is redundant when the field name is `pg`.
- Prevents the temptation to add MySQL, MongoDB, etc. at core — they're plugins, not built-ins.
- Self-documenting: glancing at a manifest tells you the dependency stack.

### Why selectors on `needs`

- Symmetric with k8s nodeSelectors — devs already know the pattern.
- Enables hybrid setups (some apps on integrated PG, others on AWS RDS).
- Enables migration without app rewrites — change selector, operator handles the rest (with manual data-migration wizard for v1.0).
- Default-`integrated` keeps simple cases simple.

### Why per-environment overrides via CUE unification, not separate manifests

- Single source of truth per Application. Easier to read, easier to audit.
- CUE unification is type-safe and composable, unlike Helm's text templating.
- Env diffs are explicit and minimal — base + delta, not three full manifests.
- Promotion between envs is a platform operation (image promote), not a manifest copy.

### Why OpenBao instead of Vault

- HashiCorp's BSL relicensing in 2023 made Vault non-OSI-open-source. OpenBao is the MPL-2.0 community fork (forked from Vault 1.14.0).
- As of 2026 OpenBao is production-ready (v2.5.0+, Linux Foundation governance, IBM contributors).
- API-compatible with Vault — workload code that uses Vault SDK works unchanged.
- No commercial restrictions, no enterprise-feature paywall.
- Aligns with our "no vendor-lock at any layer" principle.

### Why SealedSecrets at Tier 1, not OpenBao

- Solo-tier on a single €5 VDS does not have a KMS to auto-unseal OpenBao
- Manual Shamir unseal at every restart is unacceptable UX for a solo founder
- OpenBao's footprint (3-node HA + Raft) is overkill for a single-app environment
- SealedSecrets gives 80% of the value (encrypted secrets in Git) at 5% of the operational cost
- Migration path is clean: `platform-cli upgrade-tier` imports SealedSecrets into OpenBao on Tier 2+
- This is our **principle 1.8 in action**: enterprise practices must not block solo-tier adoption

### Why hybrid native-SDK + OpenTofu-shim for infrastructure providers

- Native SDKs give the best UX and integration depth where it matters most (Hetzner + AWS, our two main targets)
- OpenTofu has providers for **every cloud and on-prem virtualization platform that matters**; recreating that ecosystem ourselves would burn years of engineering with no incremental value
- Wrapping OpenTofu under our CUE manifests gives community contributors a low bar to add their cloud — write a `InfrastructureProviderPlugin`, ship the platform-CUE→Tofu translator, done
- Users see only CUE and `platform-cli plan/apply`; OpenTofu is an implementation detail
- OpenTofu is MPL-2.0, Linux Foundation, OSS-friendly — no vendor-lock concerns

### Why MigrationPlan as a first-class concept

- The most common cause of production outages in declarative-GitOps systems is "I changed one line and the operator did something I didn't expect"
- Selector changes for stateful services (PG, ClickHouse) are inherently destructive — they require data migration, downtime, and explicit risk acceptance
- Auto-applying these silently is **worse** than no automation at all — it creates a false sense of safety
- MigrationPlan turns the implicit "this might blow up" into an explicit "this **will** require approval, here's the risk breakdown" — same model that Terraform `plan/apply` proved successful
- Approval is human-in-the-loop **by design**, not by accident

### Why FSL-1.1-MIT for the core (Sentry's model)

- BSL has non-compete clauses that scare some enterprise users and are not OSI-approved
- Pure Apache-2.0 lets cloud vendors paraderaiderbrand the platform as their managed offering with zero contribution back (the Cozystack-style risk we've been trying to avoid)
- AGPL on portal + MPL on core split is workable but creates licensing complexity and split governance
- FSL-1.1-MIT (Functional Source License) provides a clean middle ground:
  - 2-year commercial use restriction (cloud vendors cannot offer it as their primary managed product)
  - Auto-converts to MIT after 2 years (every release becomes fully OSI open)
  - Used in production by Sentry (since 2023), proven model
  - Single license to communicate, easier than tier-split licensing
- The 2-year window is enough for us to establish a managed offering as the canonical commercial deployment, after which the protection naturally relaxes
- **Built-in safety net:** even if the project is abandoned, the most recent release is at most 2 years away from being MIT — community can always continue without us
- Plugins under MIT from day one keeps the contribution bar minimal

### Why HTTP API for notifications (not direct NATS exposure)

- HTTP is the universal lowest common denominator — every language has a built-in HTTP client; no SDK lock-in, no NATS-client dependency in user code
- A solo founder writing a small Bun service shouldn't have to learn NATS protocols just to send an email
- HTTP is debuggable with standard tools (curl, browser DevTools, Postman); NATS requires specialist tooling
- Internal queue/persistence is still NATS JetStream — this is an implementation detail, not user-visible
- This decision is principle 1.8 in action: don't force enterprise-grade primitives onto solo-tier UX
- We retain the option to expose direct NATS access for power users in a future version if real demand emerges

### Why platform-only notification templates

- Becoming a content/template marketplace would dilute focus and balloon scope (we'd compete with SendGrid, Mailgun, ConvertKit, etc. — losing battle)
- The platform's job is to be a transport with rich audit, not a marketing tool
- Apps already have their own Git repos for their own templates — that's the right place for app-specific content
- Built-in templates exist only for things the platform itself sends (AccessGrant lifecycle, alerts, MigrationPlan notifications, bootstrap)
- Override mechanism is simple: edit the template file in the platform's Git repo or supply a configmap override; no special template-management UI needed

### Why three-tier plugin model (built-in / gRPC sidecar / future WASM)

- **Built-in (Rust)** for the canonical six — performance, stability, statically linked, no per-plugin overhead.
- **gRPC sidecar plugins** for community contributions — language-agnostic, works today, OCI-distributed.
- **WASM plugins (future)** — once WASI 1.0 stabilizes (threading, async I/O, networking), migrate sidecar plugins to WASM for sandbox + zero-process-overhead.
- Three tiers because no single mechanism balances *performance + safety + extensibility* today.

### Why Dockerfile-first build pipeline (not Buildpacks-default)

- Most backend devs already know Dockerfile + dockerignore.
- Magic build pipelines (Heroku-style buildpacks) are great when they work, painful when they don't — and the failure modes are opaque.
- Better strategy: keep Dockerfile, add **transparency tools** (CVE scan, SBOM, layer analysis, recommendations).
- The platform value-add is *audit and feedback*, not hiding what's in the image.
- Buildpacks remain available for those who want them — opt-in.

### Why infrastructure tooling in Rust + CUE, not pure Terraform/Ansible

- One configuration language for the user (CUE everywhere) reduces cognitive load
- For our two main providers (Hetzner, AWS), native Rust SDKs give the tightest integration
- For everything else, OpenTofu is the obvious choice — its provider ecosystem is unmatched, and it's MPL-2.0 OSS
- Talos OS makes Ansible obsolete for the substrate (immutable, API-driven)
- State in Git (encrypted via age/sops) avoids Terraform's "where does the state live" headache
- Users never write or read Terraform/Tofu directly; it's strictly an implementation detail of community providers

---

## 9. Glossary

- **Tier:** the deployment scale of the platform (1–4), affecting which substrate / providers are active.
- **Application:** the dev-facing unit of deployment, with per-environment overrides.
- **Environment:** a named target deployment (`dev`, `staging`, `prod`) with its own namespace/vCluster, ServiceProvider selectors, and exposure rules.
- **ServiceProvider:** a backend implementation for a platform-service type (e.g., `pg-integrated`, `pg-aws`).
- **ServiceProviderPlugin:** an OCI-distributed gRPC plugin extending the platform with new service types or backends.
- **InfrastructureProviderPlugin:** a plugin extending `platform-cli` with support for an additional cloud or substrate, typically by wrapping an OpenTofu module.
- **ResourceClaim:** an internal CRD generated when an Application declares a `need`; routes to a matching ServiceProvider.
- **AccessGrant:** declarative access for humans/external systems, replacing manual kubeconfig + VPN distribution.
- **MigrationPlan:** auto-generated plan describing a destructive change (data migration, version upgrade) that requires explicit human approval before execution.
- **ExternalSurface:** the platform-managed external contour (git, registry, monitoring, backups, access).
- **Infrastructure:** the substrate manifest (nodes, network, OS image) managed by `platform-cli`.
- **Platform Service:** one of the six canonical multi-tenant services (Postgres, JetStream, ClickHouse, Redis, S3, Notifications).
- **Integrated provider:** a ServiceProvider that runs as a workload inside the platform itself.
- **Managed provider:** a ServiceProvider that delegates to an external managed service (e.g., AWS RDS).
- **Substrate:** the compute layer (VDS / VM / bare metal) the platform runs on.
- **Workload Identity:** a SPIFFE-issued X.509 identity that pods use to authenticate to OpenBao, ServiceProviders, and other workloads.
- **Static Egress IP:** a fixed IP address assigned to an Application's outbound traffic, for third-party whitelisting.
- **SealedSecrets:** Bitnami's mechanism for encrypted secrets in Git, used as the Tier 1 default before OpenBao is required.

---

## Appendix A — Repository Structure (proposed)

```
platform/
├── cli/                    # platform-cli (Rust)
├── operator/               # Application/ResourceClaim/AccessGrant operator (Rust, kube-rs)
├── schemas/                # CUE schemas for all CRDs
├── providers/              # ServiceProvider implementations
│   ├── pg-integrated/
│   ├── pg-aws/
│   ├── jetstream-integrated/
│   ├── clickhouse-integrated/
│   ├── redis-integrated/
│   └── s3-integrated/
├── backstage-plugins/      # TS plugins for Backstage
├── manifests/              # base platform manifests (per tier)
├── docs/                   # TechDocs source
└── examples/               # reference Applications
```

---

## Appendix B — Non-goals

To prevent scope creep, the platform **explicitly does not** aim to:

- Be a general-purpose k8s distribution (Cozystack / Rancher's domain).
- Support arbitrary databases as platform services. Five, not fifty.
- Compete with Backstage as a portal; we extend it.
- Replace `kubectl` / `helm` / `k9s` for platform engineers — they remain the operations interface.
- Run on Windows nodes.
- Provide Function-as-a-Service / WASM as primary primitive (may be added later as opt-in).
