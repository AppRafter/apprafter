# ADR 0047: CRD codegen — CUE as the single source; generated CRD, gated Rust, typed webhook

## Status

`Accepted` (2026-06-10).

This is a **tech-debt refactor**, not a product-track subphase: it carries
no `plan.md` SR marker and no `spec.md` §6 milestone. ADR-first per CLAUDE.md
("open a new ADR before significant architecture moves"). On acceptance it
actualizes three current-state notes: the aspirational generation claim in
**ADR 0004** ("a single CUE module … generates OpenAPI v3 CRD definitions"); the
**spec.md Appendix A** layout note, which is additionally *stale* — it names the
removed `cli-providers::k8s::application_crd` (dropped in B.1.71; the CRD now
ships only from the operator chart); and the CLAUDE.md architecture note ("There
is no CUE→CRD/Rust generator yet … hand-rolled mirrors, kept in sync by hand").
The same landing pass fixes both the "no generator yet" wording and the stale
`application_crd` reference. It does not move any §6 box. The Phase-0 Application
spike (2026-06-10) cleared the R1 gate: a CUE-generated Application CRD reached
`Established` on a real apiserver and accepted the union-bearing objects.

## Context

Each logical CRD is mirrored across up to five hand-maintained
representations:

1. the CUE schema (`schemas/v1alpha1/<crd>.cue`),
2. a `cue vet` example (`examples/…`),
3. the OpenAPI v3 CRD YAML in the operator chart
   (`operator/charts/apprafter-operator/templates/crd-<crd>.yaml`),
4. the kube-rs Rust type (`operator/operator-core/src/<crd>.rs`),
5. the admission-webhook validator (`operator/admission-webhook/src/…`).

Twelve CRDs are *defined in CUE*; seven are *implemented* today (Application,
ServiceProvider, ResourceClaim, RetainedClaim, MigrationPlan, SourceCredential
carry all five mirrors; PlatformStack carries four — no example). The M2 tail
and Phase 3+ push the *implemented* count toward 10+. The per-schema-change tax
is linear in the number of mirrors, and the mirrors drift independently.

Mirror #3 (the hand-rolled CRD YAML) is both the most drift-prone and the one
with a track record: release `0.2.15` shipped an `additionalProperties` +
`properties` CRD that the apiserver rejects structurally — `helm lint`,
`cargo`, and `cue vet` all passed it, and only the nightly e2e caught it after
publish. That class of bug is a direct cost of hand-rolling OpenAPI v3 by hand.

Mirror #5 carries a quieter drift path of its own: the admission webhook reads
the spec as an untyped `serde_json::Value` by string key (`validator.rs`), so a
renamed field silently stops matching — the rule just stops firing, with no
compile-time signal, and inline test fixtures that may not notice.

ADR 0004 already ratified CUE as the schema source of truth that "generates
OpenAPI v3 CRD definitions," and spec §1.1 commits to "one config language:
CUE." No generator exists, so the claim is aspirational and the mirrors are
kept in sync by hand.

Two forces shape the direction:

- **Asymmetry of tooling.** The Rust types already derive
  `kube::CustomResource` + `schemars::JsonSchema`, so `CustomResourceExt::crd()`
  emits a CRD in one call — Rust→CRD is nearly free. The reverse, CUE→Rust, has
  no clean generator: the only path is `cue export` → JSON Schema → `typify`,
  which degrades the hand-tuned types (the untagged `EnvValue` union, the
  `OneOrMany` scalar|array fields) the operator depends on.
- **CUE is the most user-facing structure.** Users read and write CUE; the CUE
  schema is the documented, published API surface that examples are vetted
  against. Divergence between the CUE a user sees and the contract the apiserver
  actually enforces is a schema that lies to the user. Minimizing that
  divergence is the primary goal.

## Decision

We will make **CUE the single source of truth** for every CRD's schema,
generate the drift-prone mirror from it, and bind every remaining hand-written
mirror so that none can silently drift from the CUE.

1. **CUE owns the schema *and* the CRD envelope.** Each
   `schemas/v1alpha1/<crd>.cue` carries, alongside its `#Type`, the CRD-envelope
   metadata (group/version/kind/names, scope, shortNames, additionalPrinterColumns,
   subresources). The Phase-0 spike fixed the encoding: a hidden `_crdMeta` field,
   which `cue export -e '_crdMeta'` surfaces. The envelope must not remain a
   hand-rolled YAML mirror, or the refactor defeats itself.

2. **Generate the chart CRDs from CUE.** A small Rust tool, `crdgen`, in the
   operator workspace runs `cue export ./schemas/v1alpha1 --out openapi`, takes
   the CR's component schema, and post-processes it into a Kubernetes *structural*
   schema: inline every `$ref` (structural schemas forbid references), collapse
   each `oneOf`/`anyOf` union node (the untagged `EnvValue`, the `OneOrMany` needs
   shapes) to `x-kubernetes-preserve-unknown-fields: true` (the webhook validates
   the union), and give each object node a `type`. CRD-only constraints CUE
   deliberately keeps out of `cue vet` — the `image` `pattern: "^.+$"`, for one —
   are carried by `@crd(...)` field attributes the generator reads and emits. It
   then wraps the schema in the CRD envelope from `_crdMeta`, and writes
   `operator/charts/apprafter-operator/templates/crd-<crd>.yaml` with a
   "GENERATED — do not edit" header. `just gen-crds` regenerates; the files are
   committed so chart consumers and code review still see a diff. CUE remains the
   source — the generator written in Rust is only the transform engine, exactly
   as `cli-providers/build.rs` already shells out to `cue export`.

3. **Rust types stay hand-written, behind a local-first drift-gate.** The
   kube-rs types keep their ergonomic shape. `crdgen check`, surfaced as
   `just crd-check` (a linter-style local target, mirrored in CI and optionally
   in the lefthook pre-commit hook), asserts two things:
   - **CUE ↔ committed**: regenerating from CUE reproduces the committed
     `crd-<crd>.yaml` byte-for-byte (catches "edited CUE, forgot to regen" and
     "hand-edited a generated file").
   - **Rust ↔ CUE**: the CRD derived from the kube-rs type
     (`CustomResourceExt::crd()`) is semantically equivalent to the CUE-derived
     CRD, modulo an explicit allowlist of intentional **CRD-vs-CRD** deltas —
     cases where the kube-rs derivation and the CUE export legitimately differ,
     e.g. a field the Rust type makes required (it is not `Option<T>`) that CUE
     leaves optional. Each allowlist entry needs a one-line reason. Cross-field
     invariants the webhook adds beyond *either* CRD (the `image`-reachable
     rule, env-key patterns) are Decision #4 — they are not schema deltas and
     never enter this comparison.

4. **Webhook validators stay hand-written but become typed.** They encode
   cross-field invariants OpenAPI v3 cannot express (e.g. `image` reachable via
   `base.image` or every `environments[*].image`), so they are out of
   *generation* scope. Today they read the spec as an untyped `serde_json::Value`
   by string key, so a renamed field silently bypasses a rule. Each validator is
   refactored to deserialize the admission object into the `operator-core` typed
   structs (#4) and read typed fields — sound because a *validating* webhook runs
   only after the apiserver's structural validation, so the object already
   conforms to the generated CRD and always deserializes. The compiler then gates
   field references: a rename that is not propagated fails to build. The
   cross-field *rules* stay hand-written and unit-tested (there is nothing in CUE
   to gate them against — that is why the webhook exists). This binds #5
   transitively to CUE through #4's gate; it is not part of the CRD-vs-CRD gate.

5. **Scope and migration.** The seven implemented CRDs are in scope. Application
   migrates first as a feasibility spike (it is the hardest: ~205-line CRD, the
   untagged `EnvValue` union, `OneOrMany`, and a ~2165-line validator). Each CRD
   then migrates as a **behavior-neutral commit** that (a) replaces its
   hand-rolled CRD YAML with the CUE-generated one — structurally equivalent, the
   apiserver accepts the same objects — and (b) retypes its webhook validator
   onto the `operator-core` structs, leaving the cross-field rules and their
   tests unchanged. The five schema-only CRDs are out of scope until they grow a
   Rust type; they adopt the generator from day one.

## Consequences

Positive:

- **Every representation is held machine-consistent with CUE.** #3 (CRD YAML) is
  generated, so it cannot drift; #4 (Rust) is held by `just crd-check`
  (CUE↔Rust); #2 (examples) are already held by `cue vet` — an example cannot
  contradict the schema; #5 (webhooks) become typed, so the compiler rejects a
  stale field reference. The cross-field webhook *rules* — the one thing with no
  schema to check against — remain under unit tests.
- The most drift-prone mirror — the hand-rolled CRD YAML (#3) — is additionally
  *eliminated*; the `0.2.15`-class structural bug becomes unrepresentable.
- The per-change edit count does not collapse to one: a typical field add still
  edits CUE (#1) and the Rust type (#4), then regenerates, and a rename also
  touches the typed validator. The win is that every desync now fails a *local*
  check (`cue vet` / `just crd-check` / `cargo build`), never a post-publish e2e.
- ADR 0004's generation claim becomes true; spec §1.1's "one config language" is
  honoured for the CRD surface; the user-facing CUE is provably equal to the
  contract the apiserver enforces.

Negative:

- New build tooling to own: the `crdgen` binary and its OpenAPI→structural
  post-processor.
- A CUE convention for CRD-envelope metadata that contributors must learn.
- The gate's allowlist must be curated honestly or it hides real drift.
- Retyping the seven webhook validators (Application's is ~2165 lines) is real
  work and can subtly shift validation behaviour; each retype rides its CRD's
  migration behind the existing validator unit tests.

Neutral:

- Generated CRDs are committed, so they stay diff-reviewable.
- CUE is still consumed by a Rust tool; this does not make Rust the source —
  the source is the CUE files, edited by humans.

## Alternatives considered

- **Rust as the source (kube-rs-native crdgen).** Cheapest: `CustomResourceExt`
  already emits the CRD; a `schemars`→CUE step could regenerate the CUE view.
  Rejected: it inverts ADR 0004 and spec §1.1 and demotes the most user-facing
  structure (CUE) to a generated artifact — the opposite of "minimize divergence
  the user sees."
- **Full CUE→Rust generation (typify).** Generate the Rust spec structs from
  CUE too. Rejected: `typify` degrades the hand-tuned typed unions
  (`EnvValue`, `OneOrMany`) and is the heaviest path, for the least user-facing
  benefit (the Rust type is internal).
- **CUE→CRD only, no Rust gate.** Smaller. Rejected: it leaves internal
  Rust↔CUE drift unguarded, and a local linter-style gate was explicitly wanted;
  the gate is cheap relative to the generator it rides alongside.
- **Pure `cue cmd` generator (no Rust).** Keeps everything in cuelang. Rejected:
  the structural-schema post-processing and envelope assembly are awkward in cue
  scripting, and the Rust gate needs `CustomResourceExt::crd()` regardless — one
  Rust binary does both sides.

## Risks

- **R1 — `cue export --out openapi` is not 1:1 with a Kubernetes structural
  schema** (int-or-string, `preserve-unknown-fields` for `OneOrMany`/untagged
  unions, unsupported constructs, the envelope metadata's CUE encoding).
  *Mitigation:* the Application spike must produce an apiserver-valid CRD
  structurally equivalent to the current hand-rolled one **before** the rest are
  migrated; CUE field attributes drive the `x-kubernetes-*` injection. If the
  spike cannot reach equivalence, the decision is revisited.
- **R2 — migration silently changes a CRD's accepted-object set.**
  *Mitigation:* per-CRD structural-equivalence assertion against the
  pre-migration hand-rolled CRD, plus `just crd-validate` (ephemeral kind
  apiserver Establishes-check); the first landing for each CRD is behavior-neutral.
- **R3 — the gate allowlist becomes a dumping ground that masks real drift.**
  *Mitigation:* every entry carries a one-line reason; the gate fails on an
  unexplained entry (the same discipline as `yankedReason`).
- **R4 — cue version drift changes the OpenAPI output** (the spike saw cue
  v0.16 locally vs v0.10 pinned in CI). *Mitigation:* the generator pins **one**
  cue version via nix, and CI's `setup-cue` + the gate use that same version, so
  the byte-compare is reproducible; the CUE↔committed gate catches any drift.
- **R5 — retyping a webhook validator subtly changes its behaviour** (a rule
  that leaned on a permissive `Value` lookup, an absent-field default, or a
  string-parse quirk). *Mitigation:* the existing validator unit tests are the
  guard — each retype is behaviour-neutral against them; the deserialize is
  total because a validating webhook runs only after structural validation, so
  the object already conforms to the generated CRD. Where a typed field cannot
  represent a previously-accepted input, that surfaces as a failing test, not a
  silent change.

## Owner

Andrey Ryahovskiy.

## Re-evaluation

Revisit if `cue`'s OpenAPI output cannot express a future CRD construct, or if
maintaining `crdgen` + the post-processor costs more than the hand-rolling it
replaces.

## References

- ADR 0004 — CUE over Pkl (the aspirational "generates OpenAPI v3 CRD
  definitions" claim this ADR fulfils).
- ADR 0003 — Rust operator over Crossplane (kube-rs `CustomResource`).
- spec.md §1.1 — "one way to do things" / one config language: CUE.
- spec.md Appendix A (repo-layout notes) — the canonical current-state note on
  hand-rolled CRD/Rust mirrors; currently **stale** (it names the removed
  `cli-providers::k8s::application_crd`) and still asserts "no CUE→CRD/Rust
  generator yet" — this ADR's landing rewrites it.
- CLAUDE.md — architecture note on hand-rolled CRD/Rust mirrors.
- The `0.2.15` `additionalProperties` + `properties` incident
  (`docs/changelog/plan-history.md`) — the structural CRD bug class this removes.
- `kube::CustomResourceExt`, `cue export --out openapi`.
