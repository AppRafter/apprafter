Refinement pass for the AppRafter landing page. Most of the structure
is correct, but several tone, technical, and visual issues need
fixing. Apply these changes:

## TONE — replace marketing-style copy

Several phrases drift into generic startup-landing tone. Replace:

- "Built for engineers who want control without complexity"
  → "Self-hosted platform for shipping applications. Manifests in
  CUE. Runs on Kubernetes."

- Triadic slogans like "Modern. Opinionated. Scalable."
  → remove entirely. The audience finds them empty.

- "Production-grade infrastructure that grows with you"
  → "Same manifest from a single VPS to confidential bare metal."

- Any phrase that could appear on Heroku's homepage in 2018
  → rewrite as a flat technical statement.

## VISUAL — reduce teal saturation

Teal is currently used as background fills, button states, illustration
fills — way too much surface area. Constrain teal to:

- The logo (legitimate use)
- The "Rafter" half of the wordmark in two-tone variant
- Exactly ONE primary CTA per section
- Code syntax highlighting (keywords only, not all colored tokens)
- Hover/focus accents on interactive elements

Everything else: slate/neutral. Target ~5-8% of total page area in teal,
not the current 25%+.

## VISUAL — replace tier scaling illustration

The current illustration of Tier 1→4 looks too "marketing illustration".
Replace with a technical diagram resembling documentation:

- Horizontal scale, left to right: Tier 1 → Tier 2 → Tier 3 → Tier 4
- Each tier represented by a labeled box with: tier name, hardware
  description, cost range
- Connections between tiers shown as thin arrows or dotted lines
  indicating "same manifest, different substrate"
- Style: ASCII-art-inspired or schematic drawing, NOT illustrated
  blocks with shading or gradients
- Could look like a system architecture diagram from a Cloudflare
  blog post

## VISUAL — code blocks restyle

Current code blocks look like generic VS Code embed. Refine:

- Background: dark slate (#1e293b in light theme, slightly lighter
  in dark theme for contrast against page background)
- No drop shadow
- 2px border-radius maximum (sharp design language)
- Filename label at top of block (e.g. "parser.cue", "infrastructure.cue")
  in muted small text
- Syntax highlighting palette: only slate + teal + one warm neutral
  for strings. NOT rainbow VS Code defaults.
- Keywords in teal, strings in warm neutral (#fbbf24 or similar muted
  amber), comments in muted-foreground, regular code in foreground
- Optional: subtle line numbers in muted-foreground

## VISUAL — spacing rhythm

Establish consistent vertical rhythm:
- Between major sections: 96px (or 6rem)
- Between sub-blocks within a section: 48px (or 3rem)
- Between heading and its body: 16px (or 1rem)

Apply uniformly. Currently rhythm is inconsistent — some sections feel
cramped, others have orphan whitespace.

## VISUAL — button corners

Per brand spec: max 2-4px border-radius on buttons. Current primary
CTA has noticeably rounded corners (~8px). Reduce to 4px. Apply to
all buttons consistently.

## CONTENT — remove "Built on Earth" / location tagline

If any "Built in [place]" or "Built on Earth" tagline exists in footer,
remove entirely. Location is not relevant to the technical audience.
Footer should be: logo + columns + copyright + license note. Nothing else.

---

## TECHNICAL ACCURACY — these fixes are critical

The current page contains technical content that diverges from the
project's actual specification. Fix the following:

### CLI name

The CLI tool is simply called `apprafter`. Subcommands follow:

- `apprafter init --provider hetzner-cloud --tier solo`
- `apprafter apply`
- `apprafter plan`
- `apprafter login`
- `apprafter upgrade-tier --to team`

NOT `apprafter-cli`, NOT `platform-cli`. Just `apprafter <subcommand>`.

### CUE Application manifest — use this canonical example

Replace any current Application manifest with this one. Do not invent
fields, do not add `replicas:` or `resources: {cpu, memory}` — those
fields do not exist in the platform's API.

```cue
kind: Application
name: parser

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
        API_KEY: from: secret("third-party/api/key")
        LOG_LEVEL: "info"
    }
    connects: {
        egress: {
            external: [
                {host: "api.example.com", port: 443}
            ]
        }
    }
    autoscale: {
        on: jetstream_lag
        min: 1
        max: 10
    }
}

environments: {
    dev: base & {
        expose: {public: false, network: vpn}
        needs.pg.selector: {tier: integrated}
    }
    prod: base & {
        expose: {public: true}
        needs.pg.selector: {tier: managed-aws}
        confidential: true
    }
}

budget: {dev: nano, prod: medium}
```

Key invariants:
- The unit is `kind: Application`, not "Service" or "Workload"
- Resource declarations are `needs.<service-type>` (pg, jetstream,
  clickhouse, redis, s3, notifications)
- Per-environment overrides via CUE unification (`base & { ... }`)
- No CPU/memory/replica fields — those are managed by `budget` and
  `autoscale`
- Selectors match ServiceProviders by labels (`tier: integrated`
  is the default, `managed-aws` is alternative)
- Environment variables sourced via `from: claim.X.Y` or
  `from: secret("path")`, never plain values for secrets

### Tier definitions — use precise wording

| Tier | Hardware | Use case |
|------|----------|----------|
| Tier 1 | Single VPS (Hetzner CX22+) | Solo founder, side project |
| Tier 2 | 3+ VPS or small dedicated | Small team, growing product |
| Tier 3 | Bare metal EPYC dedicated | Established product, mid-size team |
| Tier 4 | Confidential compute (TDX/SEV-SNP) | Regulated, compliance-driven workloads |

Cost ranges (approximate): €5-20 / €50-200 / €500-2000 / $2000+/mo.

### Platform Services — six canonical services

Not five, not seven. Exactly six:

1. **pg** — PostgreSQL (CloudNativePG)
2. **jetstream** — NATS JetStream (event log + queue + KV)
3. **clickhouse** — ClickHouse (analytics, logs, traces)
4. **redis** — Dragonfly or KeyDB (cache, sessions)
5. **s3** — MinIO or Garage (object storage)
6. **notifications** — HTTP-first notifications service (email, Slack,
   Telegram via plugins)

Each declared via `needs.<name>` in Application manifest. All run as
shared multi-tenant clusters, not per-app instances.

### MigrationPlan — describe this way

When mentioning migration safety, use this framing:

"Destructive changes (storage migration, major version upgrade,
selector switch for stateful services) generate a MigrationPlan that
pauses for explicit approval. The plan shows estimated downtime, data
volume, and step-by-step actions before anything executes."

Avoid: "we automatically migrate your data" (we don't, that's the
whole point — humans approve destructive ops). Avoid: "zero-downtime
migrations" (we don't promise that, MigrationPlan shows real downtime
estimates).

### License — exact wording

The platform is licensed under FSL-1.1-MIT (Functional Source License,
auto-converts to MIT after 2 years). When mentioning license:

- Short form: "FSL-1.1-MIT"
- Long form: "Functional Source License — converts to MIT after 2 years"
- NOT "MIT" alone, NOT "Apache 2.0", NOT "open source" alone

### Stack list — use these names exactly

If the "Stack & philosophy" section lists technologies, use these
exact names with these exact descriptions:

- **Talos Linux** — immutable Kubernetes-native OS
- **k3s / kine** — lightweight Kubernetes distribution
- **NATS JetStream** — control plane storage and event log (replaces
  etcd via kine)
- **Cilium** — networking and service mesh on eBPF
- **OpenBao** — secrets management (open MPL fork of Vault)
- **CUE** — typed configuration language
- **kube-rs** — Rust framework for the platform's operator
- **Backstage** — developer portal (extended with custom plugins)
- **OpenTofu** — used internally for community infrastructure
  providers (users don't write Terraform)

---

## Out of scope (don't change these)

- Section order is correct, keep it
- Hero headline wording is correct, keep it
- Self-hosted vs Managed structure is correct, keep both as
  "Coming soon"
- Footer column structure is fine
- Light/dark theme system is fine
- Logo placement and sizing are fine

Apply these changes in one pass. Generate updated full-page mockups
for both light and dark themes after revision.
