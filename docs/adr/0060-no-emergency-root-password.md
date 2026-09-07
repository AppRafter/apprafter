# ADR 0060: no emergency root password on a Tier-1 node

## Status

`Accepted`

Date: 2026-09-07.

## Context

A Tier-1 node is provisioned with key-only SSH. When the node boots into a
state the platform cannot reach — a cloud-init error that leaves the network
down, a full disk, a kernel that will not come up — SSH is gone with it, and
the operator needs a way in that does not depend on the thing that broke.

The provider offers two: a browser console (noVNC), and a rescue image booted
over the network with an operator-supplied key. The console is available
immediately and needs no key, but it is a login prompt: on a key-only node
there is no password to type, so it shows a machine nobody can log into.
Setting one in cloud-init (`chpasswd`) would make it useful.

This decision was made while writing `operator-guide/recovery.md` and lived in
that guide's prose for its whole life. It is recorded here because a guide
answers *what do I run*, and this is a *why it was decided that way* — the
reader of the runbook needs the constraint, not the argument.

## Decision

**We will not set a root password on a Tier-1 node, and the rescue path is the
provider's rescue image with the operator's own key.**

A node keeps key-only authentication from provisioning to destruction. The
browser console stays available and stays unusable for login, which is the
intended outcome rather than a gap.

## Consequences

- **The credential surface does not grow.** A password — even one held in the
  target's state file — brings rotation policy, leakage risk and an audit
  question with it, permanently, in exchange for an escape hatch used rarely.
  Rescue Mode reaches the same disk without changing the security baseline.
- **The recovery a Tier-1 operator reaches for is rebuild, not repair.**
  Provisioning takes minutes and the fault becomes a patch to the manifest or
  the user-data rather than a hand-edit nobody else receives. The runbook says
  so at its decision point, and rescue is for the case where there is state on
  disk worth preserving.
- **The console is a diagnostic surface, not an access one.** It still shows
  boot output and kernel messages, which is most of what triage needs.
- **An operator who genuinely needs console login has no supported route.** We
  accept this for Tier 1 and revisit it below.

## Alternatives considered

- **A password in cloud-init, held in the target's state file.** Rejected: it
  is the credential-surface growth above, taken permanently for an
  occasional convenience, and it puts a shell credential in a file whose whole
  security boundary is filesystem permissions.
- **A password generated on demand and injected through the provider API.**
  Rejected for Tier 1 as more machinery than the tier's own recovery answer
  needs — the tier has no persistent data whose loss the rebuild path does not
  already cover.
- **Nothing at all — no rescue path either.** Rejected: an operator with
  workload state on the disk needs a way to read it, and the provider's rescue
  image gives one without a standing credential.

## Risks

- **A future tier will want the console.** Tier 3 and Tier 4 run on hardware
  where "rebuild in three minutes" is not true and where an operator may be
  physically remote from the machine. Mitigation: this decision is scoped to
  Tier 1 by its own terms, and the re-evaluation trigger below names those
  tiers. When it is revisited, the shape to reach for is an explicit opt-in
  with audit logging on first use, not a default.
- **The rescue path is provider-specific.** It is Hetzner's rescue image
  today, and a second infrastructure provider will need its own equivalent
  documented. Accepted: the provider boundary already carries per-provider
  procedures.

## Owner

Platform operations.

## Re-evaluation

At Tier 3 (`M5`), and immediately if an operator reports a Tier-1 recovery that
Rescue Mode could not complete.

## References

- `docs/operator-guide/recovery.md` — the runbook this decision shapes.
- ADR 0030 — the target store and the credential chain, where the node's key
  is resolved from.
