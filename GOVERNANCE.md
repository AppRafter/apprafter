<!-- SPDX-License-Identifier: FSL-1.1-Apache-2.0 -->

# Governance

## Roles

- **Maintainers** — small group with merge rights. Responsible for
  release cuts and final architectural decisions.
- **Contributors** — anyone who opens an issue, sends a PR, or
  participates in design discussion.
- **Users** — anyone running AppRafter in production or development.

## Day-to-day decisions

For routine work — code review, bug-fix PRs, documentation updates,
small features — maintainers decide by **lazy consensus**: if no
maintainer objects within a reasonable window (typically 72 hours
for non-urgent changes), the change lands.

## Architectural decisions

Anything that warrants an ADR (a non-trivial choice with long-term
consequences — see [`docs/adr/`](docs/adr/README.md)) follows this
process:

1. The proposer drafts a `Proposed` ADR using
   [`0000-template.md`](docs/adr/0000-template.md).
2. Maintainers and interested contributors comment on the PR.
3. The proposer iterates until the discussion settles.
4. A maintainer ratifies the ADR (status → `Accepted`) once
   consensus is reached. The ratifying maintainer is recorded as
   the **Owner** in the ADR.
5. If contention persists past two iteration rounds, the decision
   escalates to a maintainer vote — simple majority of active
   maintainers, ties broken by the project lead.

## Becoming a maintainer

Contributors who consistently produce reviewed-and-merged work over
several months may be invited to become maintainers. There is no
strict quota; growth is by need and trust. Existing maintainers
nominate; promotion is by **unanimous consent** of current
maintainers.

## Stepping back

Maintainers may step back at any time. They are listed as alumni in
the project history and retain the ability to be re-instated by
the same nomination process.

## Project lead

Until the maintainer team grows beyond two or three people, the
project founder serves as **project lead** and casts the
tie-breaking vote when needed. The lead role is rotated by
unanimous consent of maintainers once the team is large enough to
support a more distributed model.

## Changes to this document

Changes to `GOVERNANCE.md` follow the architectural-decision
process above (ADR + maintainer ratification).
