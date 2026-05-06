<!-- SPDX-License-Identifier: FSL-1.1-MIT -->

# Contributing to AppRafter

Thanks for considering a contribution.

## Getting started

1. Read [`docs/contributing/setup.md`](docs/contributing/setup.md)
   and pick an install path (Nix flake, Dev Container, or manual).
2. Run `just bootstrap` to install the local Git hooks (lefthook).
3. Find an issue to work on, or open one before starting
   significant work — design discussion is welcome and reduces
   wasted effort.

## Conventions

- Commits follow [Conventional Commits]. The PR title is
  validated by CI and your local commit messages by lefthook.
- Source files declare an `SPDX-License-Identifier` header
  (see [`docs/contributing/license-headers.md`](docs/contributing/license-headers.md)).
- CUE schemas pass `./scripts/lint-cue.sh`.
- Architectural decisions are recorded as ADRs under
  [`docs/adr/`](docs/adr/README.md).
- The `plan.md` in the repository root is updated when a phase is
  closed — flip the checkboxes and append a row to the history
  table.

## Pull requests

- Use the PR template; fill in **Summary**, **Spec / ADR
  references**, and **Test plan**.
- Keep PRs small enough to review in one sitting. Split large
  changes into a stack.
- All checks (`lint`, `test`, `license-check`,
  `conventional-commits`) must pass.

## License

By contributing, you agree your contribution is licensed under the
same terms as the project:

- **FSL-1.1-MIT** — for the platform core (`cli/`, `operator/`,
  `schemas/`, `manifests/`, and platform-internal services).
- **MIT** — for plugins (`providers/`, `backstage-plugins/`, and
  community SDKs).

See [`LICENSE`](LICENSE), [`LICENSE-MIT`](LICENSE-MIT),
and [`NOTICE`](NOTICE) for the full text and rationale.

## Code of Conduct

This project follows the [Contributor Covenant
Code of Conduct](CODE_OF_CONDUCT.md). By participating, you agree
to abide by its terms.

[Conventional Commits]: https://www.conventionalcommits.org/en/v1.0.0/
