# ADR 0007: SealedSecrets at Tier 1, OpenBao at Tier 2+

## Status

`Accepted`. Date: 2026-05-06.

## Context

Tier 1 of the platform is a single Hetzner CX22 (or equivalent) at
roughly €5 / month. There is no KMS available for OpenBao
auto-unsealing, and a manual Shamir unseal at every restart is
unacceptable UX for a solo founder. OpenBao's 3-node HA + Raft
footprint is also inappropriate for a single-node, single-app
environment.

At the same time, principle 1.8 of the spec ("Enterprise practices
must not block solo-tier adoption") requires that we ship a secret-
management story that works on Tier 1 and migrates cleanly to Tier 2.

## Decision

Tier 1 uses **Bitnami SealedSecrets** as the default secrets backend.
Developers commit `kind: SealedSecret` manifests; the public
encryption key lives in Git and the in-cluster controller decrypts
them at apply time into Kubernetes Secrets. From Tier 2 upwards,
OpenBao is the default (see ADR 0006).

`platform-cli upgrade-tier` (1 → 2) includes a step that imports
existing SealedSecrets into OpenBao kv-v2 and rewrites Application
manifests to use OpenBao paths. The migration is non-destructive and
one-time.

## Amendment — `apprafter secret` is an operator toolkit, not a developer one (2026-08-30)

The Decision above says "**developers** commit `kind: SealedSecret` manifests"
and the Consequences say "Application authors see a single `secret(...)` API
regardless of which backend resolves it". Both frame the Tier-1 backend as
developer-facing. That framing is withdrawn.

**`apprafter secret seal` / `remove` are an operator surface, available at
Tier 1 only, and are never presented as a developer workflow.** The
`secret: "<name>/<key>"` reference in a manifest stays exactly as it is — that
half of the original consequence holds, and is the part that makes the Tier-2
migration mechanical. What changes is who runs the sealing command and where it
is documented.

### Why

This ADR already names the reason in its own Negative consequences:
SealedSecrets "does not provide dynamic credentials, automatic rotation, or
fine-grained ACL". Tier 1 deliberately ships a **primitive** in place of the
real thing, because a KMS-less single node cannot carry OpenBao's unseal UX or
its footprint. A primitive chosen for that reason cannot then be asked to carry
an authorization story. Attempting one produces the worst outcome: a control
that looks like a boundary and is not.

The concrete case that forced this. `ApplicationMigrationStrategy` classifies
`env-secret-ref-retarget` — repointing an environment variable at a *different*
secret — as `security-boundary`, the most severe class the app-scope detector
has, gated behind a MigrationPlan and an explicit approval. Re-sealing that same
secret with different contents reaches the identical outcome with no gate, no
classification and no record. Read as a developer-facing platform, that is an
authorization hole. Read correctly — an operator holding the cluster's sealing
key, on a single-node tier — there is no boundary being crossed, because the
actor already holds every credential in the cluster.

Role separation at Tier 1 is the operator's own to enforce, through who holds a
kubeconfig that can create SealedSecrets in a namespace. Where operator and
developer are the same person, which is the tier's design centre, there is
nothing to gate. Where a team does separate them on Tier 1, Kubernetes RBAC is
the mechanism, not a gate inside `apprafter secret`.

### What this obliges us to do instead

Not building an authorization story does not mean silence. Three deliverables,
in order:

1. **Say it.** Every surface that presents `apprafter secret` states that it is
   a Tier-1 operator tool standing in for OpenBao, that it carries no
   fine-grained access control or audit, and that Tier 2+ replaces it. The
   Backstage banner this ADR already promised ("a visible banner explaining
   this and prompting the migration as the user approaches Tier 2") is the
   Tier-2 half of the same obligation and is still unshipped — it is deferred
   with the rest of the portal, so the CLI and the documentation carry it until
   then.
2. **Move the documentation.** `docs/dev-guide/secrets.md` is in the wrong
   guide and is written to the wrong reader ("your manifest, your repository").
   The developer-facing half — binding an env var to `secret: "<name>/<key>"`
   and reading `EnvSecretMissing` — stays in the developer guide; the sealing
   workflow belongs to the operator guide.
3. **Make the blast radius visible**, which is an operations obligation rather
   than a security one: which applications resolve this secret, and which of
   them are still running an older revision. See D6 and D14 in
   `docs/measurements/day2-followups.md`.

### What follows at Tier 2+

With OpenBao's revisions and the portal in place, the platform can watch a
secret's revision and **notify** the owners of dependent applications, offering
a restart rather than performing one. An opt-in knob — per application in the
manifest, or cluster-wide, on the shape of `autoRestartAppsOnEnvChanges` — is
the right place for automation, because by then the two things that make it
unsafe today are gone: revisions are observable, and the identity performing the
change is authenticated and audited. Automatic restart is explicitly **not** the
Tier-1 default; see D6 for why an unknowable blast radius makes it unsafe there.

## Consequences

Positive:

- Tier 1 has zero unseal operations and no extra daemon footprint.
- ~80% of OpenBao's value (encrypted secrets in Git) at ~5% of the
  operational cost.
- Migration to OpenBao on tier upgrade is mechanical.
- Application authors see a single `secret(...)` API regardless of
  which backend resolves it.

Negative:

- SealedSecrets does not provide dynamic credentials, automatic
  rotation, or fine-grained ACL. The Backstage UI carries a visible
  banner explaining this and prompting the migration as the user
  approaches Tier 2.
- Two backends increase documentation surface. Acceptable: the
  difference is invisible to Application authors.

## Alternatives considered

- **OpenBao at Tier 1.** Rejected because of unseal UX and resource
  footprint.
- **Plain Kubernetes Secrets in Git.** Rejected because of plaintext
  exposure.
- **Mozilla SOPS at Tier 1.** Viable but more moving parts than
  SealedSecrets for the same outcome on a single node.

## Risks

- Migration during `upgrade-tier` is the riskiest step. Mitigated by
  pre-migration backup and post-migration content/hash verification.

## Owner

Secrets-platform maintainers.

## Re-evaluation

Revisit if a lightweight OpenBao deployment mode emerges that is
practical on a single VDS, or if SealedSecrets upstream stalls.

## References

- `spec.md` §1.8, §4.4, §8 ("Why SealedSecrets at Tier 1, not
  OpenBao").
- ADR 0006 (OpenBao at Tier 2+).
- <https://github.com/bitnami-labs/sealed-secrets>
