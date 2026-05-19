# ADR 0032: Migrate core license base from FSL-1.1-MIT to FSL-1.1-Apache-2.0

## Status

Accepted (2026-05-19). Supersedes the MIT base license choice in ADR 0001 for the core; preserves the FSL wrap rationale and the plugin MIT carve-out from ADR 0001 unchanged.

## Context

ADR 0001 established FSL-1.1-MIT for the platform core (`cli/`, `operator/`, `schemas/`, `manifests/`, platform-internal services) and MIT for plugin SDKs. The FSL wrap — protection against cloud-vendor rebranding during the active window — is sound and unchanged by this decision.

The base license choice itself was not directly compared against FSL-1.1-Apache-2.0 in ADR 0001. All four Context arguments in that ADR (self-hostable, discourage cloud rebrand, OSI-clean, abandonment safety) and all four Alternatives (Apache-only, AGPL+MPL split, BSL, plain MIT) justified the FSL wrap; none distinguished MIT base from Apache base as the FSL conversion target. The MIT base appears to have been a default-on-GitHub choice rather than a considered one against Apache.

Several dimensions favour Apache 2.0 as the base for the platform core:

- **Patent surface of the domain.** The platform orchestrates Cilium (eBPF / CNI), SPIFFE/SPIRE (workload identity), OpenBao (Vault tech lineage), Kamaji (control-plane multi-tenancy), and confidential containers (Kata / CoCo on Intel TDX, AMD SEV-SNP, ARM CCA). This is a domain with real patent footprint where an explicit patent grant from contributors is more than ceremony. Apache 2.0 §3 provides such a grant; MIT is silent on patents and leaves an implicit-license ambiguity.

- **Patent retaliation as defensive moat.** Apache 2.0 §3 terminates the licensee's patent grant if they sue alleging the software infringes their patents. This discourages patent troll behaviour and weaponisation of contributions at zero cost to the project.

- **Ecosystem convention alignment.** Kubernetes itself, Cilium, Kamaji, OpenBao, containerd, etcd, Linkerd, Istio, and effectively every CNCF graduated/incubating project are licensed under Apache 2.0. MIT in k8s infrastructure tooling is uncommon and reads as either an un-considered default or a small-CLI choice — neither matches the position the platform core occupies. MIT-as-default-on-new-GitHub-repo also creates the impression of license-as-afterthought, which is the opposite of the signal the project should send.

- **Reference precedent consistency.** ADR 0001 cites Sentry's FSL adoption as a model. Sentry uses FSL-1.1-Apache-2.0 specifically. Aligning the base license closes the gap between the cited reference and the actual choice.

- **Trademark posture.** Apache 2.0 §6 explicitly denies trademark rights granted by the license. With "AppRafter" as a brand worth protecting long-term, this is cleaner posture than MIT's silence.

The cost of switching now is minimal: the project is pre-Phase-2, contributions are effectively solo, and no external CLA backfill is required. Re-licensing later (post external contributions) would be substantially more expensive.

## Decision

The platform core (`cli/`, `operator/`, `schemas/`, `manifests/`, and platform-internal services) is licensed under the Functional Source License v1.1 with Apache 2.0 Future License (`SPDX: FSL-1.1-Apache-2.0`), copyright "AppRafter Authors", year 2026. Each release converts automatically to the Apache License 2.0 two years after its publication date.

Plugin SDKs (`providers/`, `backstage-plugins/`, future `InfrastructureProviderPlugin` / `ServiceProviderPlugin` SDKs and community plugins) remain under the MIT License from day one. This part of ADR 0001 is unchanged.

Repository LICENSE files after migration:
- `LICENSE` — the FSL-1.1-Apache-2.0 text covering the core.
- `LICENSE-APACHE` — the Apache 2.0 text (conversion target for the core).
- `LICENSE-MIT` — the MIT text (covers plugin SDKs as before).
- `NOTICE` — updated to describe the new conversion model and the pre-existing FSL-1.1-MIT releases (see Risk below).

Existing pre-this-ADR releases under FSL-1.1-MIT (if any are already published to OCI / crates.io / npm) retain their original terms; their conversion clock still runs to MIT, not Apache. Only versions published from this decision point forward convert to Apache.

## Consequences

**Positive:**
- Explicit patent grant from contributors via Apache §3 — relevant for a project that touches CNI / identity / confidential computing primitives with real patent footprint.
- Patent retaliation clause discourages patent troll behaviour and weaponisation of contributions.
- Convention alignment with the Kubernetes / CNCF ecosystem reduces cognitive load for adopters and removes "why are you not Apache like everything else here?" friction in procurement review.
- Closes the inconsistency between citing Sentry as a reference and diverging on the base license choice.
- Trademark posture is explicit (Apache §6), helpful for protecting the "AppRafter" mark.

**Negative:**
- Apache 2.0 is significantly longer than MIT (~1700 vs ~170 words). Casual readers may find the LICENSE file more intimidating at first glance.
- SPDX identifier marginally longer (`FSL-1.1-Apache-2.0` vs `FSL-1.1-MIT`).
- One-time migration cost: replace LICENSE files, update SPDX headers across the repo, update NOTICE, update README and CONTRIBUTING mentions, update spec.md §7 and §8.

**Trade-offs:**
- LICENSE verbosity traded for explicit patent / retaliation / trademark posture and ecosystem alignment.
- Inconsistent conversion targets for pre/post-ADR releases (MIT for old releases, Apache for new) traded for clean license history — pre-existing FSL-MIT releases are not re-licensed retroactively.

## Risk

**Main risk:** existing FSL-1.1-MIT releases (if any are published) retain their original terms; their MIT conversion clock continues running independently. This creates a two-license landscape during the FSL window — pre-ADR-0032 releases converting to MIT, post-ADR-0032 releases converting to Apache 2.0. **Mitigation:** document this explicitly in `NOTICE` and `docs/license.md`; audit currently-published releases before the migration commit (likely none or very few given pre-Phase-2 status). If no public releases yet exist, the risk reduces to zero.

**Secondary risk:** if any external contributors have merged commits before this ADR lands, their consent to re-license is required. **Mitigation:** audit `git log --pretty=format:'%an %ae'` before the re-license commit; obtain explicit consent from each external author. If consent cannot be obtained, those specific commits may need rewriting under the new license terms before the migration lands.

**Tertiary risk:** FSL ecosystem tooling (scanners, package managers, distros) remains immature for either FSL variant; this risk is unchanged from ADR 0001 and applies equally to both base licenses.

## Owner

Project maintainers.

## Re-evaluation triggers

- The OSI approves a successor to FSL with broader recognition (same trigger as ADR 0001).
- CNCF or another widely-adopted ecosystem body publishes a licensing convention that materially conflicts with Apache-as-default for k8s-adjacent projects.
- Adoption is materially limited by license confusion attributable specifically to the base license choice (not to FSL wrap, which is the dominant friction driver).

## Still open

- **Migration commit sequencing.** Whether to land the LICENSE/NOTICE/SPDX changes as a single commit or split (LICENSE files + SPDX header sweep + spec.md update + ADR 0001 status update) is a maintainer preference; recommendation is split commits for clean review history.

## References

- ADR 0001 (`docs/adr/0001-license-fsl-1-1-mit.md`) — original license decision; superseded for the core base license choice only. FSL wrap rationale and plugin MIT carve-out preserved.
- `spec.md` §7 ("License decision"), §8 ("Why FSL-1.1-MIT for the core (Sentry's model)") — both require update following this ADR. §8 title should reflect the new Apache base.
- `LICENSE`, `LICENSE-MIT`, `NOTICE` in repo root — to be updated; new `LICENSE-APACHE` to be added.
- README, CONTRIBUTING — license mentions to be updated.
- <https://fsl.software/> — FSL license text.
- Sentry's FSL adoption announcement (2023) — uses FSL-1.1-Apache-2.0.
- Apache License 2.0 §3 (patent grant), §6 (trademark disclaimer).
