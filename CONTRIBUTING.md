<!-- SPDX-License-Identifier: FSL-1.1-Apache-2.0 -->

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
- Documentation under `docs/` is gated: `just lint` resolves every
  `apprafter` invocation and every schema field path a page writes
  against what the tree actually ships. If you meet a finding, see
  [`docs/contributing/documentation-gate.md`](docs/contributing/documentation-gate.md)
  — it covers the marker grammar, the front-matter exemption
  channel, the typed reasons and the 180-day expiry.
- Architectural decisions are recorded as ADRs under
  [`docs/adr/`](docs/adr/README.md).
- The `plan.md` in the repository root is updated when a phase is
  closed — flip the checkboxes and append a row to the history
  table.
- **CLI version bump on every release.** Each release commit that
  bumps the patch version (`v0.1.N → v0.1.N+1`) also updates
  `cli/Cargo.toml`'s `workspace.package.version` to match. This
  keeps `apprafter --version` honest about what's installed.
  The convention starts at `v0.1.77`; earlier tags between
  `v0.1.3` and `v0.1.76` ship binaries that print `0.1.2`
  because the field drifted (one-off historical issue, not a
  bug worth retroactively patching).

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

- **FSL-1.1-Apache-2.0** — for the platform core (`cli/`, `operator/`,
  `schemas/`, `manifests/`, and platform-internal services).
- **MIT** — for plugins (`providers/`, `backstage-plugins/`, and
  community SDKs).

See [`LICENSE`](LICENSE), [`LICENSE-APACHE`](LICENSE-APACHE),
[`LICENSE-MIT`](LICENSE-MIT), and [`NOTICE`](NOTICE) for the full
text and rationale.

## Code of Conduct

This project follows the [Contributor Covenant
Code of Conduct](CODE_OF_CONDUCT.md). By participating, you agree
to abide by its terms.

[Conventional Commits]: https://www.conventionalcommits.org/en/v1.0.0/
