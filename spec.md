# Platform Specification

> **Codename:** `AppRafter`.
> **Domain:** `apprafter.dev`.
> **Status:** Phase 1 (Tier 1 single-node MVP) delivered as `v0.1.0-mvp` on 2026-05-08; Phases 2–8 in active design. See `plan.md` for the phase ledger.
> **Audience:** Architecture decisions, contributors onboarding, design rationale.
> **Revision:** 13 (App-migration security axis — 2.16b-sec, ADR 0052: the application-scope destructive classifier gains a **security axis** — a new `security-boundary` class ranked above `data-migration` (a data-loss change is coverable by backups; a leaked credential is not), gating **additive/escalation** edits an attacker with manifest write access would make: env `secret:` reference add / downgrade-to-literal / retarget, `expose.network` escalation to `public`, public-hostname add, public-port retarget, and `imagePolicy.resolve` relaxation off→digest; `image-path-change` is reclassified to `security-boundary`. §3.8 actualizes the destructive taxonomy with this security axis plus the plan rollup (`spec.risks.classifications[]` + `spec.changes[]` drill-in, so a dangerous op can't be laundered behind a benign primary), the three structural hardening defenses (approval bound to `spec.trigger.approvedSpecHash`; `Application.status` write-protected to the operator's SSA field manager; `spec.environment` immutable on UPDATE), the S-2 authority model (write authority ≠ approve authority — the k8s status-subresource patch is authoritative, the Argo button is convenience; the SSA field manager is an ownership label whose integrity rests on RBAC scoping), and the §6 deliberately-ungated set (image-tag change, replicas-up/quota, registry/CI compromise, in-cluster kubectl/RBAC, cross-tenancy — the MigrationPlan protects the git→cluster path only). Rev 12 (App-scope destructive-change gating — 2.16b, ADR 0051: the application-scope `detect_destructive` classifier is enabled and wired into the Application reconcile loop, so a destructive edit to a user `Application` auto-creates an application-scope `MigrationPlan` in the app's own namespace, owned by the `Application` CR, and pauses the app at `AwaitingMigrationApproval` until approved via `apprafter migration approve` or the Argo CD "Approve" node — rejection remains a Git revert. §3.8 actualizes the destructive taxonomy (app scope: `needs.*` removal; `expose.hostname`/`expose.network` change of a publicly-routed app; scale-to-zero; image repository change; env-reference removal — with soft carve-outs for literals, tag changes, adds, scale-from-zero, and the deferred `needs` selector/size changes) and the diff baseline (`Application.status.lastAppliedSpec`, effective per-environment); §3.1 replaces the pre-1.83b `public: … , network: …` expose examples with the current single-field `expose.network: "public" | "internal" | "vpn"` form. Rev 11: Phase 2 — Platform Services (M2) — **closed** 2026-06-10: subphases 2.1–2.12 deliver the launch platform-services scope (pg + redis + `needs.disk`, ADR 0034) — ServiceProvider/ResourceClaim machinery, CloudNativePG + Dragonfly providers, per-environment deploy (ADR 0044), needs-derived egress CiliumNetworkPolicy (ADR 0045), and `Application.env` value references (ADR 0046, §3.1: literal / bare `claim.<type>.<field>` / braceless `secret: "<name>/<key>"` → operator-resolved `secretKeyRef`; the 2.4e `DATABASE_URL` auto-injection removed; cue-cmp + `apprafter app validate` inject the schema + a generated `claim` binding so the scaffold no longer vendors the schema). spec.md §3.1/§4.4/§4.5 actualized (env-value model + the 2.9-deferred unification→override-wins and substrate→scalar reconciliation); §6 M2 box flipped at the 2.10–2.12 gate (the launch-scoped 2.16a per-claim rotation + the 2.17 checklist-housekeeping follow, 2.13–2.16 Notifications dropped); JetStream/ClickHouse/S3 providers, SPIRE workload-identity, and the Backstage ResourceClaim view deferred post-launch. Rev 10: M1.5 — Self-managing platform rethink — **closed** 2026-06-02: §6 milestone box flipped after the GitOps loop went green on the k3d e2e gate (CUE CMP render → Argo CD sync → operator reconcile → Deployment, with source-change propagation); `apprafter platform fork` (item 1.80), the Backstage MigrationPlan plugin (M3), and `platform channel` (M2) deferred; platform-scope MigrationPlan gate covered by operator unit + integration tests, real-infra migration e2e a nightly-Hetzner follow-up. No architecture change — the M1.5 design was actualized in Rev 7. Rev 9: SourceCredential CRD for private-repo credentials — ADR 0039: a config-only credential-reference CRD (§3.12) carrying zero secret material, the material sealed (SealedSecrets on Tier 1, OpenBao on Tier 2); the Application Operator derives both the Argo CD git repo-cred and the workload registry pull-secret from one source and reports validity in status; the `repo creds` CLI becomes a thin SourceCredential front-end; destructive credential change is gated via MigrationPlan. Rev 8: Managed-offering actualization for the Hosted Services launch — ADRs 0034–0038 + 0031: hardware-tier vs managed-plan terminology (0034), Minimal Data Exposure (0035), MCP agentic-safety (0036), managed control-plane infrastructure (0037), Tier 2 hard multi-tenancy via Kamaji changed to opt-in / default off (0038), apprafter-agent outbound connector (0031); managed-launch control-plane storage is embedded etcd with kine + NATS JetStream as the eventual target; managed-launch platform-services scope is pg + redis + needs.disk. Rev 7: Pre-Phase-2 spec refinements — tier model clarification, IPv6 strategy, Tenant CRD, Hubble + KEDA + Karpenter formalisation, multi-tenancy via Kamaji, cluster-admin constrain bundle, multi-cloud deferred to v2; Self-managing platform via Argo CD (M1.5): minimal cluster-bootstrap, PlatformStack CRD, unified MigrationPlan with application+platform scopes, CUE source + OCI chart distribution, CUE CMP for user app repositories).

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

| Tier | Persona | Substrate | Cost |
|------|---------|-----------|------|
| **1. Solo** | Solo founder, side-project | 1× VDS (Hetzner CPX22+) | €5–20/mo |
| **2. Small team** | 3–10 engineers, growing product | 3+ heterogeneous nodes (CCX or small dedicated; mixed sizes allowed) | €50–200/mo |
| **3. Production** | Established product, mid-size eng team | Bare metal (3–5× dedicated EPYC) | €500–2000/mo |
| **4. Regulated** | Compliance/sovereignty needs | External hyperscalers (AWS / GCP / Azure) | $2000+/mo |

Tier descriptions denote the **compute substrate** only. Features available at each tier (confidential containers, hard multi-tenancy, observability stack defaults) are described in §4.1 and Appendix C Feature Matrix. Tier 2 is **not** fixed at 3 nodes — it is the horizontal scaling pathway out of Tier 1, supporting heterogeneous configurations (mixed sizes, growing or shrinking node count over time).

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

Every change to the platform is a declarative resource reconciled by Argo CD. `kubectl apply` to production is an anti-pattern reserved for emergency overrides. Manual operations require explicit override and produce loud audit events.

The principle applies to the **platform itself** as well as user workloads. After bootstrap, the platform stack — Cilium, cert-manager, AppRafter operator, admission webhook, Backstage, and Argo CD itself — is reconciled by Argo CD from a versioned chart artifact (see §3.10 Platform stack management and §3.11 PlatformStack). The `apprafter cluster-bootstrap` command is a minimal loader that installs Argo CD and applies a single root `Application`; further reconciliation flows through GitOps.

Destructive changes — to user Applications or to the platform stack — are gated by `MigrationPlan` resources requiring explicit approval (see §3.8).

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
- Tier 1: soft multi-tenancy (Capsule policies + default-deny NetworkPolicy + workload identity); hard multi-tenancy is structurally not available on a single-node deployment
- Tier 2: HA substrate by default with the same soft multi-tenancy as Tier 1; hard multi-tenancy via Kamaji (separate Kubernetes control plane per tenant) is opt-in (`PlatformStack.spec.values.multitenancy: true`, default off — see ADR 0038)
- Tier 3+: hard multi-tenancy via Kamaji by default; see §3.9 Tenant CRD
- Tier 1 / dev mode: Hubble observability opt-in (default off to minimise footprint)
- Tier 2+: Hubble enabled by default (network observability + flow visualisation)

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
│  Tier 2: 3+ heterogeneous nodes (k3s HA)                 │
│  Tier 3: Talos + bare metal EPYC                         │
│  Tier 4: external hyperscalers (AWS / GCP / Azure)       │
│  Confidential containers: orthogonal opt-in (see ADR 0015)│
└─────────────────────────────────────────────────────────┘
```

---

## 3. Core Concepts

### 3.1 Application

The unit of deployment. Encapsulates workload, exposure, configuration, dependencies, networking, and per-environment overrides — all in a single CUE document.

```cue
apiVersion: apprafter.io/v1alpha1
kind: Application
metadata: {
    name: "parser"
}
spec: {
    // Shared base across environments
    base: {
        image: "ghcr.io/user/parser"

        expose: {
            port: 8080
            network: "public"                         // "public" → HTTPRoute on the platform Gateway; "internal" (default) → ClusterIP only; "vpn" → reserved

            // Optional — defaults to {app}.{env}.{platform-domain}. Consumed when network == "public"
            hostname: "parser-prod.example.com"

            // Optional — defaults to ["/"]
            paths: ["/", "/api/v1"]

            // TLS via cert-manager + ClusterIssuer; default true when network == "public"
            tls: true

            // Optional URL rewrites (Gateway API HTTPRoute filters)
            rewrites: [
                {from: "/api/v1/(.*)", to: "/internal/$1"}
            ]

            // WebSocket support — enables HTTP/1.1 Upgrade handling and longer idle timeouts
            websocket: true

            // Session affinity; default = websocket value (sticky when WS, non-sticky otherwise)
            sticky: true

            // IP family for ingress listeners; default inherits from Infrastructure.network.ipFamilies
            protocols: [ipv6, ipv4]
        }

        needs: {
            pg: {size: "small"}
            jetstream: {streams: ["blocks-head"]}
            redis: {}
        }
        env: {
            // literal — a quoted string
            LOG_LEVEL: "info"
            // claim ref — a bare CUE selector; composed DSN or any decomposed field
            DATABASE_URL: claim.pg.url
            DB_HOST: claim.pg.host
            // external secret ref — braceless "<name>/<key>" (SealedSecrets on Tier 1)
            API_KEY: secret: "third-party-tron/key"
        }
        connects: {
            egress: {
                external: [
                    {host: "api.tron.network", port: 443},
                    {host: "*.binance.com", port: 443},
                ]
            }
        }
        autoscale: {
            on:  "jetstream_lag"
            min: 1
            max: 10
        }
    }

    // Per-environment overrides — CUE unification, no templating
    environments: {
        dev: {
            replicas: 1
            expose: {network: "vpn"}  // reserved — reachable only over the VPN
            env: {LOG_LEVEL: "debug"}
            needs: {pg: {selector: {tier: "integrated"}}}
        }
        staging: {
            replicas: 2
            expose: {network: "vpn"}
            needs: {pg: {selector: {tier: "integrated"}}}
        }
        prod: {
            replicas: 3
            expose: {network: "internal"}  // internal only (ClusterIP)
            needs: {pg: {selector: {tier: "managed-aws"}}}
            confidential: true
            network: {
                egressIP: {
                    static: true
                    pool: "third-party-egress"
                    purpose: "tron-api-integration"

                    // Family for static IP allocation; default — both v4 and v6
                    families: [ipv6, ipv4]
                }
            }
        }
    }

    budget: {dev: "nano", staging: "small", prod: "medium"}
}
```

**Key properties:**

- **Per-environment overrides, override-wins (ADR 0044, 2.9).** No template strings, no `{{ .Values.image }}` — the selected `environments[<env>]` merges onto `base` override-wins (`image`/`replicas`/`expose` replace, `env` merges). The env is a **deploy-time, per-CR scalar** (`Application.spec.environment`): `apprafter app add --env <env>` deploys the same manifest as one self-contained Argo CD Application `<name>-<env>` in a user-chosen namespace — not a single-CR fan-out (cross-namespace ownerReferences are disallowed).
- **Each environment** is reconciled into a separate namespace (within a Kamaji `TenantControlPlane` on Tier 2+, see §3.9) with its own ServiceProvider selectors, so `dev` can use integrated PG and `prod` can use AWS RDS — same manifest, different physical reality.
- **Promotion between envs** is a platform operation (`apprafter promote parser staging prod` or Backstage button), not a manifest rewrite.
- **`needs` automatically generates** corresponding network policies — a dev declares `needs.pg`, the operator emits the egress rule. No duplication.

**Environment isolation across tiers.** On Tier 1, each Application environment maps to a namespace in the host cluster with default-deny NetworkPolicy and Capsule policies. On Tier 2+, each environment maps to a namespace **within an AppRafter Tenant's Kamaji TenantControlPlane** — providing hard API-level isolation between environments and across applications belonging to the same tenant. The Application manifest does not change between tiers; the namespace is chosen at `apprafter app add --env` (2.9 scalar model) and the substrate determines the isolation mechanism (plain namespace on Tier 1 vs Kamaji TenantControlPlane on Tier 2+). See §3.9 Tenant.

### 3.2 ServiceProvider

A declared backend for a platform service type. Multiple providers can coexist; applications select by labels.

```cue
apiVersion: apprafter.io/v1alpha1
kind: ServiceProvider
metadata: {
    name: "pg-integrated"
    labels: {tier: "integrated", location: "in-cluster"}
}
spec: {
    type:    "pg"
    backend: "cloudnative-pg"
    config: {
        cluster: "platform-postgres"
        nodes:   3
    }
}
```

```cue
apiVersion: apprafter.io/v1alpha1
kind: ServiceProvider
metadata: {
    name: "pg-aws"
    labels: {tier: "managed", location: "aws-eu-west-1", compliance: "soc2"}
}
spec: {
    type:    "pg"
    backend: "aws-rds"
    config: {
        region:         "eu-west-1"
        instance_class: "db.t4g.medium"
    }
}
```

### 3.3 ResourceClaim

Generated by the Application operator when an Application declares a `need`. Routes to a matching ServiceProvider.

```cue
// Generated, not authored
apiVersion: apprafter.io/v1alpha1
kind: ResourceClaim
metadata: {
    name:      "parser-pg"
    namespace: "default"
}
spec: {
    type:     "pg"
    selector: {tier: "integrated"}  // matched by app's `needs.pg.selector`
    size:     "small"
}
status: {
    provider:   "pg-integrated"
    connection: "secret-ref://workloads/parser/pg-conn"
    ready:      true
}
```

### 3.4 AccessGrant

Declarative access for a human or external system. Replaces ad-hoc kubeconfig + VPN-credential distribution.

```cue
apiVersion: apprafter.io/v1alpha1
kind: AccessGrant
metadata: {
    name: "alice-grant"
}
spec: {
    subject: "alice@company.com"

    // Optional — scopes the grant to a specific tenant's Kamaji control plane.
    // When omitted, the grant applies at the host cluster level (rare;
    // typically only for platform operators).
    tenant: "blockchain-team"

    scope: {
        namespaces:   ["dev", "staging"]
        capabilities: ["read", "exec"]
        resources:    ["pods", "logs", "deployments"]
    }

    // Optional — for host cluster-admin grants, two-person rule applies:
    // the grant becomes active only after all listed approvers sign.
    approvers: ["bob@company.com"]

    network: {
        routes:   ["10.0.0.0/16"]
        services: ["argocd", "grafana"]
    }

    mfa: "required"

    // For host cluster-admin emergency grants, recommended ≤ 1h.
    // For tenant-scoped read grants, longer durations (30d) acceptable.
    expiry: "30d"
}
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
6. User runs `apprafter login` to obtain an OIDC-backed kubeconfig (8h validity, auto-refresh).
7. Backstage shows current access status, expiry, ability to request renewal.
8. 5 days before expiry: reminder email. On expiry: auto-revoke (Headscale device removed, RoleBinding deleted, OIDC mapping cleared).

All auth events are written to the audit log (OpenBao audit + JetStream stream).

**Tenant scoping.** When `tenant:` is specified, the AccessGrant subject receives credentials valid only for that tenant's Kamaji TenantControlPlane. The subject cannot kubectl into the host cluster or other tenants' control planes. This is the structural foundation for MSP (multi-customer) scenarios and managed-Plane A/B separation.

**Two-person rule and JIT cluster-admin.** For host cluster-admin scope grants, the `approvers:` field is recommended (and required by policy for production-tier platforms). The grant enters `pending-approval` status; reconciliation produces the credential only after all approvers sign through Backstage or via API.

For emergency operations, **JIT cluster-admin AccessGrants** use short TTLs (`expiry: "1h"`) with mandatory `approvers`. Backstage displays a prominent "Emergency JIT access active" banner visible to all team members for the duration of the grant. Audit logs tag the event distinctly for downstream review.

### 3.5 ExternalSurface

Top-level platform manifest declaring the external contour: git host, registry, monitoring, backups, access plane.

```cue
apiVersion: apprafter.io/v1alpha1
kind: ExternalSurface
metadata: {
    name: "default"
}
spec: {
    git: {
        provider: "gitlab-self-hosted"
        url:      "git.platform.local"
        backups: {to: "s3://backup-bucket", schedule: "daily"}
    }
    registry: {
        provider:  "harbor"
        url:       "registry.platform.local"
        retention: "30d"
        signing:   "required"
    }
    access: {
        provider: "headscale"
        network:  "platform-team"
    }
    syntheticMonitoring: {
        provider:  "uptime-kuma"
        location:  "external-vps"  // out-of-band, separate failure domain
        endpoints: ["argocd", "grafana", "registry", "vpn"]
    }
    backups: {
        destination: "s3://platform-backup"  // external, never same failure domain
        retention:   "90d"
        encryption:  "required"
    }
    notifications: {
        providers: ["smtp", "slack"]
        smtp: {host: "smtp.example.com", from: "platform@example.com"}
    }
}
```

**Tier-aware co-location:** on Tier 1, ExternalSurface components (git, registry) can run on the same VDS as the cluster, in separate processes/containers. On Tier 2+, they're separate workloads in the cluster. Synthetic monitoring is **always** out-of-band on a separate small VPS — single-node failure cannot blind us.

**Bidirectional monitoring:** the cluster monitors ExternalSurface (uptime kuma watches git host, registry, etc.); ExternalSurface (via the external watcher VPS) monitors the cluster. Mutual self-healing is **not** in scope for v1.0 — too easy to false-positive into automated damage. Manual recovery procedures are documented as code (`kind: DisasterRecoveryPlan`).

### 3.6 ServiceProviderPlugin

Extension point for community-contributed service providers. Built-in providers (Postgres, JetStream, ClickHouse, Redis, S3) ship with the platform; everything else is a plugin.

```cue
apiVersion: apprafter.io/v1alpha1
kind: ServiceProviderPlugin
metadata: {
    name: "mysql-percona"
}
spec: {
    type:           "mysql"  // new resource type, becomes available as needs.mysql
    implementation: "oci://ghcr.io/community/mysql-percona-provider:v1"
    config: {
        // plugin-specific
        cluster_size: 3
    }
}
```

**Plugin tiers:**

- **Built-in (Rust):** statically linked into operator. Curated by core team. The six canonical types (Postgres, JetStream, ClickHouse, Redis, S3, Notifications).
- **Community (gRPC sidecar):** OCI image with gRPC server implementing the `ServiceProviderInterface` proto. Language-agnostic. Registered via `ServiceProviderPlugin`.
- **Future (WASM):** when WASI 1.0 stabilizes. Hot-loadable, sandboxed, no per-plugin process overhead.

**Registry:** community plugins published to a public catalog (separate repo). Each plugin is its own repo, can be MIT/BSD/whatever — no copyleft propagation through gRPC interface.

**Why this matters:** the platform team cannot afford to maintain providers for every database/queue/blob-store the world cares about. Community plugins make the platform extensible without bloat.

### 3.7 Infrastructure

Top-level manifest describing the substrate the platform runs on. Declarative, applied by `apprafter`.

```cue
apiVersion: apprafter.io/v1alpha1
kind: Infrastructure
metadata: {
    name: "platform-1"
}
spec: {
    provider: "hetzner-cloud"
    nodes: [
        {role: "control-plane", type: "cpx32", count: 3},
        {role: "worker", type: "ccx33", count: 5},
    ]
    network: {
        privateNetwork: "platform-net"
        floatingIPs: ["egress-tron-api", "egress-binance"]

        // IP family strategy (see §4.3.1).
        // Default — dual-stack (both v6 and v4).
        ipFamilies: [ipv6, ipv4]

        // When ipFamilies is single-stack and allowAppFamilyExtension is true,
        // individual Applications may opt into dual-stack pods.
        allowAppFamilyExtension: false
    }
    osImage: "talos-1.x"
}
```

`apprafter plan` shows diff. `apprafter apply` applies. State is stored in Git (encrypted via age/sops).

**Provider model:**

- **Built-in providers (v1):** Hetzner Cloud, Hetzner Robot (bare metal), AWS — implemented as native Rust SDKs for tight integration with cloud-native APIs.
- **Additional clouds:** deferred to v2. The `Provider` trait architecture supports adding new built-in providers on demand; no external plugin contract or SDK is shipped in v1.

If a concrete customer requirement for an additional cloud emerges before v2, support is added as a fourth native Rust implementation. See ADR 0016 for rationale and Appendix B Non-goals for the explicit scope statement.

**Idempotency anchor:** built-in providers label every managed cloud object with `apprafter=true`. This single label is the canonical reference for `apply` / `destroy` / `import` — if the local state file is lost, `apprafter import` reconstructs it by scanning live API objects for that label, without provisioning anything new. (See §4.12 for the matching CLI surface.)

### 3.8 MigrationPlan

A `MigrationPlan` is a declarative resource that gates destructive changes — to user Applications or to the platform stack — behind explicit approval. The same CRD covers both scopes via a discriminator field; per-scope behaviour is implemented through a Rust trait dispatch in `MigrationController`.

```cue
apiVersion: apprafter.io/v1alpha1
kind: MigrationPlan
metadata: {
    name:      "parser-pg-migration-2026-05-13"
    namespace: "apprafter-system"
}
spec: {
    scope: {
        type: application      // application | platform
        application: {
            ref: { name: "parser", namespace: "default" }
            environment: "prod"
        }
    }
    trigger: {
        type:  "selector-change"
        field: "needs.pg.selector"
        from: {tier: "integrated"}
        to:   {tier: "managed-aws"}
    }
    risks: {
        classification: "data-migration"   // safe | requires-restart | data-migration | breaking
        estimatedDowntime: "5–15 minutes"
        dataVolume: "12 GB"
        reversible: false
        requiresFullBackup: true
    }
    plan: [
        {step: 1, action: "Snapshot source DB to S3", estimatedDuration: "2m", reversible: true},
        {step: 2, action: "Provision target RDS instance", estimatedDuration: "5m", reversible: true},
        // ...
    ]
    approvers: ["alice@company.com"]
}
```

**Detection.** Reconcilers (the Application reconciler in the AppRafter operator, the PlatformController for platform changes) detect destructive changes during normal reconciliation. On detection, they create a `MigrationPlan` and **pause patching of dependent resources** — the source CR continues to exist with its new spec, but its child resources (Deployment, Service, HTTPRoute for Applications; Argo CD Application CRs for platform components) keep running the prior version. The source CR's `status.phase` reflects `AwaitingMigrationApproval`.

For application scope the diff is computed against `Application.status.lastAppliedSpec` — an operator-owned record of the last successfully applied spec, held in `status` (Argo CD ignores it, so it stays GitOps-clean) and re-stamped only after a non-blocked apply. Because one logical app is a separate `Application` CR per environment (§3.1), both sides of the diff are compared as the *effective* spec, each unified under its own environment, so a change in one environment gates that environment's CR alone. An application-scope `MigrationPlan` is created in the **application's own namespace** with a controlling `ownerReference` back to the `Application` CR, so Kubernetes garbage-collects it when the app is deleted and it renders in the user's Argo CD application tree without a separate anchor (ADR 0051).

**Gate location.** The gate is implemented inside the AppRafter reconcilers, not at the Argo CD layer. Argo CD remains a transport: it synchronises whatever is in the source. AppRafter reconcilers gate the propagation from "CR in cluster" to "child resources reflect the CR's spec." This avoids fighting Argo CD's automated sync and keeps Argo CD's role simple.

**Approve / reject semantics differ by scope:**

- **`application` scope:** approve-only. There is no explicit reject. The application manifest lives in the user's Git repository; if the user wants to reverse a change, they revert the commit in the source repo. Argo CD synchronises the reverted manifest, the reconciler observes it as a non-destructive (or differently-destructive) change, and the original MigrationPlan is superseded.
- **`platform` scope:** approve or reject. The platform target lives in the cluster (PlatformStack CR, see §3.11), not in a user repository. Reject means "revert `spec.pin` to the value stored in the plan's previous-spec snapshot annotation."

**What's destructive (triggers MigrationPlan).** For **application** scope, over the effective (per-environment) spec (ADR 0051):

- Removal of any `needs.*` entry — data loss, because the backing `ResourceClaim` and its data are garbage-collected (`data-migration`).
- `expose.hostname` removal or change **of a publicly-routed app** (`expose.network: "public"`) — the app becomes unreachable on the old hostname and the certificate churns (`requires-restart`). On a non-public app no route is emitted, so a hostname edit is inert.
- `expose.network` change from `public` to a non-public value (`internal` or `vpn`) — removes external reachability (`requires-restart`).
- `replicas` N → 0 (scale-to-zero) — a deliberate outage (`requires-restart`).
- Image **repository** (path) change — a different image rather than the same image at a new tag (`requires-restart`).
- Removal of an env value that is a **reference** (a `claim.*` selector or a `secret: "name/key"` reference, §3.1) — the workload loses a wired dependency (`requires-restart`).

**Security axis — additive and escalation edits (ADR 0052).** The list above is the *availability / data-loss* axis: a removal or reduction breaks a running service. The inverted threat — an actor with manifest write access who wants to exfiltrate data or widen the blast radius — does not remove, it **adds and escalates**. These edits gate under a new class `security-boundary`, ranked **above** `data-migration` (a data-loss change is coverable by backups; a leaked credential or a widened exposure is irreversible), so it wins the plan's primary when several ops apply:

- **Env `secret:` reference added** — the workload gains a fresh external-secret dependency it never had (adding a `claim.*` reference is self-scoped per §3.1 and stays soft).
- **Env reference downgraded to a literal** — a resolved-at-runtime reference replaced by an inline value.
- **Env `secret:` reference retargeted** to a different external secret.
- **`expose.network` escalated** from a non-public value to `public` — the app's blast radius widens from cluster-internal to the public Gateway.
- **A public hostname added** — publishing a new externally-reachable name on a publicly-routed app.
- **The public port retargeted** on a publicly-routed app — moving the exposed surface (which could expose an internal debug or metrics port).
- **`imagePolicy.resolve` relaxed** from `off` back to `digest` on a floating tag — a pinned reference returns to a mutable pull surface (a moved tag can then serve different content without a further manifest edit; an already-digest image is a no-op, and `digest → off` is hardening and stays soft).
- **Image repository (path) change** is classified `security-boundary` (not `requires-restart`) — a different repository can serve entirely different content.

The plan records the **full** blast radius, not just the primary: `spec.risks.classifications[]` lists the distinct classes present, and `spec.changes[]` drills into every detected change (each carrying its `type`, `field`, `classification`, `from`, and `to`). This makes the plan tamper-evident — a `security-boundary` op cannot be laundered behind a benign-looking primary.

**Structural hardening (ADR 0052).** Three defenses ensure the gate cannot be disarmed:

- **Approval is bound to a content hash.** The plan stamps `spec.trigger.approvedSpecHash` over the full detected change set; at consume time the operator re-verifies it, so an approval for one edit cannot be applied to a different edit, and any drift re-gates the change as a fresh pending-approval plan.
- **`Application.status` is write-protected.** Only the operator's server-side-apply field manager (`apprafter-operator`) may write `Application.status`; the admission webhook rejects any other subject's status write, so the diff baseline (`status.lastAppliedSpec`) cannot be zeroed to disarm the gate.
- **`spec.environment` is immutable on UPDATE.** Flipping the environment swaps the entire effective spec (§3.1), so the webhook rejects any change to `spec.environment` — a different environment is a different deployment (`<name>-<env>`, §3.1), not an edit.

**Approve authority is separate from write authority (S-2, ADR 0052).** A manifest-write or status-write credential must **not** equal approve authority. The authoritative approve path is the Kubernetes status-subresource patch (`kubectl patch migrationplan … --subresource=status` → `phase: approved`), governed by RBAC on the MigrationPlan status subresource; the Argo CD "Approve" button is a convenience layered on top of Argo CD's own RBAC. The operator's SSA field manager is an **ownership label, not an authentication token** — its integrity rests on scoping `patch applications/status` and the MigrationPlan status write to the operator's ServiceAccount, so that no other subject can present it. Git write access alone never approves a plan.

**Deliberately not gated (§6, ADR 0052).** The MigrationPlan protects the git→cluster path only, and the following are recorded as intentionally ungated: an image *tag* change (the tag → digest auto-rollout owns it, §3.1); replicas-up / autoscale / budget (that is `ResourceQuota`'s job); a registry or CI compromise (addressed by image-signing and `SourceCredential`, §3.12); an in-cluster `kubectl` or RBAC subject acting directly on child resources; and cross-tenancy (Kamaji / Capsule, §3.9). These lie outside the MigrationPlan's scope by design.

For **platform** scope, and for ServiceProvider-declared destructive fields:

- Selector change for stateful claims (pg, clickhouse) — *deferred while a single integrated provider is the only option (ADR 0051); revisit when a second provider ships*.
- Major version upgrade of a platform service (e.g., pg 15 → 16).
- Storage class change.
- Any change marked `destructive: true` in the ServiceProvider schema.
- Platform-stack diffs classified as `requires-restart`, `data-migration`, or `breaking` (see §3.11 PlatformStack).

**What's NOT destructive (auto-applies).** For **application** scope these are soft — they auto-apply, and the soft-destructive ones emit a `SoftDestructiveChange` Kubernetes Event rather than gating:

- Any addition (`needs`, env, or `expose` add) and scale from zero (0 → N) or scale down to a non-zero count.
- Env value *literal* removal (a plain string, not a reference) — a failed rollout self-protects (new pods fail readiness, old pods keep serving, revert in Git).
- Image **tag** change on the same repository — resolved tag → digest and rolled out automatically (§3.1, image auto-rollout).
- A `needs.*.size` change — PVC and CNPG storage are expansion-only, so a shrink is rejected at the provisioner layer.
- Replica count changes that are not a scale-to-zero, and other expose edits that do not remove public reachability.

For **platform** scope: any diff classified as `safe`.

**No automatic expiration.** A `MigrationPlan` in `pending-approval` state remains there indefinitely. Auto-rejection would harm solo operators on extended absences. If a user wants to dismiss a plan without acting, manual reject (platform scope) or Git revert (application scope) is the path.

**Approval surfaces.** The CLI provides `apprafter migration list` (across all namespaces — platform-scope plans live in `apprafter-system`, application-scope plans in the application's namespace), `apprafter migration approve <name>` (resolves the plan's namespace automatically), and `apprafter migration reject <name>` (platform scope only — an application-scope reject is a Git revert). The Argo CD UI also exposes an "Approve" Resource Action via a Lua script on the plan node — for a platform plan under the platform-stack tree, and for an application plan under the user's own Argo application tree (ADR 0048, ADR 0051). A Backstage queue view across both scopes follows in the post-launch portal bundle.

**Approvers** are listed as email addresses in `spec.approvers`. When the AccessGrant subsystem (§3.4) is fully delivered, the field will accept identity references and approval will route through the same identity layer.

This turns "oops, blew up prod" into an **explicit gate** with risk visibility and human control.

See ADR 0027 for the full decision record, future enhancements (skip, partial migration), and risk analysis.

### 3.9 Tenant

Top-level CRD describing a multi-tenancy boundary. An AppRafter Tenant wraps a Kamaji `TenantControlPlane` (providing hard API-level isolation) and a Capsule `Tenant` (providing policy enforcement layered on top).

```cue
apiVersion: apprafter.io/v1alpha1
kind: Tenant
metadata: {
    name: "blockchain-team"
}
spec: {
    controlPlane: {
        // Number of Kamaji control plane pod replicas; HA recommended for T2+.
        replicas: 2

        // Datastore claim — uses standard ResourceClaim mechanism, matching
        // pg-integrated by default. The Kamaji controller's persistence shares
        // the same operational story as Application databases (backups, HA, observability).
        datastore: {
            type:     "pg"
            selector: {tier: "integrated"}
        }
    }

    // Tenant owners — receive cluster-admin scope inside the Tenant's TCP only.
    // Owners come from AccessGrants referencing this tenant.
    owners: [
        {from: "accessGrant", subjects: ["alice@team.com", "bob@team.com"]},
    ]

    // Capsule policies enforced within the tenant's namespaces.
    policies: {
        quotas: {cpu: 100, memory: "200Gi", storage: "1Ti"}
        allowedRegistries:     ["harbor.platform.local"]
        allowedRuntimeClasses: ["kata"]
        networkPolicies: {
            defaultDeny:        true  // enforced regardless; setting is for clarity
            allowSameNamespace: true  // pods in same namespace can reach each other
        }
    }

    // Optional — limit Applications that can be deployed into this tenant
    // by Application name or labels. Useful for MSP scenarios.
    applicationSelector: {
        matchLabels: {team: "blockchain"}
    }
}
```

**Lifecycle:**

1. Apply Tenant manifest via Argo CD.
2. AppRafter operator translates Tenant into:
    - Kamaji `TenantControlPlane` resource in `kamaji-system` namespace, with declared datastore claim.
    - Capsule `Tenant` resource within the TCP, with declared policies.
3. AccessGrants referencing this tenant resolve into cluster-admin role bindings **inside the TCP only**. Subjects receive a kubeconfig via `apprafter login --tenant blockchain-team`.
4. Application resources targeting this tenant are deployed into namespaces inside the TCP. Each environment of an Application maps to a separate namespace.

**Plane A/B separation in managed scenarios:**

When AppRafter is operated as a managed offering (Managed Ops or Turnkey Cloud), the operator side runs on the host cluster (**Plane A**). Customer workloads run inside their Tenant's TCP (**Plane B**). The operator does not have automatic kubectl access into customer TCPs — accessing customer data requires an explicit AccessGrant approved by the customer.

This is structural separation, not disciplinary. The Plane A operator has no credentials for Plane B unless the customer issues them.

**Tier behaviour:**

- **Tier 1:** Tenant CRD is accepted but produces Capsule-only enforcement (no Kamaji TCP — structurally unavailable on single-node). Soft multi-tenancy via Capsule + workload identity + default-deny NetworkPolicy.
- **Tier 2:** HA substrate by default, with the same soft multi-tenancy as Tier 1. Hard multi-tenancy with a Kamaji TCP per Tenant is **opt-in** (`PlatformStack.spec.values.multitenancy: true`, default off; see ADR 0038); Kamaji remains the mechanism when enabled (ADR 0023).
- **Tier 3+:** Full hard multi-tenancy with Kamaji TCP per Tenant by default.

Granularity is at the Tenant level. A single team (one customer of an MSP) typically maps to one Tenant containing multiple Applications and environments. For strict per-environment isolation (e.g. prod isolated from dev at API level), create separate Tenants (`team-nonprod`, `team-prod`).

Dev mode (DEV_MODE_SPEC §1) maps to Tier 1 footprint — Tenant CRD accepted but produces Capsule-only enforcement.

**References:** see ADR 0023 for full rationale.

### 3.10 Platform stack management via Argo CD

The platform stack — Cilium, cert-manager, the AppRafter operator, admission webhook, default NetworkPolicies, Backstage, and Argo CD itself — is managed declaratively via Argo CD `Application` resources after the cluster bootstrap completes. `apprafter cluster-bootstrap` is a minimal loader; subsequent reconciliation flows through GitOps.

**Bootstrap loader scope.** The loader performs exactly one Helm release: Argo CD itself. After Argo CD is running, the CLI applies a single root `Application` resource that points to the platform-stack OCI chart (see §3.11 PlatformStack and ADR 0028 for distribution). All other platform components — including cert-manager, which produces certificates for Argo CD's own UI on tier deployments with a configured domain — arrive through Argo CD reconciliation.

Argo CD's web UI uses a self-signed certificate on first boot, which is sufficient for port-forward access. Operators configuring a domain at bootstrap time receive a cert-manager-issued certificate within the time Argo CD takes to reconcile cert-manager and its `ClusterIssuer` (typically 2–5 minutes on Tier 1).

**Self-managing Argo CD.** Argo CD itself is shipped as one of the Applications in the chart. Version bumps and configuration changes to Argo CD flow through normal reconciliation, with the safety constraint that Argo CD updates are classified as at least `requires-restart` in MigrationPlan (see §3.8) and `syncPolicy.automated.prune` is set to false for the self-managing Application. A documented manual recovery path (`apprafter platform rescue`) reinstalls Argo CD from the loader if the self-managed Argo CD becomes unable to reconcile itself.

**Argo CD's role for user Applications.** User application repositories (containing `apprafter/Application.cue`) are connected to Argo CD by the user through the Argo CD UI, the CLI (`apprafter app add --repo <url>`), or Backstage Software Templates. CUE files are rendered to Kubernetes YAML by a Config Management Plugin (CMP) sidecar in Argo CD's repo-server; the sidecar is shipped as part of the platform-stack chart (see ADR 0029).

**Day-2 surface.** Argo CD UI surfaces the state of both platform components and user Applications side by side. Operators investigating Cilium health use the same interface as developers checking the parser service. There is no separate diagnostic flow for "platform vs workload."

Dev mode (DEV_MODE_SPEC) installs a reduced platform stack in the local k3d cluster; the same PlatformStack CRD and Argo CD-reconciled model applies, with the platform-stack chart's `tier: dev` overlay enabling minimal-footprint defaults.

See ADR 0025 for the full architectural rationale, ADR 0028 for distribution mechanics, and ADR 0029 for the CMP sidecar.

### 3.11 PlatformStack

A `PlatformStack` CR is the declarative control plane for the platform version. Exactly one instance exists per cluster, named `default`, in the `apprafter-system` namespace. It is created by `apprafter cluster-bootstrap` and edited thereafter through the CLI (`apprafter platform upgrade`, `apprafter platform channel`, `apprafter platform freeze`), through `kubectl edit`, or through Backstage.

```yaml
apiVersion: apprafter.io/v1alpha1
kind: PlatformStack
metadata:
  name: default
  namespace: apprafter-system
spec:
  # Channel selection. When pin is unset, the controller resolves to the
  # latest version of this channel.
  channel: stable                  # stable | beta | edge

  # Optional explicit version freeze. When set, channel is ignored for
  # resolution; channel still affects what the controller reports as
  # availableVersion.
  pin: "0.2.0"

  # Default false. When true, the controller automatically bumps to the
  # latest channel version, unless the diff is classified as destructive
  # (in which case a MigrationPlan is created and the bump is gated).
  autoUpgrade: false

  # Where the chart is pulled from. Defaults to the AppRafter upstream.
  # Forks override repoURL; tracking forks may keep upstream for
  # availability visibility (see ADR 0028).
  source:
    upstream: oci://ghcr.io/apprafter/platform-stack
    repoURL: oci://ghcr.io/apprafter/platform-stack
    checkInterval: 6h

  # Global values passed to the umbrella chart.
  values:
    tier: solo
    domain: example.com

  # Per-component overrides: freezes, value tweaks, enable/disable.
  overrides:
    cilium:
      pin: "1.16.5"              # do not move this component even on stack bump
    backstage:
      enabled: false             # opt out entirely
```

**Status reports** include `currentVersion`, `targetVersion`, `availableVersion`, `lastUpstreamCheck`, a per-component status array, a `versionHistory` ring buffer of recent transitions, and standard Kubernetes conditions including `UpgradeAvailable` when a newer version of the configured channel is detected.

**No explicit `spec.version` field.** Version resolution is implicit:
- If `spec.pin` is set, that exact version is targeted.
- Otherwise, the latest version available in `spec.channel` from `spec.source.upstream` is targeted.

The OCI registry tag is the canonical source of version truth; `status.currentVersion` reports what is actually applied. `status.versionHistory` keeps the most recent N transitions for rollback and audit.

**Channels.** Three channels follow the Talos and Kubernetes release-channel patterns: `stable` (tested combinations, recommended for production), `beta` (newer combinations, mostly stable), `edge` (latest, breaking changes allowed).

**Safe auto-upgrade.** When `spec.autoUpgrade: true`, the controller bumps `status.currentVersion` toward `status.availableVersion` only if the diff is classified as `safe` in the chart's compatibility metadata (ADR 0028). Any other classification (`requires-restart`, `data-migration`, `breaking`) triggers a `MigrationPlan` (§3.8) instead of an automatic bump. Because the MigrationPlan gate makes unattended auto-advance safe, **`cluster-bootstrap` writes `autoUpgrade: true` as the opt-out default on Tier 1** — push-and-it-converges, with an explicit `spec.autoUpgrade: false` to opt out. The bare-schema default stays `false` for raw creation and for currently-unwired higher tiers (ADR 0026).

**Curated upstream combinations.** The platform-stack chart at any given version ships a tested combination of component versions. Cross-component compatibility is validated upstream during release. Users do not need to think about which Cilium pairs with which cert-manager; the curated bundle is the contract. The PlatformController does perform an environment-diff check at apply time, confirming the cluster's current Kubernetes version satisfies the chart's `minimumKubernetesVersion` before patching component Applications.

**PlatformController.** A new component (delivered as part of the `apprafter-operator` binary or a sibling controller in the same workspace) reconciles PlatformStack CRs. It periodically checks the upstream OCI registry for new tags within the configured channel and updates `status.availableVersion`. On spec change, it pulls the chart at the target version, renders the umbrella chart with the merged values and overrides, computes a diff vs the currently-applied umbrella Application's values, classifies the diff, and either patches the umbrella Application (non-destructive) or creates a MigrationPlan (destructive).

**CLI awareness.** `apprafter` separately tracks its own version against upstream releases (npm-style, with a 24h TTL cache) and warns when a CLI update is available. This is unrelated to platform version; the user upgrades the CLI on their own cadence.

See ADR 0026 for the full decision record, including channel semantics, override behaviour, and re-evaluation triggers.

### 3.12 SourceCredential

A `SourceCredential` is a **config / reference CRD that carries zero secret material**. It declares which private git repositories and container registries a credential covers; the credential material itself lives **sealed** outside the spec (a SealedSecret on Tier 1, an OpenBao path on Tier 2). It exists because deploying an Application from a private repository needs two distinct accesses — Argo CD must read the repo (git-read) and the kubelet must pull the image (registry-pull) — and both must be stored without a plaintext Secret and gated like any other change. See ADR 0039.

```cue
apiVersion: apprafter.io/v1alpha1
kind: SourceCredential
metadata: {
    name:      "myorg"
    namespace: "apprafter-system"
}
spec: {
    // At least one of git / registry. The two halves are independent;
    // a single classic PAT is simply the same backend in both.
    git?: {
        backend:      #Backend         // sealedSecretRef | openBaoPath
        repoPrefixes: ["github.com/myorg/"]
    }
    registry?: {
        backend: #Backend
        hosts:   ["ghcr.io/myorg/"]
    }
}
// #Backend = { sealedSecretRef: {...} } | { openBaoPath: string }
// No token, no base64 material, ever, in the spec.
```

**One source, two derived outputs.** The Application Operator owns all derivation (§4.5): from the single `SourceCredential` plus its sealed material it derives a **prefix-matched** Argo CD `repo-creds` Secret in `argocd` (Argo selects the credential for a clone by URL-prefix match) and a **host-matched** `dockerconfigjson` pull-secret in the workload namespace, auto-attached to the workload ServiceAccount / `Deployment.imagePullSecrets` by registry-host match against the rendered image. These derived Secrets are operator outputs, never hand-managed; rotating the material re-derives both halves. There is no second source of truth.

**Status.** Per-half `conditions` carry `Present` / `Valid` / `Invalid` / `Unverified`, the covered prefixes/hosts, and `lastValidated`. The operator performs real reachability validation (`git ls-remote` for git, a registry token-exchange/HEAD for registry) on change and periodically with backoff. Where the operator has no egress (air-gapped / restricted) validity is `Unverified`, not `Invalid`, and the coverage gate is configurable between `present` and `confirmed`.

**Material never in the spec, never in the app repo.** A CRD is not a Secret; material in `spec` would be plaintext-at-rest and scavenge-able (ADR 0024). It is also kept out of the application repository for a hard ordering reason: Argo needs the git credential to clone the private repo, so a credential living inside that repo could not be read to obtain it. The sealed blob is safe in transit, at rest, and in Git, which is what makes both delivery modes safe — CLI→cluster (`kubectl apply` the SealedSecret + CR) or commit-to-config-repo for Argo to sync. Host/prefix auto-match means the application repository carries nothing about credentials.

**Tier and gating.** The flow is identical on Tier 1 and Tier 2 — only the `backend` changes (SealedSecrets ↔ OpenBao path, see §4.4), behind the same CRD. Because `SourceCredential` is an AppRafter CRD, registration mutations pass through the admission webhook and the MigrationPlan gate (§3.8): rotating to an equivalent valid credential is non-destructive, while removing a covered repo-prefix or registry-host, deleting a credential while applications match it, or narrowing scope are destructive and produce a `MigrationPlan`. The gate is actor-agnostic — it catches a human editing a credential and the CLI alike.

The launch default is a single classic GitHub PAT (`repo` + `read:packages`) used in both halves — GitHub's coarse scopes, not a platform choice. The schema is split-ready (independent git / registry backends from day one), so a least-privilege split — a deploy-key or fine-grained PAT for git plus a `read:packages`-only PAT for registry, or a single narrow token on GitLab — is available as an opt-in. The `repo creds` CLI (§4.12) is a thin front-end over this CRD. See ADR 0039.

---

## 4. Layer Specifications

### 4.1 Compute Substrate

**Tier 1 (single VDS):** k3s in single-node mode with default SQLite backend during M1; kine + NATS JetStream backend introduced in M3. 1–2 GB RAM overhead. Cilium in CNI mode, kube-proxy replacement, KubeVirt disabled, Kata disabled. Dual-stack IPv4+IPv6 default. No quorum (single node). Hard multi-tenancy structurally unavailable on a single-node deployment; soft multi-tenancy via Capsule policies + default-deny NetworkPolicy + workload identity. Hubble opt-in (default off to minimise footprint).

**Tier 2 (3+ nodes, heterogeneous allowed):** k3s in HA mode. Control-plane storage at managed launch is **embedded etcd** (the standard k3s HA pattern); kine + NATS JetStream (≥3 replicas, embedded or as a workload) remains the eventual target and is introduced post-launch when audit-replayability or the etcd scale ceiling warrants it (see §4.2 and ADR 0005). Cilium with mTLS, full Gateway API. Hubble enabled by default. Hard multi-tenancy via Kamaji (one TenantControlPlane per AppRafter Tenant) is **opt-in** (`PlatformStack.spec.values.multitenancy: true`, default off — see §3.9 and ADR 0038); the Capsule policy layer applies regardless. Dual-stack IPv4+IPv6 default. Node configuration heterogeneous — mixed sizes allowed, scaling out from Tier 1 supported through `apprafter upgrade-tier`.

**Tier 3 (bare metal):** Talos Linux on dedicated EPYC servers. Full Kubernetes (not k3s). Cilium with full Gateway API, Hubble default. Kata containers as default runtime. KubeVirt enabled for VM workloads. LINSTOR for replicated block storage. Kamaji multi-tenancy. Confidential containers opt-in (when SEV-SNP-capable hardware is provisioned). Dual-stack networking default.

**Tier 4 (external hyperscalers):** AWS / GCP / Azure for regulatory or compliance-driven deployments. Talos Linux on cloud instances; same software stack as Tier 3. Kamaji multi-tenancy. Confidential containers opt-in (TDX / SEV-SNP capable instance types — AWS C8i/M8i/R8i, Azure DCadsv5/DCedsv5, GCP Tau VMs). Karpenter for node autoscaling (AWS native; see ADR 0021). Dual-stack networking default.

**Confidential containers** are an orthogonal opt-in feature available on any tier where the hardware supports it. See ADR 0015.

**Open question:** at what tier does kine+NATS stop scaling? See §7.

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

**Launch sequencing.** kine + NATS JetStream is the committed control-plane storage and the basis for the replayable-audit-log capability. At the managed launch, however, the Tier 2 HA substrate uses **embedded etcd** (the standard k3s HA pattern); kine + NATS is introduced post-launch when audit-replayability becomes a priority or the etcd scale ceiling is approached (see §4.1 Tier 2, ADR 0005, and `speedrun-plan.md`).

### 4.3 Networking

- **Cilium** as the only CNI, with kube-proxy replacement
- **Gateway API** as the only ingress/egress mechanism (no Service/Ingress/LoadBalancer split exposed to the dev)
- **mTLS by default** between all workloads via Cilium service mesh or SPIRE-issued certificates
- **NetworkPolicy: default-deny** at namespace creation; the Application's `connects` declares allowed flows
- **Dual-stack IPv4+IPv6 by default** across all tiers (per §4.3.1); single-stack opt-in via `Infrastructure.network.ipFamilies`

#### 4.3.1 IP family strategy

AppRafter runs **dual-stack IPv4+IPv6 by default** across all tiers. Both Hetzner Cloud (delegated /64 IPv6 per VDS) and AWS (full dual-stack VPC) provide IPv6 at no additional cost. Cilium has been production-ready for dual-stack since v1.12.

**Pod network:** every pod receives both IPv4 and IPv6 interfaces by default. Cluster CIDR uses dual notation: `--cluster-cidr=10.42.0.0/16,fd00:42::/64`. Cilium IPAM allocates from both pools.

**Service network:** Services are dual-family by default with IPv6 listed first in `.spec.ipFamilies`. CoreDNS returns AAAA records before A records, so IPv6 is preferred for cluster-internal traffic. Workloads needing IPv4 (legacy databases, external services without AAAA) fall through to A record resolution transparently.

**Ingress (Gateway API):** Gateway listeners accept both families simultaneously. On Tier 1 (Hetzner Cloud), the VDS public IPv4 and delegated /64 IPv6 are both forwarded to the Gateway. On Tier 4 (AWS), the native dual-stack ALB handles both families.

**Egress:** Applications open outbound connections via `getaddrinfo()`, which returns address lists for both families when present. Happy Eyeballs (RFC 8305, default behaviour in glibc, modern language runtimes) attempts IPv6 first with a ~250ms timeout, falling back to IPv4 if needed. Application code is agnostic to the family choice.

Static egress IPs (for third-party whitelisting via `Application.network.egressIP`) support both families per tier:
- **Tier 1:** node IPv4 + node IPv6 prefix delivered by Hetzner.
- **Tier 2–3:** Cilium Egress Gateway with floating IPv4 + delegated /64 IPv6 attached to dedicated egress nodes.
- **Tier 4 AWS:** NAT Gateway with Elastic IP (IPv4) + native IPv6 egress through the VPC.

**Single-stack opt-in.** Operators may set `Infrastructure.network.ipFamilies: [ipv6]` or `[ipv4]` to deploy a single-stack cluster. This is a cluster-wide decision; per-tier deviation is not enforced by the platform.

**Heterogeneous mode.** When a cluster is configured single-stack but selected Applications need the other family, `Infrastructure.network.allowAppFamilyExtension: true` permits Applications to opt into dual-stack via their own `expose.protocols`. By default, `allowAppFamilyExtension: false` and Applications can only narrow (never extend) the cluster-level family list.

**NAT64.** A NAT64 + DNS64 component is **not** shipped in v1. Operators choosing IPv6-only deployment accept the trade-off that IPv4-only external services become unreachable. If NAT64 capability becomes a recurring requirement, it will be added as an opt-in platform component (deferred until concrete demand emerges).

**See also:** ADR 0017 for the full decision record including rationale, Happy Eyeballs mechanics, per-layer details, and re-evaluation triggers.

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
    LOG_LEVEL: "info"                       // literal — a quoted string
    DATABASE_URL: claim.pg.url              // claim ref — composed DSN from the ResourceClaim
    DB_HOST: claim.pg.host                  // decomposed claim field (user/pass/host/port/db)
    API_KEY: secret: "third-party-x/y"      // external secret ref — braceless "<name>/<key>"
}
```

Each ref resolves to a container `EnvVar{valueFrom: secretKeyRef}` — into the claim's connection Secret (claim ref) or the named Secret (secret ref). On Tier 1 the secret's backing Secret is produced by SealedSecrets; on Tier 2+ OpenBao backs it (Vault Secrets Operator syncing into a Secret, or Vault Agent) — **the same `secret: "<name>/<key>"` ref is backend-agnostic**, and the secret's lifecycle (rotation, dynamic leases) lives at its source layer, not in the env ref (Phase 3). Devs don't see the difference — they reference by logical name.

**Migration Tier 1 → Tier 2:** `apprafter upgrade-tier` includes a step to import existing SealedSecrets into OpenBao and rewrite Application manifests. One-time, non-destructive.

### 4.5 Application Operator

Custom operator written in **Rust** on **kube-rs**. Distributed as a single Helm chart (`operator/charts/apprafter-operator/`) packaging two cooperating binaries — `apprafter-operator` (the reconcile loop) and `apprafter-admission-webhook` (a separate pod for cross-field validation). Image tags for both binaries are pinned by the `RELEASED_OPERATOR_VERSION` constant in `cli-providers` and built from the same git tag, so bootstrap never references a mixed pair.

**`apprafter-operator` — reconcile-loop responsibilities:**

1. Resolve the active environment (ADR 0044, 2.9): the env is a deploy-time **per-CR** property (`Application.spec.environment`, injected by the cue-cmp); the operator's `effective_spec` merges the selected `environments[<env>]` onto `base` **override-wins** (`image`/`replicas`/`expose` replace, `env` merges) — a pure-Rust merge for v1alpha1, switchable to CUE FFI once CUE-only constructs appear
2. Resolve `needs` → create `ResourceClaim`s with appropriate selectors
3. Wait for ResourceClaims to be `ready`, collect connection refs
4. Render Deployment + Service + Gateway Route + ScaledObject (KEDA) + NetworkPolicy + EgressIP allocations from the Application
5. Apply children via server-side apply with field manager `apprafter-operator` (cooperates with co-owners on shared fields); cascading delete via `ownerReferences`
6. Inject credentials via workload identity (SPIFFE), not mounted Secrets; configure secret injection from OpenBao (Vault Agent / CSI driver)
7. Update `Application.status` per environment with `phase`, `observedGeneration`, `conditions` (`lastTransitionTime` preserved across same-status reconciles per k8s `meta/v1.Condition` semantics), traffic, replicas, autoscale state, recent events, current egress IP
8. Reconcile `SourceCredential` (§3.12): derive a prefix-matched Argo CD `repo-creds` Secret in `argocd` and a host-matched `dockerconfigjson` pull-secret in the workload namespace (auto-attached to the workload ServiceAccount / `Deployment.imagePullSecrets` by registry-host match against the rendered image); validate git (`git ls-remote`) and registry (token-exchange/HEAD) reachability on change and periodically with backoff; write `Present` / `Valid` / `Invalid` / `Unverified` into `SourceCredential.status`, surfacing `Unverified` (not `Invalid`) where the operator has no egress. The derived Secrets are operator outputs; rotating the sealed material re-derives both halves.

Leader election via Lease (10s renew / 30s expiry, holder identity from `POD_NAME`). Prometheus signals (`reconcile_total`, `reconcile_duration`, `reconcile_errors`) and axum `/healthz` / `/readyz` / `/metrics` routes.

**`apprafter-admission-webhook` — validation responsibilities:**

Enforces cross-field invariants the OpenAPI v3 CRD schema can't express — `image` non-empty (via `base.image` or every `environments[*].image`), env names DNS-1123, env keys `^[A-Z_][A-Z0-9_]*$`. CUE schemas stay free of half-measure regex stubs and remain the design-time view; runtime enforcement layers as **CRD OpenAPI v3 → admission webhook**. TLS cert is auto-rotated via cert-manager; `caBundle` is synced onto the `ValidatingWebhookConfiguration` via the `cert-manager.io/inject-ca-from` annotation.

**`apprafter-agent` — managed-plan connectivity (optional).** When a cluster is attached to a managed plan (ADR 0034), an `apprafter-agent` opens an **outbound** connection from the customer cluster to the hosted bus (gRPC streaming over HTTP/2 + TLS; see ADR 0031). It ships from the open-source operator workspace — so the cluster stays fully functional without it — has no inbound listener, and exposes only AppRafter-CRD operations and metadata, never raw Kubernetes access or customer data (ADR 0035). It is absent on self-host-only clusters.

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

**Managed launch scope.** At the Hosted Services managed launch the integrated providers shipped are **`pg` (CloudNativePG)** and **`redis` (Dragonfly)** — 2 of the 6 — together with the **`needs.disk`** block-storage claim primitive. `jetstream`, `clickhouse`, `s3`, and `notifications` are deferred to a prioritised post-launch backlog and re-activated on demand (see `speedrun-plan.md`); transactional email at launch goes direct rather than through the notifications service. The full six-service set remains the eventual target.

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

- **Git host & container registry:** at the Hosted Services launch the source of truth is **GitHub + GHCR** (`ghcr.io`); self-hosted git hosts (GitLab, Forgejo) and registries (Harbor) re-activate only for self-host compliance customers (see `speedrun-plan.md`). Private-repo and private-registry access is carried by the **`SourceCredential`** CRD (§3.12) — a config-only object with sealed material from which the operator derives both the Argo CD git repo-cred and the workload registry pull-secret. Credentials are never set up separately or stored as plaintext Secrets.
- **CI runners:** GitLab Runner or Woodpecker, deployed as workloads on the same platform.
- **Synthetic monitoring:** Uptime Kuma on a separate small VPS (the only required external dependency) or external SaaS free-tier. Watches platform endpoints from outside.
- **Backup destination:** external S3-compatible (Hetzner Storage Box, Cloudflare R2). Required external — never store backups in the same failure domain.
- **Bootstrap CLI:** `apprafter init --provider hetzner --tier solo` provisions a fresh cluster from zero. Same CLI handles tier upgrades.

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

### 4.12 Infrastructure Tooling (`apprafter`)

Single Rust binary that manages the substrate (everything below the cluster). Inspired by Vercel/Heroku UX — solo founders should not have a worse experience than they'd get on a closed-source PaaS.

**Workflow:**

```bash
# Initial provisioning
apprafter init --provider hetzner-cloud --tier solo --region nbg1

# Show diff before applying
apprafter plan

# Apply changes from Git
apprafter apply

# Tear down the cluster (idempotent; filters on apprafter=true)
apprafter destroy --yes

# Reconstruct a lost state file by scanning live cloud resources
apprafter import

# Retrieve the cluster kubeconfig (decrypts the age-encrypted cache, fetches via SSH on miss)
apprafter kubeconfig

# Retrieve the Argo CD admin password (same cache semantics)
apprafter argocd-password

# Minimal bootstrap loader: installs Argo CD via Helm, creates the default
# PlatformStack resource, and applies a root Application pointing at the
# platform-stack OCI chart. All other platform components — Cilium,
# cert-manager, the AppRafter operator, admission webhook, Backstage,
# default NetworkPolicies, and self-managed Argo CD — arrive through
# normal Argo CD reconciliation after the loader completes.
apprafter cluster-bootstrap

# Open a UI in the browser with port-forward and auto-fetched credentials.
# Supports argocd, backstage, grafana (when present), hubble (Tier 2+).
apprafter open <ui-name>

# Platform stack management — operates on the PlatformStack CR in cluster.
apprafter platform status              # current/available versions, components
apprafter platform upgrade [--to <v>]  # bump spec.pin (or to latest of channel)
apprafter platform channel <name>      # switch channel: stable | beta | edge
apprafter platform freeze <component>  # pin a component version inside spec.overrides
apprafter platform unfreeze <component>
apprafter platform fork --to <oci-ref> # power-user fork bootstrap (see ADR 0028)
apprafter platform rescue              # emergency: reinstall Argo CD from loader

# MigrationPlan management.
apprafter migration list
apprafter migration approve <name>
apprafter migration reject <name>      # platform-scope only; application-scope reverts via Git

# User application lifecycle (apps connected to Argo CD via the CUE CMP).
apprafter app add [<git-url>]          # register an app repo as an Argo CD Application
apprafter app list | status | logs | rollback | remove
apprafter app open <name>              # port-forward a user app + open the browser
apprafter app scaffold                 # generate apprafter/Application.cue from a runtime preset

# Private-repo / private-registry credentials — a SourceCredential front-end (§3.12).
apprafter repo creds add <name>        # shape-check + seal material + create/update a SourceCredential CR
apprafter repo creds list | show       # read status (coverage + validity) only — never the material
apprafter repo creds rotate <name>     # re-seal the material; the operator re-derives both halves
apprafter repo creds remove <name>     # delete the CR behind a reverse-dependency gate

# Tier upgrade (e.g., solo → team)
apprafter upgrade-tier --to team

# User-side login after AccessGrant
apprafter login
```

The CLI is a thin client over declarative resources. Most commands map directly to `kubectl patch` or `kubectl edit` on PlatformStack or MigrationPlan CRs. The same effects are achievable by editing the CRs directly; the CLI provides ergonomic wrappers. The `app` and `repo creds` groups manage Argo CD `Application` and `SourceCredential` CRs the same way; `repo creds` seals material client-side and can read only status, never the credential material (cryptographic, not merely RBAC — §3.12). CLI version is independent of platform version (see §3.11), and the CLI separately tracks its own update availability against upstream releases, warning when a CLI update is available.

**State and idempotency:**

- State lives at `.apprafter/state.json` (per checkout) — JSON in v0.1.x; will migrate to CUE-encoded once the state schema stabilizes.
- Sensitive material — kubeconfig YAML, Argo CD admin password — is cached **age-encrypted** under `APPRAFTER_AGE_KEY` (default `~/.config/apprafter/age.key`, mode 0600, auto-created on first run). The encrypted blob lives inside `state.json`; the identity stays out of the repo.
- All managed cloud resources are labeled `apprafter=true`. This is the canonical idempotency anchor for `apply` / `destroy` / `import`. `apprafter import` can reconstruct a lost state file by scanning the cloud provider's API for objects with this label — no fresh provision needed.

**Provider implementation:**

- **Hetzner Cloud, Hetzner Robot, AWS** — implemented via native Rust SDKs (`hcloud-rust`, `aws-sdk-rust`). Tight integration, no external binary. These are the only infrastructure providers shipped in v1.

If additional cloud support becomes necessary before v2, it is added as a fourth native Rust provider implementation (the `cli-providers::Provider` trait is generic). No external plugin contract or OpenTofu shim is shipped. See ADR 0016 for rationale and Appendix B Non-goals.

**No Ansible:** Talos is immutable, all node config goes through Talos API. Mutable Linux is not a target substrate.

**Bare metal flow:** for Tier 3+, `apprafter` orchestrates Talos `talos-bootstrap` (PXE/ISO) + `talm` for manifest generation. Same CLI, abstracted under tier-specific subcommands.

### 4.13 Cluster-admin constrain

AppRafter's security posture treats cluster-admin power as a risk to be minimised structurally, not merely audited after the fact. The default Kubernetes RBAC model grants cluster-admin god-mode (read any secret, exec any pod, modify any resource). This is incompatible with a security-first platform.

The complete cryptographic solution is Confidential Containers (CoCo, §4.1 Tier 3+), but it is hardware-dependent and workload-opt-in. For all other workloads, defense in depth applies: a bundle of mechanisms, each reducing a different aspect of cluster-admin power.

| # | Layer | What it constrains | Reference |
|---|---|---|---|
| 1 | Workload identity (SPIFFE) | Auth between workloads via X.509 SVID, not cluster RBAC | §4.4 |
| 2 | Secrets via OpenBao + workload identity | Secret access via Vault Agent / CSI with SPIFFE check, not `kubectl get secret` | §4.4 |
| 3 | Kamaji TenantControlPlane separation | Tenant-admin ≠ host-admin; host cluster-admin has no automatic kubectl access into TCPs | §3.9 |
| 4 | Two-person rule via AccessGrant `approvers` | Host cluster-admin grants require explicit approval from a second admin | §3.4 |
| 5 | JIT cluster-admin via short-TTL AccessGrant | Emergency host grants TTL'd at 1h; auto-revoke; loud audit | §3.4 |
| 6 | Audit pipeline as code | All cluster-admin actions tagged and routed to immutable JetStream stream | §4.10 |
| 7 | OpenBao audit log | All secrets access logged with SPIFFE identity | §4.4 |
| 8 | Confidential Containers (CoCo) | Hardware-level memory encryption blocks cluster-admin read for confidential workloads | §4.1 Tier 3+ |

Each layer alone has gaps:
- Layer 1 requires SPIRE integrity.
- Layer 2 requires SPIFFE working.
- Layer 3 doesn't help if cluster-admin issues themselves a TCP grant (mitigated by Layer 4).
- Layer 4 is procedural and can be socially circumvented in small teams (mitigated by Layer 5 making grants short-lived).
- Layers 6 and 7 are forensic, not preventive.
- Layer 8 is hardware-bound and workload-opt-in.

Together, the layers reduce cluster-admin's realistic blast radius significantly while remaining operationally viable. The bundle is the structural answer to "minimize cluster-admin intervention in application workloads".

See ADR 0024 for the full rationale and per-layer detail.

---

## 5. Tech Stack Decisions

| Layer | Choice | Rationale |
|-------|--------|-----------|
| Operator language | **Rust** + kube-rs | Performance, idiomatic for control-plane work, low memory footprint. AI-assisted development closes the productivity gap. |
| Tooling / CLI | **Rust** (`apprafter` binary) and **Bun + project-internal opinionated TypeScript framework** (see github.com/AppRafter/onebun) for UI services and Backstage plugin backends | `apprafter` CLI in Rust; UI-facing services and Backstage plugins in TS/OneBun. |
| Backstage plugins | **TypeScript** (Backstage native) | No choice; Backstage is React/TS. |
| Config language | **CUE** | Typed, generic, validates schemas, importable. Pkl considered as alternative; deferred. |
| GitOps | **Argo CD** | Mature, open source, the only k8s tool with product-grade UI. The platform itself is reconciled through Argo CD (see §3.10). |
| Platform stack distribution | **OCI Helm chart** rendered from CUE source in our monorepo; signed via cosign; published to `ghcr.io/apprafter/platform-stack` with GitHub Release `.tgz` mirror | See ADR 0028. Standard Kubernetes ecosystem pattern; fork via OCI copy, no user-side Git repository required. |
| User app config rendering in Argo CD | **CUE Config Management Plugin (CMP) sidecar** in argocd-repo-server | See ADR 0029. Native Argo CD extension; activates only when `*.cue` files present in user repos. |
| CI | **GitLab CI** or **Woodpecker** | Whichever pairs with Git host choice. |
| Container runtime | **containerd** default; **Kata** Tier 3+; **Kata-CC** for confidential opt-in | Standard for Tier 1–2; isolation for higher tiers / confidential workloads (orthogonal opt-in, see ADR 0015). |
| Storage | **Local-path** Tier 1; **LINSTOR** Tier 3+ | Match complexity to tier. |
| OS | **Talos** Tier 3+; standard Linux Tier 1–2 | Talos is overkill for single-VDS; reasonable for bare metal. |
| Workload autoscaling | **KEDA** | Mature CNCF Graduated project; ScaledObject-based, exhaustive trigger ecosystem. See ADR 0019. |
| Node autoscaling | **Karpenter** — AWS native in Phase 6.2; Hetzner via Cluster API in Phase 5+ | Cluster-autoscaler not supported. See ADR 0021. |
| Network observability | **Hubble** (eBPF, Cilium-native) | Default Tier 2+; opt-in Tier 1 and dev mode. See ADR 0020. |
| Hard multi-tenancy | **Kamaji** + **Capsule** policy layer | Per-tenant separate kube-apiserver via Kamaji; Capsule for policy enforcement. Tier 3+ default; **Tier 2 opt-in** (`multitenancy: true`, ADR 0038); Tier 1 structurally unavailable. See ADR 0023. |
| Infrastructure providers | **Hetzner Cloud, Hetzner Robot, AWS** (native Rust SDKs) | Additional clouds deferred to v2. See ADR 0016. |

---

## 6. Roadmap

> **Managed-launch sequencing.** The ledger below is the open-source roadmap. The Hosted Services managed launch (ADR 0034) re-orders which items ship first — pulling a Tier 2 HA substrate (embedded etcd), a condensed MigrationPlan primitive, `needs.pg` / `needs.redis` / `needs.disk`, and selected Phase 4 items (ExternalSurface, HTTPRoute auto-gen, external-dns, backups, a Hubble + OTel subset) into the launch scope, while deferring others (the full six platform services, kine + NATS, Kamaji hard multi-tenancy, AccessGrant/OIDC) to a prioritised post-launch backlog. The managed track itself (hosted scaffolding, `apprafter-agent`, MCP server, billing) is product-2 work outside this ledger. See `speedrun-plan.md` for the launch sequencing; the milestone boxes here flip only on actual phase closure.

### Milestone M0 — Architecture (current)

- [x] Core principles documented
- [x] Tier model defined
- [x] Initial CUE schemas drafted
- [x] Codename chosen
- [x] Repository structure defined
- [x] License chosen (FSL-1.1-Apache-2.0 for core, MIT for plugins; see ADR 0001 + ADR 0032)

### Milestone M1 — MVP single-node ✅

**Delivered as `v0.1.0-mvp` on 2026-05-08.** The end-to-end path (`apprafter init` → `apply` → `cluster-bootstrap` → Argo CD sync → operator + admission-webhook stack → Bun hello-world `Application` live) is exercised nightly by `e2e/mvp.sh` on a real Hetzner account; observed wall-clock 6–9 minutes (well under the < 30-minute solo-tier budget).

Subsequent to M1 delivery, ADRs 0025–0029 reframe `cluster-bootstrap` as a minimal loader (Argo CD only) with the rest of the stack delivered through Argo CD reconciliation of a versioned OCI chart. The M1.5 sub-phase (see below) performs this rework before M2 starts; existing M1 acceptance is preserved through equivalent end-to-end coverage.

**Target:** working Tier 1 deployment on a single Hetzner CX/CPX-class VDS (default `cpx22` since `v0.1.42`, after Hetzner retired `cx22` upstream), deploying a hello-world Application end-to-end.

- [x] `apprafter init` provisions k3s + NATS + Cilium + Argo CD + Backstage on fresh VDS (NATS deferred to M2; everything else lands in v0.1.2 → v0.1.20)
- [x] Application CRD + Rust operator (basic: image, expose, no `needs` yet) (v0.1.21 → v0.1.32)
- [x] Argo CD configured, GitOps loop working (v0.1.13 → v0.1.17)
- [x] Backstage with minimal Application plugin (status view) (v0.1.18 → v0.1.20 + v0.1.33 → v0.1.36)
- [x] One golden-path template (Bun HTTP service) (v0.1.37 → v0.1.38, OneBun-based)

### Milestone M1.5 — Self-managing platform rethink ✅

**Target:** the platform stack reconciles itself through Argo CD from a versioned OCI chart. `cluster-bootstrap` is a minimal loader. User app repositories sync via Argo CD with a CUE CMP plugin. PlatformStack CRD provides declarative version control. MigrationPlan unified across application and platform scopes.

- [x] `platform-stack` CUE source in monorepo with CI publishing to `ghcr.io/apprafter/platform-stack` (signed, GitHub Release mirror)
- [x] Minimal `cluster-bootstrap`: install Argo CD via Helm, apply root Application
- [x] All platform components wrapped as templated Argo CD Applications inside the umbrella chart
- [x] Argo CD self-management with `prune: false` and MigrationPlan-gated upgrades
- [x] CUE CMP sidecar (`ghcr.io/apprafter/argocd-cue-cmp`) shipped as part of the chart
- [x] `PlatformStack` CRD + `PlatformController` (channels, autoUpgrade, overrides, status)
- [x] Unified `MigrationPlan` CRD with `scope: application | platform` discriminator
- [x] `MigrationController` with strategy dispatch for both scopes
- [x] CLI thin wrappers: `apprafter platform {status,upgrade,freeze,rescue}` (`channel` deferred to M2 pending stable/edge divergence; `fork` below)
- [x] CLI thin wrappers: `apprafter migration {list,approve,reject}`
- [x] `apprafter open <ui>` for UI access with port-forward + auto-credentials
- [ ] `apprafter platform fork` for power-user OCI fork bootstrap — deferred (power-user path, no launch dependency)
- [ ] Backstage plugin: MigrationPlan queue view — deferred to M3 (portal track)
- [x] CLI npm-style version check (own version vs upstream releases)
- [x] e2e/mvp.sh updated to exercise the new path end to end

**Acceptance:** `apprafter bootstrap-all` on a fresh Hetzner account produces a working Tier 1 cluster with all platform components reconciled by Argo CD. The Argo CD UI shows the umbrella platform Application plus child Applications for each component, all Healthy. `kubectl get platformstack default` shows the resolved version and current status. `apprafter open argocd` opens the UI with credentials filled. Adding a user app repo with `apprafter/Application.cue` via the Argo CD UI deploys the app end to end.

**Closed 2026-06-02.** The GitOps loop is proven green on the k3d e2e gate (`e2e/gitops-walk.sh`, `e2e-k3d` workflow): `cluster-bootstrap` brings up Cilium + Argo CD + the platform-stack umbrella with `PlatformStack/default`, then `apprafter app add` → CUE CMP render → Argo sync → operator reconcile → Deployment Available, and a source change (replicas 1→2) propagates end to end. The fresh-Hetzner-account path runs nightly via `e2e/mvp.sh`. Deferred with rationale: `apprafter platform fork` (item 1.80, power-user OCI fork — no launch dependency), the Backstage MigrationPlan queue plugin (M3 portal track), and `platform channel` (M2). The platform-scope MigrationPlan gate is covered by operator unit + integration tests; a real-infra (HTTPS-registry) migration e2e is a nightly-Hetzner follow-up (the k3d local registry is plain HTTP, which the controller's HTTPS-only OCI client cannot pull).

**Blocks M2** because Phase 2 ServiceProviders, ResourceClaims, and Tenant logic build on the GitOps-managed platform.

### Milestone M2 — Platform Services

**Target:** `needs.pg`, `needs.jetstream`, `needs.redis` working with integrated providers.

**Closed 2026-06-10** (the platform-services core, plan gate subphases 2.1–2.12). The launch platform-services scope (ADR 0034: pg + redis + `needs.disk`) ships end to end: ServiceProvider/ResourceClaim CRDs + scheduler/provisioner/GC, CloudNativePG (`pg-integrated`) and Dragonfly (`redis-integrated`) providers, `needs.disk` (ADR 0043), DSN/decomposed-key + external-secret env references (ADR 0046, the closing subphase), per-environment deploy (ADR 0044), and needs-derived egress CiliumNetworkPolicy (ADR 0045). The higher-numbered Phase-2 items are not M2-functionality gating: **2.13–2.16 Notifications** are SR:D-dropped from launch (managed portal + direct transactional email); **2.16a per-claim password rotation** is a launch hardening follow-on (SR:A); **2.17** is M2-spec-checklist housekeeping. Deferred with rationale: the **NATS JetStream / ClickHouse / S3 providers** (out of the Hosted-Services launch scope, ADR 0034 — the grammar is ready, the backends follow post-launch), **SPIRE workload-identity credential injection** (Tier 1 uses Secret-backed injection — SealedSecrets material, ADR 0024 Layer 2 — with SPIRE a Tier-2+ item), and the **Backstage ResourceClaim view** (post-launch portal bundle PL1, with the other portal surfaces). Credential injection is delivered via the connection-Secret `secretKeyRef` path (§3.1) rather than SPIRE for the launch.

- [x] ServiceProvider CRD
- [x] ResourceClaim CRD
- [x] CloudNativePG integration (pg-integrated provider)
- [ ] NATS JetStream provider (account/stream allocation) — deferred (post-launch; ADR 0034)
- [x] Dragonfly provider (Redis)
- [ ] Workload-identity-based credential injection (SPIRE setup) — deferred (Tier 2+; launch uses Secret-backed `secretKeyRef`)
- [ ] Backstage plugin for ResourceClaim view — deferred (post-launch portal bundle PL1)

### Milestone M3 — Multi-node + observability

**Target:** Tier 2 deployment (3 nodes), full observability stack.

- [ ] HA mode bootstrap in `apprafter` (dual-stack validated)
- [ ] Replace k3s SQLite with kine + NATS JetStream backend; verify watch/conditional-write semantics; SQLite→NATS migration tool if data preservation needed
- [ ] OpenTelemetry pipeline default for all workloads
- [ ] ClickHouse provider (logs, traces, app analytics)
- [ ] VictoriaMetrics integration
- [ ] Hubble enabled by default at Tier 2+; Hubble UI standalone (Helm flag); Grafana network dashboards
- [ ] Backstage flow visualizer plugin (Hubble flows on Application page, "convert flow to policy" button)
- [ ] Kamaji controller as platform-service; first TenantControlPlane provisioned for default tenant
- [ ] Capsule controller for policy layer
- [ ] AppRafter `Tenant` CRD operator integration
- [ ] Cilium Egress Gateway with family-aware static IP allocation
- [ ] OpenBao 3-node HA with auto-unseal
- [ ] Migration: SealedSecrets → OpenBao

### Milestone M4 — External Surface + Access

**Target:** declarative external contour, AccessGrant working.

- [ ] ExternalSurface CRD
- [ ] HTTPRoute auto-generation by operator (`expose.network: "public"` → working route + cert + sticky/WS as needed)
- [ ] GitLab/Forgejo deployable from manifest
- [ ] Harbor registry deployable from manifest
- [ ] Headscale + Tailscale Operator integration
- [ ] AccessGrant CRD + reconciler — tenant scoping, `approvers` for two-person rule
- [ ] JIT cluster-admin AccessGrant flow (short-TTL, mandatory approvers, audit-tagged)
- [ ] Backstage self-service flow for AccessGrant
- [ ] Cilium FQDN policies for `connects.egress.external`
- [ ] external-dns integration with `DNSZone` CRD
- [ ] Audit pipeline cluster-admin actions tagging; separate audit stream `audit.cluster-admin`

### Milestone M5 — Tier 3 (bare metal)

- [ ] Talos installation flow in `apprafter`
- [ ] LINSTOR provisioning automation
- [ ] Kata containers as default runtime
- [ ] MSP scenarios + multi-customer Kamaji scaling (Tenant CRD already covers single-instance multi-customer from M3)
- [ ] Migration path Tier 2 → Tier 3 (data + workloads)
- [ ] (When CAPI bring-up complete for Turnkey foundation) Karpenter on Hetzner becomes opt-in for OSS Tier 2+ clusters

### Milestone M6 — Tier 4 (regulated)

- [ ] AWS provider (`apprafter` integration; includes Karpenter standalone install as part of AWS stack)
- [ ] Kata-CC runtimeClass + nodepool selectors for confidential workloads (opt-in via `Application.confidential: true`; available on any tier with supporting hardware)
- [ ] Confidential service providers where applicable
- [ ] Attestation flow integrated with workload identity
- [ ] NAT64 component (marker — implemented on-demand when IPv6-only deployments request it)
- [ ] Bare metal slow autoscaling research (design constraint: UX/DX must not degrade compared to faster tiers)

### Milestone M7 — 1.0

- [ ] Stable CUE schema (semver guarantee)
- [ ] Complete docs site (TechDocs)
- [ ] Reference deployments published
- [ ] Public bootstrap-from-zero benchmark (target: <15 min from `apprafter init` to first Application live)

---

## Known limitations of v0.1.x

The following features are declared in the Application manifest schema and accepted by the operator, but are not enforced or fully implemented in the v0.1.x series. They are addressed in later phases of the roadmap (§6).

- **Platform stack installed imperatively.** Sub-phases 1.5–1.18 install Cilium, cert-manager, Argo CD, the AppRafter operator, admission webhook, and Backstage via direct `helm install` and `kubectl apply` in `apprafter cluster-bootstrap`. Drift correction does not apply to platform components, and changing platform configuration requires rebuilding the CLI. M1.5 (Self-managing rethink) replaces this with Argo CD-managed platform components from a versioned OCI chart.

- **HTTPRoute auto-generation** is deferred to Phase 4 (M4). v0.1.x renders Deployment + Service per Application, but does not auto-create Gateway HTTPRoute resources. Operators must manually configure routing via `kubectl apply` on HTTPRoute manifests until Phase 4 lands. Hostname conflict detection, TLS auto-issuance, WebSocket/sticky semantics, URL rewrites — all part of Phase 4 deliverable.

- **`confidential: true` flag** is deferred to Phase 6 (M6). The manifest field is accepted but does not enforce Kata-CC runtimeClass or confidential nodepool scheduling until the CoCo stack is implemented.

- **`connects.egress.external` not enforced.** The field is currently advisory only — Applications declare external dependencies for documentation purposes, but Cilium FQDN policies blocking non-declared egress land in Phase 4.

- **Static egress IP allocation manual.** `network.egressIP.static: true` is accepted in the manifest but not provisioned by the operator yet. Cilium Egress Gateway with family-aware static IP allocation lands in Phase 4.

- **Hard multi-tenancy is opt-in at Tier 2.** Tier 2's default is an HA substrate only; v0.1.x runs single-tenant. Hard multi-tenancy via Kamaji is opt-in (`PlatformStack.spec.values.multitenancy: true`, default off — ADR 0038). Kamaji + Capsule + AppRafter Tenant CRD land in Phase 3 (M3).

- **AccessGrant `approvers` / JIT cluster-admin** — fields accepted but reconciliation lands in Phase 4 (M4).

- **DNS automation.** External DNS records are managed manually in v0.1.x. external-dns + `DNSZone` CRD land in Phase 4 (M4).

- **MigrationPlan reconciler.** The `MigrationPlan` CRD schema is defined; reconciler implementation with both application and platform scopes lands in M1.5. Until then, all changes are applied immediately by the operator without approval gating.

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
- **Infrastructure tooling — built-in vs community:** **Hetzner Cloud + Hetzner Robot + AWS** as built-in (native Rust SDKs). Additional clouds **deferred to v2**: the `cli-providers::Provider` trait is preserved as a future extension point, but no community plugin contract or OpenTofu shim is shipped in v1 (superseded ADR 0011 → ADR 0016). No raw Terraform/Ansible exposed to users.
- **Migration safety:** **MigrationPlan** as a first-class concept. Destructive changes pause for explicit approval, with risk breakdown shown in Backstage.
- **Operator deployment:** **Helm chart** at `operator/charts/apprafter-operator/`. Two binaries — `apprafter-operator` (reconcile loop) and `apprafter-admission-webhook` (cross-field validation) — ship from the same git tag, pinned by the `RELEASED_OPERATOR_VERSION` constant in `cli-providers`. Bumping the constant and tagging a release happens in the same commit/PR series; otherwise a fresh `apply` pulls a non-existent tag from GHCR and bootstrap stalls.
- **Admission validation layering:** **CUE schemas stay design-time** and free of half-measure regex stubs. Runtime enforcement is **CRD OpenAPI v3 → admission webhook** (the webhook owns cross-field invariants like `image` non-empty, env-name DNS-1123 conformance, env-key `^[A-Z_][A-Z0-9_]*$`).
- **Tier-1 control-plane firewall topology:** **defense in depth** via fail2ban (SSH brute-force) + the cloud-provider firewall (network-level allow-list). Evaluated and dropped (v0.1.43): ufw — silent initcaps failure on Ubuntu noble during cloud-init.
- **Built-in cloud-provider idempotency:** every managed object is labeled `apprafter=true`; that label is the canonical anchor for `apply` / `destroy` / `import`. State at `.apprafter/state.json`; sensitive material (kubeconfig YAML, Argo CD admin password) cached **age-encrypted** under `APPRAFTER_AGE_KEY` (default `~/.config/apprafter/age.key`, mode 0600, auto-created on first run).
- **k3s install profile (Tier 1):** k3s starts with five disabled subsystems — `--disable=traefik,servicelb`, `--disable-kube-proxy`, `--flannel-backend=none`, `--disable-network-policy` — so Cilium owns CNI / kube-proxy replacement / NetworkPolicy without port collisions (the embedded flannel-vxlan daemon would otherwise sit on UDP 8472 alongside Cilium's VXLAN).
- **Multi-tenancy isolation choice:** **Kamaji** is the hard-multi-tenancy mechanism (one TenantControlPlane per AppRafter Tenant); **Capsule** as policy enforcement layer within tenant. vCluster and HNC evaluated and rejected. Default-on at Tier 3+; **opt-in at Tier 2** (`PlatformStack.spec.values.multitenancy: true`, default off — ADR 0038). T1 not eligible (structural limitation); soft mt via Capsule + workload identity. See ADR 0023 (mechanism) and ADR 0038 (Tier 2 default).

### License decision

- **Core platform:** **FSL-1.1-Apache-2.0** (Functional Source License → Apache 2.0 after 2 years), modeled on Sentry's approach (Sentry also uses FSL-1.1-Apache-2.0).
  - Each release carries a 2-year window of commercial-use protection (cloud-vendor SaaS rebranding restricted)
  - 2 years after a given release date, that release auto-converts to Apache 2.0 (fully OSI-approved)
  - Active development always lives in the FSL window; older releases are continuously freed
  - Bonus property: even if the project is abandoned, the most recent release is always at most 2 years from becoming Apache 2.0 — a built-in safety net for the community
  - Pre-ADR-0032 releases (v0.0.1–v0.1.96) were published under FSL-1.1-MIT and continue converting to MIT per their own anniversaries; see ADR 0032 and NOTICE
- **ServiceProviderPlugin SDKs and community plugins:** **MIT** from day one — minimal friction for community contribution
- **Business model:** managed offering (à la Akuity → Argo CD), not BSL/non-compete clauses

### Still open

1. **kine + NATS scaling ceiling.** What's the realistic upper bound? At what scale (nodes / objects / CRD churn rate) does sharding become necessary? Empirical question, answer in production.

2. **CUE vs Pkl re-evaluation point.** When and how to formally re-decide. Suggested: M5 (Tier 3 milestone), with a written ADR comparing.

4. **Migration tooling depth.** v1.0: manual MigrationPlan with operator-authored steps. v2.x: ship platform-provided automated migration plans for common cases (PG `tier: integrated → managed-aws`, ClickHouse major upgrade, etc.). Gradual increase in automation.

5. **Cost attribution model.** v1.0: **rough percentages** of cluster CPU/RAM/disk/network per Application. Refine over time toward query-count-based for DBs, byte-published for queues, etc.

6. **Backstage vs custom portal long-term.** Stay on Backstage for v1.0 (faster MVP). If we hit limits, evaluate custom portal on **OneBun + Svelte/React + Tauri** (existing experience). Bridge: keep portal API surface clean so backend can be swapped without losing custom plugins.

7. **WASM plugin readiness.** When does WASI 1.0 + threading + async I/O reach production-grade for our use case? Until then, gRPC sidecar plugins are the standard. ADR when transitioning.

8. **Bidirectional self-healing of External Surface.** v1.0: **monitoring only**. Self-healing is a separate hard problem (false-positives can cause cascading damage). Address in M5+ with explicit DR-as-code procedures.

9. **Codename.** Final naming, affects domain, repo, brand from day one. Candidates: `Cumulus`, `Bedrock`, `Helix`, `Substrate`, `Atlas`. Decision criterion: short, memorable, available domain (.io / .dev), no existing major OSS conflict. Chosen: `AppRafter`

10. **OneBun integration depth.** UI services (Backstage backends, build-report renderer, notifications service) are clear OneBun targets. `apprafter` itself is Rust (decided). UI layer beyond Backstage — TBD.

11. **Per-environment substrate (federated multi-cluster).** Should `dev` be able to live on a Tier 1 substrate while `prod` lives on Tier 3 — under one Backstage view? Compelling but requires multi-cluster control plane design. **Deferred to v2.x roadmap.**

12. **kine+NATS as Kamaji datastore.** Kamaji's default datastore options are MySQL, Postgres, etcd. kine supports a NATS backend. Combining kine+NATS as a Kamaji datastore is not officially validated. Research item — if it works, single-substrate (NATS for everything) becomes possible; if not, integrated Postgres remains the path. Phase 7+ or v2.

13. **Cert-manager bootstrap timing for users with domain from day one.** A user who configures `spec.argocd.domain` at bootstrap time expects HTTPS via cert-manager immediately. Current flow: bootstrap → Argo CD reconciles cert-manager → Argo CD UI eventually serves a real cert. The window between Argo CD coming up (self-signed) and cert-manager issuing the real cert (a few minutes) needs documented UX. See ADR 0025 still-open.

14. **MigrationPlan future enhancements: skip and partial migration.** A "skip this update, wait for next" action lets users acknowledge an available upgrade without acting on it. Per-component approval (instead of atomic plan approval) allows partial platform updates. Both deferred to a future iteration of ADR 0027; current scope is approve/reject only, atomic per plan.

15. **Cross-tier PlatformStack semantics.** When a user upgrades from Tier 1 to Tier 2, does the existing PlatformStack mutate (`spec.values.tier: solo → team`) or is a new instance created? Lean toward in-place mutation with MigrationPlan gating; will revisit in tier-upgrade design work for Phase 3. See ADR 0026 still-open.

16. **Multi-cluster PlatformStack aggregation.** The managed offering (ADR 0034; the cross-cluster aggregator) requires viewing platform versions across many customer clusters. Out of scope for v1; addressed in the managed-offering control-plane design (ADR 0037).

17. **Canonical filename for user app CUE.** Phase 1.11 uses `apprafter/Application.cue`. Alternatives include `apprafter.cue` at repository root or `.apprafter/app.cue`. Recommendation per ADR 0029 is to keep `apprafter/Application.cue`; settling early prevents fragmentation across user repositories.

18. **Multi-app monorepo strategy.** One Argo CD Application per service, or one ApplicationSet with a Git generator discovering service paths? ApplicationSet is the standard pattern but adds indirection. Decision deferred until multi-app monorepo use cases mature in Phase 2+. See ADR 0029 still-open.

19. **Compatibility metadata authoring tooling.** A future tool that analyses changelogs of upstream components (Cilium, cert-manager, etc.) and pre-fills the `compatibility.cue` classification. Out of scope for v1; CI enforces presence but classification remains a human decision. See ADR 0028 still-open.

20. **Non-GitHub fork support.** `apprafter platform fork` for GitLab and other Git hosts. Phase 2+ depending on user demand.

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
- Migration path is clean: `apprafter upgrade-tier` imports SealedSecrets into OpenBao on Tier 2+
- This is our **principle 1.8 in action**: enterprise practices must not block solo-tier adoption

### Why no multi-cloud in v1

- Hetzner Cloud, Hetzner Robot, and AWS cover the target audience (solo founders + small business in EU, regulated workloads on AWS) for v1.
- The earlier hybrid native-SDK + OpenTofu-shim approach (see superseded ADR 0011) introduced two state models, two error models, and two reconciliation paths — a leak of abstraction that compounds maintenance cost without proportional benefit.
- The `cli-providers::Provider` trait is preserved as a generic extension point. Adding a fourth native cloud is straightforward when concrete demand materialises. This is **not** "we cannot add clouds", it is "we don't add them speculatively".
- Crossplane was considered as an alternative but disqualified by its bootstrap problem (it requires an existing management cluster to provision the first VPS — incompatible with Tier 1 single-VDS bootstrap from CLI). Cluster API may be adopted at Phase 5+ for Turnkey customer hosting, but it is a Turnkey concern, not an OSS core dependency.
- See ADR 0016 for full rationale and re-evaluation triggers.

### Why dual-stack networking everywhere

- Both Hetzner Cloud (delegated /64 IPv6 per VDS) and AWS (full dual-stack VPC) provide IPv6 at no additional cost.
- Cilium is production-ready for dual-stack since v1.12+.
- Pods with both v4 and v6 interfaces handle outbound to IPv4-only legacy services natively, avoiding NAT64 middlebox complexity.
- Manifest portability is preserved — same Application works identically across all tiers.
- See ADR 0017 for the full per-layer strategy and NAT64 deferral rationale.

### Why Kamaji over vCluster for hard multi-tenancy

- AppRafter's security-first positioning (workload identity, OpenBao, CoCo) implies hard multi-tenancy should be a structural guarantee, not a policy-based aspiration.
- vCluster has higher insider attack surface through its syncer process, which holds permissions in both host and vcluster. Kamaji's per-tenant kube-apiserver pod is a cleaner separation.
- Cozystack uses Kamaji in production at scale — validation of the choice for multi-customer scenarios.
- T1 single-node cannot host hard multi-tenancy under either tool's model that aligns with bootstrap principles — T1 deliberately gets soft multi-tenancy as part of Tier 1 simplifications.
- See ADR 0023 for full rationale.

### Why defense-in-depth for cluster-admin

- The default Kubernetes RBAC model grants cluster-admin god-mode powers, fundamentally incompatible with security-first positioning.
- Confidential Containers (CoCo) provides cryptographic isolation but is hardware-dependent and workload-opt-in. It does not solve the problem for non-confidential workloads.
- A bundle of mechanisms — workload identity, OpenBao secrets, Kamaji TCP separation, two-person rule, JIT TTL, audit pipeline, CoCo where applicable — collectively reduces cluster-admin's blast radius significantly.
- See ADR 0024 for the full bundle and per-layer rationale.

### Why Argo CD as the platform control surface

- §1.4 declares GitOps as the only control surface. The Phase 1 implementation of `cluster-bootstrap` performed 9 imperative steps directly against the cluster, with platform components not reconciled by Argo CD afterward — Argo CD sat in the cluster as an optional appendix tracking at most a single user repository.
- Reconciling the platform stack through Argo CD makes drift correction work cluster-wide, surfaces platform health in the same UI as user workloads, removes the requirement to rebuild the CLI binary to change platform configuration, and makes audit logging native.
- The bootstrap chicken-and-egg (Argo CD must run before it can manage itself) is resolved by the standard "bootstrap loader" pattern used by Argo CD Autopilot, Flux bootstrap, and similar tools. The loader installs Argo CD only; the rest of the stack arrives through Argo CD reconciliation of a versioned OCI chart.
- See ADR 0025 for the full architectural rationale.

### Why a declarative PlatformStack CRD instead of CLI-embedded versioning

- The CLI binary is updated on the user's cadence; the platform stack ships on its own release cadence. Embedding platform version in the CLI binary would couple two release cycles that should be independent.
- A declarative CRD makes the platform version visible to Kubernetes audit logging, to Backstage, and to any external tooling reading the cluster state.
- The PlatformController periodically checks upstream for new versions and surfaces availability through `status.availableVersion` without requiring CLI invocation. Auto-upgrade is safe by default because destructive changes route through MigrationPlan gating.
- The version targeted by the user is implicit (`spec.pin` for explicit freeze, otherwise channel-resolved latest). Status carries the truth: `status.currentVersion` (what is applied), `status.availableVersion` (what is upstream), `status.versionHistory` (recent transitions for rollback and audit).
- See ADR 0026 for the full decision record.

### Why unified MigrationPlan with scope discriminator

- The earlier MigrationPlan design in §3.8 described only Application-scope destructive changes. The PlatformStack work introduced a parallel need for platform-scope destructive changes.
- Two separate CRDs (`MigrationPlan` + `PlatformMigrationPlan`) would duplicate schemas, controllers, webhooks, and UI surfaces. A single CRD with a scope discriminator and Rust trait dispatch keeps machinery minimal.
- Reject semantics differ legitimately by scope. Application manifests live in user Git repositories — to revert, the user reverts the commit; an explicit "reject" action on the in-cluster MigrationPlan would be confusing because it cannot affect the source repository. Platform manifests live in the cluster (PlatformStack CR), so reject is a meaningful action: revert `spec.pin` to the previous value.
- The gate is implemented inside AppRafter reconcilers, not at the Argo CD sync layer. Argo CD has already applied the CR to the cluster by the time our reconciler observes the destructive change; the practical gate is the propagation from "CR in cluster" to "child resources reflect the CR's spec." This keeps Argo CD's role as a simple transport.
- See ADR 0027 for the full decision record.

### Why CUE source + OCI chart distribution for the platform stack

- The platform-stack repository contains only CUE source. Rendered Helm chart artifacts are produced in CI and published to OCI on tag; they are not committed back to Git. This preserves both the "CUE as configuration language" positioning and clean repository history.
- OCI is the standard Kubernetes ecosystem distribution channel. Argo CD pulls OCI Helm charts natively. Cosign signing produces verifiable artifacts. Forking is a single `crane copy` to a different registry.
- A secondary GitHub Release attachment ships the rendered `.tgz` for users who prefer plain Helm without involving AppRafter components — an honest fallback that does not compromise the primary distribution path.
- The umbrella chart pattern (one template iterating over `values.components`) lets PlatformController patch a single Argo CD Application while producing N child Applications. Adding a new platform component is a CUE change in our repo, not a templates-folder change.
- See ADR 0028 for the full decision record.

### Why CUE Config Management Plugin for user app repositories

- User app repositories contain CUE (the golden-path template generates `apprafter/Application.cue`). Argo CD does not understand CUE natively; without a compilation step, GitOps deployment of user apps does not work.
- The Argo CD Config Management Plugin (CMP) extension point is a sidecar to `argocd-repo-server` and is the native mechanism for adding language/format support. It is used upstream for Kustomize variants, Jsonnet, Tanka.
- Server-side compilation preserves user experience — users write CUE, push, and Argo CD's repo-server renders. No local render step, no pre-commit hook, no separate "rendered output" branch.
- The CMP activates only when `*.cue` files are present. Users who prefer raw YAML are not forced into CUE.
- See ADR 0029 for the full decision record.

### Why MigrationPlan as a first-class concept

- The most common cause of production outages in declarative-GitOps systems is "I changed one line and the operator did something I didn't expect"
- Selector changes for stateful services (PG, ClickHouse) are inherently destructive — they require data migration, downtime, and explicit risk acceptance
- Auto-applying these silently is **worse** than no automation at all — it creates a false sense of safety
- MigrationPlan turns the implicit "this might blow up" into an explicit "this **will** require approval, here's the risk breakdown" — same model that Terraform `plan/apply` proved successful
- Approval is human-in-the-loop **by design**, not by accident

### Why FSL-1.1-Apache-2.0 for the core (Sentry's model)

- BSL has non-compete clauses that scare some enterprise users and are not OSI-approved
- Pure Apache 2.0 (without the FSL wrap) lets cloud vendors rebrand the platform as their managed offering with zero contribution back (the Cozystack-style risk we've been trying to avoid)
- AGPL on portal + MPL on core split is workable but creates licensing complexity and split governance
- FSL-1.1-Apache-2.0 (Functional Source License) provides a clean middle ground:
  - 2-year commercial use restriction (cloud vendors cannot offer it as their primary managed product)
  - Auto-converts to Apache 2.0 after 2 years (every release becomes fully OSI open)
  - Used in production by Sentry (since 2023), proven model
  - Single license to communicate, easier than tier-split licensing
- Apache 2.0 was chosen over MIT as the conversion target (ADR 0032): explicit patent grant + retaliation clause matter for a project that orchestrates Cilium/eBPF, SPIFFE, OpenBao, Kamaji and confidential-computing primitives (Kata-CC on Intel TDX, AMD SEV-SNP, ARM CCA); also aligns with the Kubernetes / CNCF ecosystem convention and provides an explicit trademark disclaimer
- The 2-year window is enough for us to establish a managed offering as the canonical commercial deployment, after which the protection naturally relaxes
- **Built-in safety net:** even if the project is abandoned, the most recent release is at most 2 years away from being Apache 2.0 — community can always continue without us
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

### Why infrastructure tooling in Rust + CUE, not Terraform/Ansible

- One configuration language for the user (CUE everywhere) reduces cognitive load
- For all v1 providers (Hetzner Cloud, Hetzner Robot, AWS), native Rust SDKs give the tightest integration and a single error / state model
- Talos OS makes Ansible obsolete for the substrate (immutable, API-driven)
- State in Git at `.apprafter/state.json` (sensitive material age-encrypted) avoids Terraform's "where does the state live" headache
- Additional clouds are deferred to v2 rather than carried as an OpenTofu shim — see "Why no multi-cloud in v1" above and ADR 0016

---

## 9. Glossary

- **Tier:** the deployment scale of the platform (1–4), affecting which substrate / providers are active.
- **Application:** the dev-facing unit of deployment, with per-environment overrides.
- **Environment:** a named target deployment (`dev`, `staging`, `prod`) with its own namespace (Tier 2+: within a Kamaji TenantControlPlane), ServiceProvider selectors, and exposure rules.
- **ServiceProvider:** a backend implementation for a platform-service type (e.g., `pg-integrated`, `pg-aws`).
- **ServiceProviderPlugin:** an OCI-distributed gRPC plugin extending the platform with new service types or backends.
- **InfrastructureProviderPlugin:** the plugin extension point for additional clouds. **Deferred to v2** (see Appendix B Non-goals and ADR 0016). The `Provider` trait in `cli-providers` is preserved as the future extension point; no SDK or plugin contract ships in v1.
- **ResourceClaim:** an internal CRD generated when an Application declares a `need`; routes to a matching ServiceProvider.
- **AccessGrant:** declarative access for humans/external systems, replacing manual kubeconfig + VPN distribution.
- **MigrationPlan:** declarative resource gating destructive changes — to user Applications or to the platform stack — behind explicit approval. Uses a `scope: application | platform` discriminator; see §3.8 and ADR 0027.
- **ExternalSurface:** the platform-managed external contour (git, registry, monitoring, backups, access).
- **Infrastructure:** the substrate manifest (nodes, network, OS image) managed by `apprafter`.
- **Platform Service:** one of the six canonical multi-tenant services (Postgres, JetStream, ClickHouse, Redis, S3, Notifications).
- **Integrated provider:** a ServiceProvider that runs as a workload inside the platform itself.
- **Managed provider:** a ServiceProvider that delegates to an external managed service (e.g., AWS RDS).
- **Substrate:** the compute layer (VDS / VM / bare metal) the platform runs on.
- **Workload Identity:** a SPIFFE-issued X.509 identity that pods use to authenticate to OpenBao, ServiceProviders, and other workloads.
- **Static Egress IP:** a fixed IP address assigned to an Application's outbound traffic, for third-party whitelisting.
- **SealedSecrets:** Bitnami's mechanism for encrypted secrets in Git, used as the Tier 1 default before OpenBao is required.
- **SourceCredential:** a config-only CRD (§3.12) referencing sealed git/registry credential material; the operator derives the Argo CD repo-cred and the workload pull-secret from it and reports validity in status. The `repo creds` CLI is a front-end over it. See ADR 0039.
- **Bootstrap loader:** the minimal scope of `apprafter cluster-bootstrap` after the M1.5 rethink — installs Argo CD via Helm and applies the root Application that points to the platform-stack chart. Everything else arrives through Argo CD reconciliation. See §3.10.
- **Channel (platform):** one of `stable`, `beta`, `edge` — determines which set of platform-stack versions the PlatformController considers when resolving `spec.pin: unset` and when reporting `status.availableVersion`. See §3.11 and ADR 0026.
- **CMP (Config Management Plugin):** an Argo CD extension point implemented as a sidecar in `argocd-repo-server`. AppRafter ships a CMP for CUE so user app repositories can use CUE as source and have Argo CD render it to YAML at sync time. See ADR 0029.
- **Hard multi-tenancy:** API-level isolation through separate `kube-apiserver` per tenant. AppRafter provides this via Kamaji on Tier 2+; see §3.9 Tenant.
- **Kamaji:** the hosted-control-plane project used by AppRafter to provide hard multi-tenancy. Each AppRafter Tenant maps to a Kamaji TenantControlPlane.
- **MigrationController:** the reconciler for `MigrationPlan` CRs. Dispatches scope-specific logic through Rust trait implementations. See §3.8 and ADR 0027.
- **Plane A / Plane B:** managed-offering architectural distinction. Plane A is the management plane operated by the AppRafter provider (host cluster, monitoring, Backstage). Plane B is the customer data plane (workloads inside customer Tenants' TCPs). The provider does not have automatic kubectl access to Plane B.
- **PlatformController:** the reconciler for `PlatformStack` CRs. Tracks upstream availability, classifies diffs, patches the umbrella Argo CD Application, and creates `MigrationPlan` for destructive changes. See §3.11 and ADR 0026.
- **PlatformStack:** the in-cluster declarative resource that controls the platform version. One instance per cluster, named `default`. See §3.11 and ADR 0026.
- **Platform-stack chart:** the OCI Helm chart produced from CUE source in the AppRafter monorepo, signed with cosign, and published to `ghcr.io/apprafter/platform-stack`. The chart's umbrella template renders one Argo CD `Application` per platform component. See ADR 0028.
- **Soft multi-tenancy:** isolation through namespace boundaries, RBAC, NetworkPolicies, and policy enforcement (Capsule). Cluster-admin remains a shared trust boundary. Used at Tier 1.
- **TCP (TenantControlPlane):** a Kamaji resource representing a tenant's hosted Kubernetes control plane. AppRafter's `Tenant` CRD wraps this resource and a Capsule `Tenant` resource together.
- **Tenant (AppRafter):** a top-level CRD wrapping Kamaji TenantControlPlane and Capsule Tenant. The unit of multi-tenancy isolation in AppRafter. See §3.9.
- **Two-person rule:** AccessGrant policy requiring approval by one or more additional admins (the `approvers` field) before a cluster-admin scope grant becomes active. Part of the cluster-admin constrain bundle, §4.13.
- **Umbrella chart:** the platform-stack Helm chart pattern: a single template iterates over components declared in values, producing one Argo CD `Application` per component. Lets PlatformController patch a single CR while producing N child Applications. See ADR 0028.

---

## Appendix A — Repository Structure

```
apprafter/
├── cli/                            # four-crate Cargo workspace
│   ├── platform-cli/               # `apprafter` binary crate (dir name kept for v0.1.x git continuity; renamed in v0.2.x)
│   ├── cli-core/                      # errors, Tier, logging (tracing → stderr), CUE subprocess wrapper, secrets (age)
│   ├── cli-state/                     # .apprafter/state.json reader/writer
│   └── cli-providers/                 # Provider trait + Hetzner Cloud impl + k8s bootstrap renderers; owns RELEASED_OPERATOR_VERSION
├── operator/                       # five-crate Cargo workspace + Helm chart
│   ├── apprafter-operator/            # reconcile-loop binary
│   ├── admission-webhook/             # cross-field validation binary (separate pod)
│   ├── operator-core/                 # shared kube-rs types, leader election (Lease), secrets
│   ├── operator-rendering/            # pure Application → [k8s object] function
│   ├── operator-controllers/          # per-CRD controllers (application/…)
│   └── charts/apprafter-operator/     # Helm chart — single source of truth for operator + webhook deployment
├── schemas/                        # CUE schemas — design-time view of all CRDs (apprafter.io/v1alpha1)
├── cue.mod/                        # CUE module manifest at repo root (module: "apprafter.io")
├── providers/                      # built-in ServiceProvider implementations (statically linked into operator in M2+)
│   ├── pg-integrated/
│   ├── pg-aws/
│   ├── jetstream-integrated/
│   ├── clickhouse-integrated/
│   ├── redis-integrated/
│   └── s3-integrated/
├── backstage-plugins/              # TS plugins for Backstage (backends use OneBun)
├── manifests/                      # platform manifests per tier
│   ├── tier-1/
│   ├── tier-2/
│   ├── tier-3/
│   └── tier-4/
├── e2e/                            # end-to-end harnesses (e.g. mvp.sh — real-Hetzner smoke, ~6–9 min)
├── docs/                           # TechDocs source + ADRs (docs/adr/) + changelog
├── examples/                       # reference Applications + Infrastructure manifests
├── .github/workflows/              # CI: lint, test, license-check, conventional-commits, release-operator, nightly E2E
├── platform-stack/                 # CUE source for the platform-stack Helm chart
│   ├── cue/
│   │   ├── platform.cue               # umbrella schema
│   │   ├── components/                # per-component CUE: cilium, cert-manager, ...
│   │   ├── tiers/                     # tier-specific overlays
│   │   └── compatibility.cue          # change classification per version
│   ├── Chart.yaml.tmpl
│   ├── README.md
│   └── CHANGELOG.md
└── argocd-cue-cmp/                 # CUE CMP sidecar Dockerfile + plugin.yaml
    ├── Dockerfile
    ├── plugin.yaml
    └── entrypoint.sh
```

**Notes on the layout (deltas from the original sketch):**

- `cue.mod/` is at the **repo root** (not under `schemas/`) so `schemas/` and `examples/` share import paths (`apprafter.io/schemas/v1alpha1`) — standard CUE monorepo practice.
- `cli/` and `operator/` are **separate Cargo workspaces** (no top-level `Cargo.toml`); always `cd` into one before running `cargo`.
- The OpenAPI v3 CRDs in the operator chart (`operator/charts/apprafter-operator/templates/crd-*.yaml`) are **generated from the `schemas/v1alpha1` CUE schemas** by `crdgen` (ADR 0047). The kube-rs Rust types in `operator-core` and the typed admission-webhook validators stay hand-written, but `just crd-check` machine-gates them against the CUE (CUE↔CRD byte-identity + Rust↔CUE field set), so none can silently drift.
- The `apprafter-agent` (managed-plan outbound connector, ADR 0031) ships from the `operator/` workspace — as a sibling binary or an `apprafter-operator` subcommand (distribution model still open) — and is not required for a self-host cluster to function.

**CI publishes:**

- `oci://ghcr.io/apprafter/platform-stack:<version>` on `platform-stack/v*` tags.
- `oci://ghcr.io/apprafter/argocd-cue-cmp:<version>` on `argocd-cue-cmp/v*` tags.
- GitHub Release attachment with rendered `.tgz` for users who want plain Helm.

---

## Appendix B — Non-goals

To prevent scope creep, the platform **explicitly does not** aim to:

- Be a general-purpose k8s distribution (Cozystack / Rancher's domain).
- Support arbitrary databases as platform services. Six, not fifty.
- Compete with Backstage as a portal; we extend it.
- Replace `kubectl` / `helm` / `k9s` for platform engineers — they remain the operations interface.
- Run on Windows nodes.
- Provide Function-as-a-Service / WASM as primary primitive (may be added later as opt-in).
- **Multi-cloud infrastructure beyond Hetzner Cloud, Hetzner Robot, and AWS.** Community plugins for additional clouds are deferred to v2. The `Provider` trait architecture supports future addition, but no SDK or plugin contract is shipped in v1. New cloud support, if introduced before v2, is added as native Rust implementations on demand.

---

## Appendix C — Feature Matrix

The matrix below describes default behaviours and opt-in availability of platform features across tier and mode combinations. Entries:
- **default** — feature enabled by default at this tier
- **opt-in** — feature available but disabled by default
- **required** — feature is structural; cannot be disabled
- **✗** — feature not available at this tier
- **layered (see ref)** — behaviour described in referenced ADR or section

| Feature | T1 prod | T1 dev mode | T2 prod | T3 prod | T4 prod | Managed Ops | Turnkey |
|---|---|---|---|---|---|---|---|
| Cilium CNI | required | required | required | required | required | required | required |
| Dual-stack networking | default | default | default | default | default | default | default |
| Hubble (network observability) | opt-in | opt-in | default | default | default | default | default |
| Hubble UI standalone | opt-in | opt-in | default | default | default | default | default |
| Backstage flow visualizer plugin | ✗ | ✗ | default | default | default | default | default |
| OpenBao | opt-in (with KMS) | opt-in | default | default | default | default | default |
| SealedSecrets | default | layered (DEV_MODE_SPEC §11.4) | opt-in (legacy fallback) | opt-in | opt-in | opt-in | opt-in |
| Workload identity (SPIFFE/SPIRE) | opt-in | minimal | default | default | default | default | default |
| KEDA (workload autoscaling) | default | opt-in | default | default | default | default | default |
| Kata containers | ✗ | ✗ | ✗ | default (T3) | default | per-tier | per-tier |
| Confidential containers (CoCo) | opt-in (hardware-dep, rare) | ✗ | opt-in (hardware-dep, rare) | opt-in (SEV-SNP hardware) | opt-in (TDX/SEV-SNP instances) | opt-in | opt-in |
| Karpenter (node autoscaling) | ✗ | ✗ | opt-in (Phase 5+ via CAPI) | research (slow autoscaling design constraint) | AWS native default (Phase 6.2) | default | default |
| Cluster-autoscaler | ✗ | ✗ | ✗ | ✗ | ✗ | ✗ | ✗ |
| Hard multi-tenancy (Kamaji) | ✗ structurally | ✗ | opt-in (ADR 0038) | default | default | default | default |
| Capsule policy layer | default (opt-out) | default | default | default | default | default | default |
| OpenTelemetry pipeline | opt-in | minimal | default | default | default | default | default |
| ClickHouse (logs/traces) | opt-in | ✗ | default | default | default | default | default |
| VictoriaMetrics (metrics) | opt-in | ✗ | default | default | default | default | default |
| Backstage portal | default | ✗ | default | default | default | default | default |
| Argo CD | required | required | required | required | required | required | required |
| Argo CD self-management via Argo CD | required | required | required | required | required | required | required |
| PlatformStack CRD | required | required | required | required | required | required | required |
| CUE CMP sidecar in Argo CD | default | default | default | default | default | default | default |
| Headscale (AccessGrant access plane) | opt-in (Tier 1 may use external Tailscale) | ✗ | default | default | default | default | default |
| Forgejo / GitLab self-hosted | opt-in | ✗ | opt-in | opt-in | opt-in | opt-in | opt-in |
| Harbor registry | opt-in | ✗ | opt-in | opt-in | opt-in | opt-in | opt-in |

The matrix is a living document; new features added in future phases are recorded here as defaults are established. Application-level fields (`needs.*`, `expose.*`, `confidential`, etc.) are not part of this matrix — they are described in §3 Core Concepts.

**Managed plans.** The `Managed Ops` and `Turnkey` columns are managed **plans**, a separate axis from the hardware tier (per ADR 0034). The launch managed plan is **Hosted Services**, in which AppRafter hosts only the management/UX layer while the customer's cluster stays a standard open-source install on the customer's own infrastructure; its feature behaviour is that of the customer's underlying hardware tier (T1/T2), so it is not a separate column. `Managed Operations` and `Turnkey Cloud` are post-launch plans. See ADR 0034 (managed-plan model), ADR 0035 (Minimal Data Exposure), and ADR 0037 (managed control-plane infrastructure).
