<!-- SPDX-License-Identifier: FSL-1.1-Apache-2.0 -->
# ADR 0056: machine-picker — live (region × SKU) matrix and no implicit server-type default

## Status

`Accepted` (2026-08-12).

ADR for subphases 2.16h and 2.16h-a (`plan.md` §2.16h). Records the
design decisions for the interactive machine-picker matrix and the
removal of the implicit `cpx22` server-type default. Ships as a
CLI-only change (cli-providers + cli-core + cli-state + platform-cli;
no operator, schema, cue-cmp, or platform-stack change).

## Context

Before this ADR, the `apprafter` CLI had no interactive path for
choosing a server type. `DEFAULT_SERVER_TYPE = "cpx22"` was the
hard-coded fallback (added in v0.1.42 when Hetzner retired `cx22`),
overridable only through a CUE Infrastructure manifest
(`APPRAFTER_MANIFEST` → `nodes[0].kind`). There was no `--server-type`
flag, no target-store field, and no environment variable.

The region picker existed (`target add` wizard, phase A.4b), but it
operated as an independent `Select` — no visibility into which server
types are available in the chosen region. Two independent dropdowns
assemble region/SKU pairs that the validator then rejects post-hoc with
a confusing error. Server-type availability is a region-dependent
property, so the correct model is a single `(region × SKU)` crossed set.

A secondary problem: provisioning a server is a spending decision. An
implicit default silently commits the operator to a specific instance
class across all `up` / `apply` invocations where the type is not
explicitly set. This is the same class of invisible-configuration hazard
that the v0.1.77 target-store work fixed for credentials — it now fires
for machine type.

## Decision

### (a) Single endpoint — `GET /v1/server_types`

Catalog, per-region availability, and prices all come from a single
paginated `GET /v1/server_types` call. Each server type carries a
`locations[]` array (`{id, name, available, recommended, deprecation}`)
and a `prices[]` array (`{location, price_monthly.{net,gross},
price_hourly.{net,gross}}`).

`GET /v1/datacenters` is **rejected**: that endpoint was deprecated
on 2026-06-02 and returns `410 Gone` after 2026-10-01. There is no
separate `/v1/availability` endpoint. The single `list_server_types()`
paginated call is the entire data source; no `list_datacenters()` call
is added.

### (b) The picker is one `(region × SKU)` crossed matrix

Region availability is SKU-dependent. Two independent `Select` boxes
allow the user to assemble pairs that are structurally invalid (a type
not offered in the chosen region). The picker is a single `inquire`
`Select` over rows of type `MachineRow { offer, latency_ms }`, where
each offer encodes both axes. Selecting a row writes **both** region and
server type into the active target in one atomic step.

### (c) `UnavailableKind` split — structural before selection, transient at provision

```
Unknown              — SKU not found in the catalog at all.
NotOfferedInRegion   — SKU exists but has no entry in locations[] for that region.
Retired              — unavailable_after <= now (at selection time).
OutOfCapacity        — available == false in locations[], but not retired.
```

`Unknown`, `NotOfferedInRegion`, and `Retired` are **structural** errors
resolved before the user makes a selection (invalid explicit
`--server-type` / `--region` pair → typed error + alternatives, picker
never shown). `OutOfCapacity` is **transient** — the picker hides
sold-out rows by default (toggle reveals them, non-selectable), and a
sold-out event at provision time produces a `ServerTypeUnavailable {
kind: OutOfCapacity }` error pointing the operator at retrying or
choosing another type.

A **future deprecation date** (a row whose `unavailable_after` is in the
future) is **not** an error. The row remains selectable, rendered with a
`!` badge, so the operator can plan ahead. The retired cutover (`<=
now`) is what triggers the structural exclusion.

### (d) No implicit server-type default — Decision 0 (BREAKING)

**We do not provision a machine type the operator has not chosen.**
Provisioning is a spending decision and the silent `cpx22` default
inverted that: an operator who never saw the default still paid for it.

`DEFAULT_SERVER_TYPE` is removed. On the create path
(`needs_server_create == true`), if the type resolves to nothing through
the full chain below, the CLI errors with `ServerTypeNotSelected` — a
typed, stable diagnostic code — before any API call is made. No resource
is created.

On the **reconcile path** (`needs_server_create == false` — the machine
already exists), the type is **not required**; `apply` / `up` reconcile
firewall, network, and kubeconfig without it. A post-upgrade first
`apply` on a live cluster succeeds and backfills the type from the live
server (see "self-heal").

**Resolution precedence — identical shape for both axes:**

| Rung | server_type | region |
|------|-------------|--------|
| 1 — flag | `--server-type` | `--region` |
| 2 — manifest | `nodes[0].kind` | `spec.region` |
| 3 — state (fact) | `HetznerCloudState.server_type` | `State.region` |
| 4 — target (pref) | `TargetConfig.server_type` | `TargetConfig.region` |
| 5 — env (escape) | `APPRAFTER_SERVER_TYPE` | — |
| 6 — default | **none → `ServerTypeNotSelected`** | `nbg1` |

The env rung (`APPRAFTER_SERVER_TYPE`) is a **distinct low rung** — it
is NOT bound to the clap flag — so a stray shell environment variable
cannot silently override a committed IaC manifest or an explicit flag.
Region retains its `nbg1` default (not a spend decision; removing it
would double the breaking surface for existing scripts).

### (e) Sort via a pre-picker `Select`, not a `sort:` filter token

Sort order is chosen through an initial `Select` prompt (default:
`Latency ↑`; options: `Price ↑ / Cores ↓ / RAM ↓ / Disk ↓ /
Location`). The `inquire` scorer callback can filter and rank but cannot
re-sort live as the user types, so a discoverability-first `Select`
makes the available sort axes explicit before the matrix appears.

### (f) `state.json` records the created type as a FACT

`HetznerCloudState.server_type: Option<String>` is written on the
CREATE path alongside `server_id`. This is the **recorded fact** of
what was actually provisioned — distinct from `TargetConfig.server_type`
(the preference / intent). The fact enables:

- **Reproduction:** a `destroy` → `restore --reprovision` on the same
  target re-provisions the same type (state gone after destroy, but
  `TargetConfig` was backfilled at provision time, so the type survives
  via rung 4).
- **Legacy self-heal:** an existing target whose state pre-dates this
  field has `server_type: None`; on the first `apply` after upgrade the
  reconcile path (not the create path) detects this, reads the live
  server pinned by `HetznerCloudState.server_id`, and backfills **both
  stores** (state = the fact; `TargetConfig` = the adopted preference).
- **Drift detection:** if the recorded fact diverges from the live
  server, an out-of-band machine change has occurred (see §g).

`apprafter import` also fills `HetznerCloudState.server_type` from the
live server, but `import` is not the backfill path: `--dry-run` prints
only, and without `--force` it won't overwrite an existing
`state.hetzner_cloud`. The backfill on `apply` is what updates an old
state file in place.

### (g) Fact-drift guard only; no in-place resize (machine change = rebuild)

There is **no in-place machine resize**. Changing the machine of a
*running* cluster means a rebuild: `apprafter backup create` +
`apprafter restore --reprovision --server-type <sku>` (a fresh box of the
new type, data restored from the backup, brief downtime). Given that:

- `apply` on an existing server carries ONE guard — **fact drift**:
  `HetznerCloudState.server_type` (the recorded FACT) vs the live
  `server.server_type.name` (and `State.region` vs `server.location.name`).
  A mismatch means the machine was changed **outside AppRafter** (e.g. a
  resize through the Hetzner Console) → a **warning**.
- `apprafter target machine` **refuses on a provisioned target** (whose
  state carries a `server_id`), pointing at the backup + reprovision path
  above. It sets the type only on an **un-provisioned** target — you
  changed your mind before the first `apply`, or you are reusing a target
  after its cluster was destroyed (state wiped).

A provisioned target therefore cannot hold a type preference that differs
from its live machine, so there is no "deferred intent" to surface. An
earlier revision printed a second, info-level guard (target *preference*
vs live) that also pointed at a non-existent `up --reprovision`; both were
dropped as dead once `target machine` was scoped this way.

## Consequences

**Positive:**

- The operator always knows and controls which machine is provisioned.
  No silent spend.
- The `(region × SKU)` matrix surface shows availability, price, and
  latency in one view; the operator picks a row rather than consulting
  the Hetzner Cloud Console.
- Existing clusters self-heal on first `apply` after upgrade —
  zero-action migration for the common case.
- A single fact-drift warning fires only on an out-of-band machine change
  (a Hetzner-Console resize); normal reconciles are silent. Changing a
  machine on purpose goes through the explicit backup + reprovision path,
  not a nagging preference guard.
- Works past 2026-10-01 (`/v1/datacenters` gone, single-endpoint
  design is unaffected).

**Negative / neutral:**

- **BREAKING for `up` / `apply` / `restore --reprovision` without an
  explicit type on a fresh target.** The caller must supply the type via
  `--server-type`, `APPRAFTER_SERVER_TYPE`, `nodes[0].kind` in the
  manifest, or `apprafter target machine` before the next provision.
  Existing clusters are unaffected (reconcile path, no type required).
- `target add` wizard flow changes: the region `Select` is replaced by
  the `(region × SKU)` matrix (region is now a dimension of the row,
  not a standalone prior step). This is a non-interactive-compatible
  change (`--server-type` + `--region` → skip the picker and validate
  the pair directly).
- `inquire 0.7.5` does not expose the in-progress filter string from a
  cancelled `.prompt()` call. Revealing sold-out rows (the `ShowSoldOut`
  toggle) re-runs the picker with the previous filter as
  `with_starting_filter_input`, but the filter typed since `setup()`
  is lost (M6 in the frozen design). This is a known UX limitation.

## Alternatives considered

- **`GET /v1/datacenters` for availability.** Rejected: deprecated
  2026-06-02, returns `410 Gone` after 2026-10-01. The
  `locations[].available` field on `GET /v1/server_types` is the
  current source of truth and requires no additional endpoint.
- **Two independent `Select` boxes (region, then SKU).** Rejected:
  allows assembling structurally invalid pairs that the validator only
  catches post-hoc. An invalid pair produces a confusing error after
  the operator has already committed to both choices. The matrix is a
  single choice that guarantees validity.
- **`sort:` token in the filter string.** Rejected in favour of the
  pre-picker sort `Select`. A sort token is not discoverable (the
  operator must read documentation to know `sort:price-asc` is valid),
  while a `Select` prompt shows the available sort axes explicitly and
  sets a default. It is also not an `inquire` limitation — the scorer
  can reorder, but only at setup time, not live.
- **T-shirt aliases (`small` / `medium` / `large` → curated SKUs).**
  Deferred. Useful sugar, but the mapping requires curation and breaks
  if Hetzner's lineup changes. A raw SKU is always accepted as the
  escape hatch; the alias layer is additive when needed.
- **A `ratatui` grid TUI.** Deferred. A full grid with column headers,
  live resort on header click, and arrow-key navigation is better UX on
  a wide terminal. The `inquire` matrix is sufficient for the current
  ~140–160 row catalog; the `ratatui` path is a separate build, not an
  `inquire` extension.

## Risks

- **`inquire` sold-out toggle loses in-progress filter (M6).** When the
  operator reveals sold-out rows by selecting `ShowSoldOut`, the picker
  re-runs with `with_starting_filter_input` set to the filter string
  captured at the toggle selection moment — not whatever was typed after
  the picker opened. *We accept this* as a known `inquire 0.7.5`
  limitation. It is documented in the release notes.
- **Re-provision retaining state — stale fact (T12 review flag).** If a
  target is re-provisioned without a prior `destroy` (a path that
  retains `state.json`), the write-once `HetznerCloudState.server_type`
  fact and the preserved `kubeconfig_age` / `argocd_admin_password_age`
  cache entries could be stale. A follow-up should confirm `apprafter
  kubeconfig` / `argocd-password` re-validate the cached material after
  a reprovision that retains state.
- **Cross-target clone (DR to a new target).** A new target has no
  `TargetConfig.server_type` and no `HetznerCloudState` — passing
  `--server-type` is required. The backup-manifest-carries-type path
  (a follow-up, D0.4) is the correct long-term home because it survives
  total machine loss, but it touches the 2.6d backup format and is
  deferred.

## Deferred (recorded explicitly)

- **T-shirt→SKU aliases** — additive sugar; requires curation.
- **`ratatui` grid TUI** — better column/sort UX; separate build.
- **Tier-aware SKU filtering** — the offer/validator path is architected
  to accept tier constraints; filtering is additive.
- **Latency as a filter** — latency is sort-only; filter-by-latency
  is additive.
- **Backup manifest carries source type** — the correct DR home for
  cross-target clone type reproduction, but touches the 2.6d format.

## Owner

Andrey Ryahovskiy.

## Re-evaluation

- **When `ratatui` is evaluated (order 4+ or post-launch):** reconsider
  the `inquire` matrix vs a full grid TUI if the catalog grows beyond
  ~200 rows or if the sold-out-toggle filter-loss (M6) draws repeated
  complaints.
- **If `inquire` releases a version that exposes the typed filter from
  `.prompt()`:** close M6 by carrying the filter string across the
  toggle re-prompt.
- **At Phase 3.1 (Tier-2 multi-node):** add tier-aware SKU filtering
  so the picker can exclude incompatible types at the selection step.
- **When a cross-target clone / restore-to-new-target path is added:**
  implement backup-manifest-carries-type (B5b / D0.4) so the source
  type survives total machine loss without an explicit `--server-type`
  argument.

## References

- `docs/superpowers/specs/2026-08-12-2.16h-machine-picker-design.md` —
  the frozen design (rev 6), covering all corrected premises (C1/C2),
  verification items (V1–V10), and the full decision rationale
  (M/N/H labels).
- `plan.md` §2.16h + §2.16h-a — the deliverable decomposition and
  acceptance criteria.
- ADR 0030 — CLI target store and credential chain (the stable
  `apprafter::<area>::<reason>` diagnostic-code scheme referenced for
  `server_type_not_selected` and `server_type_unavailable`).
- `cli/cli-providers/src/machine.rs` + `machine_filter.rs` — offer
  model, join, and filter predicate parser.
- `cli/platform-cli/src/commands/machine_picker.rs` — the picker UI
  (`MachineRow`, `PickerChoice`, scorer, sold-out toggle).
- `cli/cli-providers/src/hetzner_cloud/validators.rs` — `classify()`
  and per-`UnavailableKind` alternatives.
- `cli/cli-core/src/error.rs` — `ServerTypeNotSelected` +
  `ServerTypeUnavailable { kind: UnavailableKind }` error variants.
- `cli/cli-state/src/state.rs` — `HetznerCloudState.server_type`
  (the provider-specific FACT field).
- `cli/platform-cli/src/commands/apply.rs` — resolution chain, backfill,
  the fact-drift guard.
- `cli/platform-cli/src/commands/target_machine.rs` — `target machine`
  (refuses on a provisioned target; sets the type only pre-provision).
- `cli/cli-providers/src/backfill.rs` — `backfill_from`, `classify_guard`.
