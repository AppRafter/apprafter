# ADR 0017: IPv6 strategy — dual-stack everywhere, no NAT64 in v1

## Status

Accepted (2026-05-12).

## Context

Spec.md §4.3 (rev.5) contained a placeholder: "IPv6 primary, IPv4 optional". This was a marker, not a specification.

Without an explicit strategy, every part of the platform (Cilium config, Gateway listeners, egress IP allocation, NetworkPolicy generation, Application manifest schema) would arrive at ad-hoc choices, accumulating drift over time.

Five distinct layers needed decisions:
1. Pod network (which family pods get on creation).
2. Service network (ClusterIP family for cluster-internal traffic).
3. Ingress (Gateway listener family).
4. Egress (outbound to external services, including legacy IPv4-only).
5. Per-tier deviation.

## Decision

### Default: dual-stack everywhere

All tiers run dual-stack by default. Each pod has both an IPv4 and an IPv6 interface. Services are dual-stack with IPv6 primary in DNS resolution. Gateway listeners accept both families. Egress traffic uses the family selected by Happy Eyeballs (RFC 8305) per destination.

### Opt-in single-stack

Available via `Infrastructure.network.ipFamilies` cluster-wide setting. Same default applies to all tiers; no per-tier deviation enforced by the platform.

```cue
kind: Infrastructure
network: {
    ipFamilies: [ipv6, ipv4]            // default — dual-stack
    allowAppFamilyExtension: false      // default; only relevant if cluster is single-stack
}
```

### Heterogeneous mode via `allowAppFamilyExtension`

When `ipFamilies: [ipv6]` (cluster IPv6-only) and `allowAppFamilyExtension: true`, individual Applications may declare `protocols: [ipv6, ipv4]` to receive dual-stack pods (cluster admin opts in to this capability).

Application can always **narrow** family list from the cluster default; widening requires `allowAppFamilyExtension: true`.

### NAT64 not shipped in v1

NAT64 + DNS64 (for IPv6-only pods reaching IPv4-only external services) is a known operational pattern, but adds a middlebox with known failure modes (MTU issues, ICMP translation quirks, DNS interception complexity). It is **not** shipped in v1.

Users who deliberately opt into IPv6-only deployment accept the trade-off that IPv4-only outbound services become unreachable. NAT64 is an opt-in component, possible at any tier, deferred until concrete demand emerges.

## Per-layer behaviour

### Pod network

Dual-stack pods are the default. Cilium configured via Helm values `ipv4.enabled=true, ipv6.enabled=true`. Cluster-CIDR and service-CIDR both dual notation: `--cluster-cidr=10.42.0.0/16,fd00:42::/64`.

### Service network

`Service.spec.ipFamilies: [IPv6, IPv4]` by default (IPv6 primary). CoreDNS returns AAAA first, A second. Cluster-internal traffic prefers IPv6 (shorter header, less overhead). Services that need IPv4 (legacy databases, etc.) fall back to A records.

### Ingress (Gateway API)

Gateway `Listener` resources are dual-family. On Hetzner Cloud Tier 1: VDS public IPv4 + delegated /64 IPv6 are both forwarded to the Gateway. On AWS Tier 4: native dual-stack ALB.

### Egress

Pods initiate connections via `getaddrinfo()` which returns address list. Happy Eyeballs (RFC 8305) tries IPv6 first with ~250ms timeout, falls back to IPv4. Application code does not see the family decision.

Outbound to legacy IPv4-only services (banks, payment processors, internal corp APIs) works natively because pods have IPv4 interfaces.

Static egress IPs (for third-party whitelisting) support both families:
- **Tier 1:** node IPv4 + node IPv6 prefix from Hetzner.
- **Tier 2-3:** Cilium Egress Gateway with floating IPv4 + delegated /64.
- **Tier 4 AWS:** NAT Gateway with Elastic IP (v4) + native IPv6 egress.

### Per-tier

Identical behaviour across T1-T4. Single-stack opt-in is a cluster-wide decision, not tier-bound.

## Application manifest expression

```cue
expose: {
    port: 8080
    public: true
    protocols: [ipv6, ipv4]  // ingress listeners; default both
}

network: {
    egressIP: {
        static: true
        pool: "third-party-egress"
        families: [ipv6, ipv4]
    }
}
```

Resolution order: Infrastructure default → Application override. Application may narrow family list; widening requires cluster-level opt-in.

## Rationale

### Hardware and provider support is universal

Hetzner Cloud delivers IPv4 + /64 IPv6 with every VDS at no extra cost. AWS supports full dual-stack VPC since 2023 at no extra cost. Cilium is production-ready for dual-stack since v1.12+.

### Application code does not change

Standard library `getaddrinfo()` + Happy Eyeballs means application code is agnostic to which family is used. No application logic changes are required for IPv6 adoption.

### NAT64 has known operational cost

NAT64 adds a middlebox with known failure modes. Not shipping it in v1 keeps the core platform's failure modes simpler. Users with IPv6-only requirements can layer NAT64 themselves when they need it.

### Manifest portability

Identical behaviour across T1-T4 preserves manifest portability (principle 0.2). Same Application works in dev mode (T1) and on regulated T4 hyperscaler without family-related changes.

## Consequences

**Positive:**
- Compatible with both IPv6-capable and IPv4-only external services out of the box.
- Future-proof against IPv4 address exhaustion / cost increases.
- Manifest portability across tiers.

**Negative:**
- Slightly higher resource consumption (dual interfaces per pod).
- IPv6 stack complexity must be operationally supported (firewall rules for ICMPv6, MTU considerations).
- Static egress IP allocation must handle both families.

**Trade-offs:**
- Simplicity (single-stack) traded for compatibility (works with everything).

## Risk

- **Hetzner Cloud + k3s dual-stack edge cases.** Known good as of 2026, but Hetzner-specific quirks (firewall, MTU mismatch with tunnels, ICMPv6 rules) may surface. Mitigation: audit and end-to-end test in plan.md subphase 1.2.
- **NAT64 demand emerges unexpectedly.** Some users may deploy IPv6-only and then hit IPv4-only external services. Mitigation: clear documentation that single-stack IPv6 trades off IPv4 reachability; NAT64 component path documented for on-demand addition.

## Owner

Core platform team; Cilium configuration in Phase 1.4 audit + Phase 3.1 HA setup.

## Re-evaluation triggers

- Cilium drops dual-stack support or changes recommendation.
- AWS / Hetzner change pricing such that dual-stack carries cost.
- NAT64 demand becomes a recurring customer ask (then implement opt-in component).
- IPv6-only environments become mandatory due to regulation (currently no such regulation exists for general workloads).

## References

- RFC 8305 (Happy Eyeballs v2).
- Cilium dual-stack documentation.
- ADR 0022 (Tier model — IP family identical across tiers).
- spec.md §4.3 Networking + §4.3.1 IP family strategy (new section).
- spec.md §3.1 Application (expose, network.egressIP).
- spec.md §3.7 Infrastructure (network.ipFamilies, allowAppFamilyExtension).
