# ADR 0058: public surfaces are written for their reader, not derived from our artifacts

## Status

`Accepted`

Date: 2026-08-29.

## Context

Three independent documentation defects were found in one session. Read together they are
one failure, not three.

| Symptom | The internal artifact behind it |
|---|---|
| `operator-guide/backup-restore.md` documented two CLI defects as current behaviour, with manual workarounds, for a week after both were fixed | a **discovery** artifact — the walk's bug list |
| `operator-guide/redis.md`, `postgres.md` and `persistent-disk.md` instruct the reader to hand-author internal CRDs and exec into pods | a **validation** artifact — the e2e walk script |
| `/status/` publishes plan coordinates, release versions, yank history and e2e script names | a **planning** artifact — `FEATURE_TRACKER.md`, published by `git mv` |

In each case an artifact of the project's own process reached a public surface without being
rewritten for the audience that surface serves.

### The stale section

`backup-restore.md`'s "Two defects to plan around" was written 2026-08-21 (`20b1ffc`). Both
defects were fixed the next morning — `f7ee105` and `e94fb4c`, released as cli v0.2.49 — and
`894bdfd` added e2e assertions proving the opposite of what the page says. The page was never
reopened. The correct record went into `docs/changelog/UNRELEASED.md`, which is excluded from
the published site, and `docs/changelog/plan-history.md` received no entry at all.

This is not rare. Between 2026-05 and 2026-08 the repository accumulated **79 corrective
documentation commits in roughly 101 days**. One of them, `79c3307`, withdrew a guide that had
been stale for 93 days and stated this same diagnosis — 42 hours before the stale section was
written.

### Why no gate caught it

The documentation gate is green on the false section today:

```
docsgen gate OK: every claim in the in-scope documentation resolves
37 pages, 652 invocations, 359 identifiers, 118 code paths, 87 ADR references, 5 exemptions
```

Every gate in the repository validates one of two properties: **referential integrity** (does
this name resolve — command path, flag, field path, file path, ADR number, link anchor?) or
**fidelity** (does this artifact byte-match the source that generated it?). The stale section is
a passage in which every noun resolves and every verb is false: `apprafter target machine` is a
real command, `cli/platform-cli/src/commands/backup.rs` a real file, `run_backup_check` a real
function.

Three aggravating facts:

1. **The trigger fired and passed.** `.github/workflows/docs.yml` filters on
   `cli/platform-cli/src/**`, so the documentation CI ran on both fix commits and reported
   success. Trigger coverage is complete; assertion coverage is empty. A green run reads as
   "the documentation agrees with this change" and means only "no identifier stopped resolving".
2. **The health ratchet objects to the repair.** Obligation counts may only grow, so deleting
   the false section lowers `code_paths` and `invocations` below the committed census and fires
   `health-baseline`. Leaving the falsehood costs nothing; removing it is the noisy operation.
3. **Executable documentation was deliberately not built** (ADR 0057), so no mechanism compares
   a documented outcome against a real one.

ADR 0057 priced and rejected the obvious remedies with measurements — staleness gating by
elapsed time, a per-page `since` claim, typed `verified-by` bindings. Those rejections stand and
this ADR does not revisit them. But one behavioural-truth gate does exist and was overlooked:
`cli/docsgen/src/shipped.rs` classifies each `needs` key as shipped or merely declared by
whether a provisioner backend arm and a seeded `ServiceProvider` exist, and fails a guide that
presents an unshipped type as usable. It was never generalised past `needs` keys.

### The shape defect

A census of the 27 pages under `docs/operator-guide/`, `docs/dev-guide/` and `docs/index.md`
measured **478 `apprafter` invocations against 325 foreign commands**. Classified by the role
each foreign command plays for the reader:

| Role | Count | What it is |
|---|---|---|
| VERIFY | 153 | independent confirmation of something already achieved |
| TROUBLESHOOT | 60 | diagnosis, relevant only when something has gone wrong |
| EXPLAIN | 51 | illustration of the mechanism, not meant to be run |
| SETUP | 61 | the reader must run it to complete the page's goal |

Four out of five foreign commands are not part of achieving anything, and **96 blocks interrupt
the happy path** — they sit before the recipe is complete. **11 of the 27 pages** cannot be
completed with `apprafter` commands alone.

Of the 61 SETUP commands, 27 are legitimately external (installing the binary before it exists,
`git`, container tooling, a rescue-mode shell, a provider's web console), 8 are documentation
laziness where an `apprafter` command already exists, and 23 looked like product gaps. Re-judged
against "should a reader of a recipe ever do this?" rather than "does a CLI equivalent exist?",
**14 of those 23 turned out to be walk material that does not belong on a guide at all**, 3 were
the shell's own configuration, 3 belonged in verification or troubleshooting, and 3 were real —
of which one did not survive verification.

The correlation that explains the shape is exact. Five pages reference an e2e walk. Four of them
are the four worst pages by foreign-command count. The fifth, `backup-restore.md`, references
walks too and stayed clean — because its walks are CLI-driven.

> A guide inherits the shape of its walk.

The three `needs.*` pages share one skeleton with their scripts: *"This guide **walks** a Tier-1
operator through the full `needs.X` chain"* → "The chain in one paragraph" → "Happy chain — steps
1 to N" (the walk's phases) → "Confirm the X is physically gone" (the walk's assertions) →
"Checklist — did it work?" (the walk's pass/fail) → "For contributors".

The forced-GC sections are the sharpest consequence. They instruct the reader to delete their own
`RetainedClaim` and hand-author a replacement with a `retainUntil` in the past, while
`operator/admission-webhook/src/validator_retainedclaim.rs` states that *"RetainedClaims are
written by the `resourceclaim-provisioner` finalizer … users must never hand-author them"*. The
platform ships code whose purpose is to prevent what the guide teaches.

### The positioning conflict

Three sources disagreed about whether this matters.

- `spec.md` Appendix B listed as a non-goal: *"Replace `kubectl` / `helm` / `k9s` for platform
  engineers — they remain the operations interface."*
- ADR 0057's 2.19i amendment ratified the current shape with measurements, concluding that
  `kubectl`-dominance in the dependency guides *"is a reason to keep them together, not to split
  them"*.
- The landing page promises *"no kubectl, no YAML"*.

The conflict was resolved deliberately in favour of making the landing page true.

## Decision

**A public documentation surface is written for the reader it serves. An artifact of our own
process is never published verbatim.** Three clauses follow.

### 1. A guide is a recipe

An operator or developer guide exists to give ready-made procedures for installing, configuring,
tuning and performing routine maintenance. Its main flow carries **only `apprafter` commands and
genuinely external tools**. A raw `kubectl` step in a recipe path is a defect in the guide.

Everything else has exactly one role and one destination:

| Role | Destination | Form |
|---|---|---|
| **Delete** | nowhere | walk material. The property it demonstrated becomes one sentence of prose, with the e2e named as its evidence. |
| **Verify** | on the page, collapsed | a collapsible block. **One per section outcome, never per step.** It states what the reader should *see*, not only what to run. |
| **Explain** | a separate mechanism layer | full pages, reached by one link from the guide. |
| **Troubleshoot** | the troubleshooting page | a row in the diagnostic table. |

**Allowlisted external tools**, permitted in a recipe path without exemption: the pre-install
bootstrap that runs before the binary exists; `git`; container build and push tooling; host
utilities and `ssh` inside a rescue-mode ramdisk, where no cluster and no binary exist; DNS and
TLS inspection; `restic` used as the documented portability escape hatch; the reader's own shell
configuration; a provider's web console. Rolling workloads after a node-substrate change is
allowlisted with a stated reason and recorded as an open product question.

A page may hold a page-level exemption with a stated reason. The rescue runbook has one: its
reader is inside a provider ramdisk where `apprafter` is not installed.

### 2. A guide does not mirror a walk

The e2e walk is a validation artifact. It lives in `e2e/`, it is referenced, and it is not
transcribed. A page whose headings track a script's phases, whose sections restate its
assertions, and whose checklist is its pass/fail is a walk wearing a guide's clothes.

The walk proves a property to us. The guide tells a reader what to do. A reader does not need to
prove the platform works; they need to know that it does and what to run.

### 3. An internal artifact is never published verbatim

Discovery notes, walk transcripts, planning ledgers and strategy documents are rewritten for the
audience or they do not ship. Moving a file into a published directory is not a rewrite.

Two corollaries:

- **A defect note is a commitment to retract it.** Documenting current behaviour as broken
  creates an obligation on whoever fixes it. A change that falsifies a page updates that page in
  the same series.
- **A redaction log never travels with the redacted output.** Notes recording what was removed
  reproduce exactly what the redaction existed to remove, and are a reviewer-side artifact only.

## Consequences

- **`spec.md` Appendix B narrows.** The platform tools remain available, supported, and the
  interface for break-glass and deep inspection — but they are not required to install,
  configure, tune or maintain AppRafter. §1.5's "no imperative scripts in the happy path" is
  extended to documented happy paths.
- **ADR 0057's 2.19i ratification is superseded** by its 2026-08-29 amendment.
- **A new gate class, `recipe-purity`,** becomes the enforcement: a foreign invocation outside a
  collapsed block, outside the troubleshooting and rescue pages, and off the allowlist is a
  finding. It is structural, which is what the existing generator is good at, and it reuses the
  existing typed, dated, expiring exemption channel.
- **A second gate class, `behaviour-claim`,** generalises `shipped.rs` — a table binding a
  documented behavioural predicate to a pure function or constant, with a completeness test
  derived from the source of truth. It supersedes the forbidden-claims lint that ADR 0057
  decision 7 still promises, that the design spec retracted, and that no code implements.
- **Deleting a false section will fire the health ratchet.** Re-recording the census is a normal,
  reviewable part of a deletion commit. This is a known counter-productive signal, recorded here
  rather than worked around.
- **Guides get shorter and mechanism pages appear.** Readers who want the internals gain a place
  to find them, at the cost of one link.
- **Some verification the reader could previously perform inline moves behind a disclosure.**
  This is deliberate: it was 47% of the foreign-command mass and the dominant reason the guides
  read as demanding Kubernetes fluency.

## Alternatives considered

- **Leave the shape as ADR 0057 ratified it.** Rejected: the ratification was measured, but it
  measured fence composition rather than the role each command plays for a reader, and it did
  not observe the walk-to-guide correlation. The landing page also promises otherwise, and the
  owner chose to make the landing true rather than retract it.
- **Collapse the deep material inline and change nothing else.** Rejected: it leaves failure
  handling smeared through the recipe instead of collected where a reader in trouble will look,
  and it preserves the walk skeleton that produced the problem.
- **Delete the deep material outright.** Rejected: independent verification has real value for an
  operator, and mechanism explanations are what a reader reaches for when something surprises
  them. Routing by role keeps both at the cost of one click.
- **Rewrite the landing to match the specification.** Rejected by the owner: the promise of an
  operable platform without Kubernetes fluency is the product, not the marketing.
- **Add per-page staleness gating by elapsed time.** Rejected previously in ADR 0057 with six
  measurements, and not revisited here.

## Risks

- **The restructure is large** — 27 pages, of which 11 fail the happy-path test today.
  Mitigation: the gate is built red before any page is rewritten, so progress is measurable page
  by page and the tiers ship independently.
- **The gate could be over-fitted to the corpus it was written against.** Mitigation: its
  fixtures are taken from the current pages before any rewrite, so it must fail on real
  historical text rather than on a synthetic case.
- **A property asserted in prose is weaker than a property a reader can check.** Mitigation:
  every deleted demonstration names the e2e that proves it, and the collapsed verification block
  keeps one honest check per outcome. We accept that a reader who wants to verify must open a
  disclosure.
- **`recipe-purity` cannot judge whether a collapsed block is honest**, only that a foreign
  command is inside one. A guide could satisfy it by collapsing everything and explaining
  nothing. Mitigation: review, and the one-block-per-outcome rule stated here as normative.
- **The behavioural predicate table is hand-curated** and can go stale like any table.
  Mitigation: the completeness test reads the source of truth, so a new predicate forces a
  decision rather than defaulting to unchecked — the property that makes `shipped.rs` work.

## Owner

Platform documentation. Amendments follow ADR 0057's convention: the decision text is written
once and left alone; what measurement later overturns is recorded as an appended amendment.

## Re-evaluation

Revisit when the corrective-documentation-commit rate over a 90-day window returns to the level
that motivated this decision, or when `recipe-purity` has been green for two consecutive phases
with no manual documentation walk finding a recipe-path regression — whichever comes first.

## References

- ADR 0057 — documentation system, and its 2026-08-29 amendment.
- ADR 0042 §9.7, ADR 0040 — behaviour the guides misdescribe today.
- `cli/docsgen/src/shipped.rs` — the existing behavioural-truth gate this generalises.
- `operator/admission-webhook/src/validator_retainedclaim.rs` — the code that forbids what the
  dependency guides teach.
- `spec.md` §1.5, Appendix B.
