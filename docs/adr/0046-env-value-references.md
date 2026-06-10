# ADR 0046: `Application.env` value references — claim and secret sources

## Status

Accepted (2026-06-10). ADR-first for Phase 2.12. This is the last Phase-2
subphase; closing it triggers the `spec.md` §3 actualization and flips the
§6 M2 milestone box.

## Context

`Application.spec.base.env` currently accepts only **literal** string values
(`env: {DATABASE_URL: "postgres://…"}`). Phase 2.4e added an implicit
auto-injection: when an app declares `needs.pg`, the operator automatically
injects `DATABASE_URL` from the provisioned connection Secret. That stopgap
has two problems:

1. **It is opaque and inflexible.** The developer has no visibility into where
   the value comes from, cannot choose the variable name, and cannot reference
   individual components of the DSN (host, user, pass, db, port) that some
   libraries require separately.
2. **It cannot address externally-managed secrets.** For credentials that live
   outside the platform (a third-party API key, a shared DB not managed by a
   claim), there is no declaration mechanism — the user falls back to manual
   `kubectl` and Argo sync oddities.

The 2.4e collision guard (rejecting a literal `DATABASE_URL` under `needs.pg`)
is a direct symptom: the auto-injection occupies a name the user cannot reclaim.

The 2.6b named-multi-claim format (ADR 0043) introduced `(type, name)` claim
identity and CUE-validated decomposed connection-Secret keys (pg: `url user
pass host port db`; redis: `url host port user pass db channelPrefix`), giving
a rich per-type field vocabulary. The question 2.12 answers is: how does a
developer bind *any* of those fields, or an external secret, into a named env
var, without losing type safety or repo hygiene?

**Authoring requirements** (shaped by the user during design, 2026-06-09):
typed + structural, minimal brackets, claim refs cue-vet-validated against
declared needs, external secrets validated format-at-webhook / existence-at-runtime,
no boilerplate in the user's repo, local CLI validation that matches the cluster.

## Decision

### 1. Three-source `env` model with a structural discriminator

`env` values come from three sources, structurally distinguished in the
manifest:

```cue
env: {
    LOG_LEVEL:    "info"                     // literal — a quoted string
    DATABASE_URL: claim.pg.url               // claim ref — bare CUE selector
    DB_HOST:      claim.pg.host              // decomposed field
    DB_PASS:      claim.pg.pass
    MAIN_URL:     claim.pg.main.url          // named claim ("main")
    REDIS_URL:    claim.redis.url
    STRIPE_KEY:   secret: "stripe/api-key"   // secret ref — braceless a:b:c
}
```

**Authoring rule:** quoted string → literal; bare `claim.<type>.<field>` (or
`claim.<type>.<name>.<field>` for a named claim) → claim reference; `secret:
"<name>/<key>"` → external secret reference. No braces are required (the
secret form uses CUE's `a: b: c` ≡ `a: {b: c}` shorthand).

At the Kubernetes resource level the env value is one of:

```cue
#EnvValue: string | #EnvRef
#EnvRef:   {claim: string} | {secret: string}   // exactly one discriminator key
// claim payload:  "<type>.<field>"  or  "<type>.<name>.<field>"
// secret payload: "<name>/<key>"
```

Both ref forms are a `{discriminator: string}` — one Rust enum variant per
resolution path. The rendered Application CR carries the refs; the operator
renderer expands them into Kubernetes `EnvVar` / `EnvVar{valueFrom:
secretKeyRef}` entries.

This shape was chosen over sentinel strings (non-structural, no type safety)
and a polymorphic struct (requires user braces for every non-literal). It is
asymmetric in authoring (claim is a bare selector; secret is a key–value)
but symmetric at the CR level (both become `{discriminator: string}`).

### 2. Claim refs are cue-validated via a generated sibling file; no repo boilerplate

The bare selector `claim.pg.url` is a **CUE lexical reference**. It resolves
via enclosing scope, not via unification — a sibling field in the same struct
would not work (`#App & {env: {x: claim.pg.url}}` fails "reference claim not
found"). The working mechanism is a **generated sibling file** placed in the
same CUE package as the user's manifest, containing:

```cue
claim: _mkClaim & {_needs: app.spec.base.needs}
```

where `_mkClaim` is a CUE helper in the AppRafter schema that materialises
`claim.<type>.<field>` (and named variants) from the declared `needs`. A
reference to an **undeclared** need type or a **non-enum** field is a `cue`
error — the selector only resolves if the field exists in the generated
`claim` struct. This gives claim refs free cue-vet validation.

The generated file is a cue-cmp / CLI **runtime artifact**: it never lands in
the user's git repository. The cue-cmp writes it into Argo CD repo-server's
ephemeral checkout, evaluates, emits stdout, and the workspace is discarded.
The CLI's `apprafter app validate` writes the same file into a temp dir for
local validation. A stdin/overlay injection is an option for the future; the
temp-file-in-ephemeral-workspace approach is the simpler default.

For **per-env** cases the cue-cmp binds `claim` to the **effective** needs
of the active environment (`base.needs` ⊗ `environments[<env>].needs`,
override-wins — the same merge the operator's `effective_spec` uses). The env
is known at cue-cmp render time via `APPRAFTER_APP_ENV` (2.9 mechanism).

This mechanism was de-risked on real `cue` (2026-06-09): a scratch package
confirmed that needs → comprehension materialises all expected fields, that
an undeclared need / non-enum field is a `cue` error, and that the
braceless `secret: "name/key"` form exports `{secret: "name/key"}` and
matches the union.

### 3. Decomposed connection-Secret keys (provisioner)

The pg and redis provisioners gain decomposed keys in the connection Secret
alongside the composed `url` DSN:

- **pg** (`connection_secret_object`): keys `url`, `user`, `pass`, `host`,
  `port`, `db`. The old `DATABASE_URL` key is dropped — its only consumer was
  the 2.4e auto-inject (removed in Decision #5).
- **redis** (`redis_connection_secret_object`): keys `url`, `host`, `port`,
  `user`, `pass`, `db`, `channelPrefix`. The old `REDIS_URL` and
  `REDIS_CHANNEL_PREFIX` keys are dropped. `acl_reconcile` is refactored to
  read the `pass` key directly (removing the only special reader of `REDIS_URL`
  that previously parsed the password out of the DSN).

These are the **canonical** and **only** keys. The renderer resolves
`claim.pg.user` → `secretKeyRef{key: "user"}`, `claim.redis.channelPrefix` →
`secretKeyRef{key: "channelPrefix"}`, etc.

### 4. Layered validation

Claim refs and secret refs are validated at different layers appropriate to
what each layer can observe:

- **`cue vet` (at validate/render time):** claim refs — undeclared need type,
  non-enum field, named ref on a scalar need, unnamed ref on a multi-entry
  need with no unnamed default → `cue` error. The validation is structural and
  free: the bare selector does not resolve if the field is absent.
- **Admission webhook (at `kubectl apply` / `app add`):** claim refs — `<type>`
  ∈ declared needs (effective base + env), `<field>` ∈ the type's enum (pg/redis
  field sets; `disk` has no connection Secret and is rejected), named ref
  matches a declared entry. Secret refs — format: parseable `name/key`; `name`
  is DNS-1123; `key` matches `[-._a-zA-Z0-9]+`. **No existence check** for
  secrets at admission (secrets are external and may not exist yet).
- **Runtime (at reconcile, after the claim gate):** after the 2.4d
  `AwaitingResourceClaim` gate guarantees all claim Secrets exist, the operator
  verifies every `secret` ref resolves to an existing Secret+key in the app
  namespace. If not, `Application` is set `Ready=False` with a clear reason
  (e.g. `env STRIPE_KEY → secret "stripe/api-key": Secret "stripe" not found
  in namespace demo`). `optional: false` on the rendered `secretKeyRef` is the
  second layer — the pod will not start if the Secret is absent. Claim refs
  cannot hit this path (gated ready before render).

### 5. 2.4e auto-injection removed

The implicit `DATABASE_URL` injection for `needs.pg` (phase 2.4e) is a
stopgap until 2.12 — it is **removed**. An app receives a DSN only by
explicitly referencing `claim.pg.url`. The 2.4e collision guard (rejecting a
literal `DATABASE_URL` when `needs.pg` is declared) is also removed: with no
auto-injection there is no collision, and the user owns every env-var name.
Any env key may carry a literal, a claim ref, or a secret ref.

Downstream: `e2e/needs-pg-walk.sh` and the `examples/` are updated to use
explicit `claim.pg.url` (or decomposed fields). Any app currently relying on
auto-injected `DATABASE_URL` must add an explicit `DATABASE_URL: claim.pg.url`
(or rename it) before upgrading.

### 6. Local validation via `apprafter app validate`

Because the generated `claim` binding exists only in the ephemeral workspace
(Decision #2), a bare `cue vet` on the user's manifest cannot resolve `claim`.
Local pre-commit validation is provided by a new CLI command:

```
apprafter app validate [manifest]
```

The CLI runs the same claim-injection + schema + `cue vet` pipeline in a temp
dir and reports errors (undeclared need, non-enum field, bad secret format).
**Manifest discovery** (no argument): defaults to `<cwd>/apprafter/Application.cue`
(the scaffold convention), or a single `*.cue` in the cwd; if none found,
requires an explicit path. An explicit path always overrides the default.

The `_mkClaim` helper is the single source of truth shared by the cue-cmp and
the CLI — both emit the same one-line `claim:` binding over it.

### 7. Schema injection at render/validate time; scaffold stops vendoring

Today `apprafter app scaffold` vendors the AppRafter CUE schema into
`<repo>/apprafter/cue.mod/pkg/apprafter.io/schemas/v1alpha1/` and the cue-cmp
resolves `import "apprafter.io/schemas/v1alpha1"` through the user's vendored
copy. The 2.12 generated `claim` binding references the new `_mkClaim` helper —
a stale pre-2.12 vendored schema (missing `_mkClaim`) would break claim refs
at render.

The fix: the cue-cmp (at render) and the CLI (at validate) lay down the
**current** schema — matching their own version — into the ephemeral workspace,
alongside the `claim` binding and `cue.mod`. The user's repo no longer needs a
vendored schema: `apprafter app scaffold` stops writing `cue.mod/pkg/` (and
the `cue.mod/` directory entirely, injected at render/validate time instead),
leaving only `apprafter/Application.cue` in the user's repository.

**Wins:** eliminates schema-version drift (vendored copy vs cluster version)
and removes repo clutter. **Migration:** existing scaffolded repos carry a
vendored `cue.mod/` — the cue-cmp overrides/ignores it (inject-wins) so old
repos keep rendering without changes. `apprafter app scaffold` (re-run) removes
the vendored directory.

### 8. Renderer resolution (pure function, no I/O)

The renderer receives the resolved `(type, name) → connectionSecretRef` map
(the `needs_secrets` map 2.4e already builds, extended to all network needs and
named claims). For each `env` entry:

- **Literal** → `EnvVar { name, value }`.
- **`{claim: "<type>.<field>"}` / `{claim: "<type>.<name>.<field>"}`** →
  `EnvVar { name, valueFrom: secretKeyRef { name: <connectionSecretRef>, key: <field>, optional: false } }`.
- **`{secret: "<name>/<key>"}`** → `EnvVar { name, valueFrom: secretKeyRef { name: <name>, key: <key>, optional: false } }`.

Deterministic ordering (env keys sorted) for byte-stable Deployments (SSA
no-op), consistent with 2.4e. The renderer remains a pure function — no I/O;
the connection-secret refs come from the already-gated ready claims.

The Rust `EnvValue` type uses `#[serde(untagged)]` for the literal-vs-object
distinction and key-discriminated (lowercase) fields for claim-vs-secret:

```rust
#[serde(untagged)]
enum EnvValue { Literal(String), Ref(EnvRef) }
#[serde(rename_all = "lowercase")]
enum EnvRef { Claim(String), Secret(String) }
```

The exact derive shape is settled during implementation.

## Consequences

- **2.4e auto-injection removed:** apps using the implicit `DATABASE_URL` must
  add an explicit `DATABASE_URL: claim.pg.url` before upgrading — a one-line
  manifest change, but a **migration** (communicated at upgrade time). The
  collision guard is removed: a literal `DATABASE_URL` under `needs.pg` is now
  valid.
- **Full claim field vocabulary:** developers can reference any component of a
  connection Secret by name (host, user, pass, port, db, channelPrefix) in any
  env var they choose, or the composed DSN (`url`). Named claims extend this
  to `claim.pg.main.url` etc.
- **External secrets are first-class:** `secret: "name/key"` brings third-party
  credentials into the manifest without manual `kubectl` — webhook-validated on
  format, runtime-gated on existence with a clear `NotReady` reason and message.
- **cue-cmp and CLI gain schema-injection machinery:** both must lay down the
  current schema + `claim` binding into an ephemeral workspace. This is new
  cue-cmp complexity but eliminates the schema-drift failure mode.
- **Claim refs are cue-vet-validated, secret refs are not:** a claim ref to an
  undeclared need or non-enum field is a `cue` error at `apprafter app validate`
  and at render; a secret ref is webhook-format-checked + runtime-existence-checked
  — the asymmetry is inherent (secrets are external, claims are declared).
- **CRD is additive:** the `env` value node carries
  `x-kubernetes-preserve-unknown-fields: true` (string-or-object — the same
  pattern `needs` uses for OneOrMany). Existing literal-only `env` maps validate
  unchanged. `just crd-validate` is mandatory before release.
- **Coordinated release:** operator + admission-webhook + argocd-cue-cmp +
  CLI + platform-stack bump (CRD field + webhook rules change; cue-cmp schema
  injection). Old scaffolded repos with vendored `cue.mod/` keep rendering
  (inject-wins); existing apps with no `claim.*` refs are unaffected.
- **`pg`/`redis` only at launch:** `claim.<type>.*` is gated to types with
  connection Secrets at launch (pg and redis). Jetstream, clickhouse, s3, and
  notifications reject `claim.*` at the webhook until their provisioners write
  connection Secrets — the grammar is ready, the backend is deferred.

## Alternatives considered

- **Sentinel strings (`"claim:pg.url"`).** No structural type safety; the value
  is opaque to CUE and the renderer must detect and parse a magic prefix.
  Rejected: the typed `{claim: string}` / `{secret: string}` union is
  structurally checkable at every layer.
- **Polymorphic union struct (`#EnvValue: string | {claim: {type, field}} | …`).** 
  More explicit, but requires braces in the manifest (`DATABASE_URL: {claim: {type: "pg", field: "url"}}`).
  Rejected: the user explicitly steered to minimal brackets; the bare-selector +
  braceless-secret authoring achieves the same structural discrimination with
  less syntax.
- **Auto-injection retained alongside explicit refs.** Keeps backward-compat
  but perpetuates the opacity and collision problems 2.12 is designed to fix.
  Rejected: the explicit-ref model is cleaner and the one-line migration cost
  is acceptable.
- **Raw `cue vet` on the user's repo (no sibling file).** Would require the user
  to vendor the generated `claim` struct — repo boilerplate and a second drift
  vector. Rejected: the generated-sibling-file mechanism (Decision #2) keeps the
  user's repo clean and pushes the boilerplate into the ephemeral workspace.

## Risks

- **Migration friction for 2.4e users.** Any app relying on auto-injected
  `DATABASE_URL` breaks silently until the manifest is updated. Mitigation:
  upgrade note + `apprafter app validate` surfaces the missing `claim.pg.url`
  (or equivalent) before the operator renders it.
- **cue-cmp schema injection:** laying the current schema into the ephemeral
  workspace must override an existing vendored `cue.mod/` without corrupting
  the package graph. Mitigation: prototype in the cue-cmp test harness before
  release; assert via the 2.9 `test-inject.sh` pattern extended for `_mkClaim`.
  The 2.9 lesson applies here too: exercise the real injection path, not just a
  host-side test.
- **CRD structural schema (`x-kubernetes-preserve-unknown-fields`).** The
  `additionalProperties`+`properties` mutual-exclusion bug (2.4h regression)
  passed every gate except the live apiserver. Mitigation: mandatory
  `just crd-validate` before release (kind via podman, the established gate).
- **Named claim grammar ambiguity.** `claim.pg.url` (type=pg, field=url) vs
  `claim.pg.main.url` (type=pg, name=main, field=url) — a two-vs-three-segment
  payload. The webhook and renderer parse the dotted payload; validation must
  reject a three-segment ref where the type is scalar (no named entries).
- **`_mkClaim` shared-helper maintenance.** The cue-cmp and CLI must track the
  same helper version; a divergence silently gives different validation results
  locally vs at render. Mitigation: single source in the platform schema,
  both tools inject the schema they ship with.

## Owner

Platform / operator. Implementation: Phase 2.12 (`plan.md` §2.12).

## References

- Design: `docs/superpowers/specs/2026-06-09-2.12-env-references-design.md`.
- ADR 0042 (needs.redis / Dragonfly — connection Secret shape).
- ADR 0043 (needs.disk / named multi-claim — `(type, name)` identity and
  named `claim.<type>.<name>.<field>` grammar).
- ADR 0044 (per-environment deploy — `effective_spec`, per-env needs merge).
- ADR 0029 (CUE CMP — schema injection, ephemeral workspace).
- `plan.md` §2.12 (`Application.env` value references).
