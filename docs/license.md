# License history and conversion model

This page describes the AppRafter licensing landscape: which license
covers which code, how the FSL → vanilla-OSS conversion works on a
per-release basis, and the two-license history left by the
ADR 0032 base-license migration.

## Current state (from ADR 0032 onward)

- **Platform core** — `cli/`, `operator/`, `schemas/`, `manifests/`,
  and platform-internal services are licensed under
  **`FSL-1.1-Apache-2.0`** (Functional Source License v1.1 with
  Apache 2.0 Future License). See `LICENSE` for the FSL text;
  `LICENSE-APACHE` reproduces the full Apache 2.0 text used as the
  conversion target.
- **Plugins** — `providers/`, `backstage-plugins/`, and the future
  `InfrastructureProviderPlugin` / `ServiceProviderPlugin` SDKs are
  licensed under the **MIT License** from day one. See `LICENSE-MIT`.
- **Documentation** — everything under `docs/` (the prose, including
  the generated reference pages, and — once published — the
  `llms.txt` / `llms-full.txt` exports) is licensed under
  **`CC-BY-4.0`** — see `LICENSE-CC-BY-4.0`. Reuse, translation and
  machine ingestion are allowed with attribution. The **code samples
  inside** those pages are licensed under **Apache-2.0** so they can
  be pasted into any project without attribution obligations.

## How the FSL conversion works

Each release of the platform core ships under FSL-1.1-Apache-2.0.
On the second anniversary of a given release date, that release
automatically converts to the Apache License, Version 2.0 — the
text reproduced in `LICENSE-APACHE`. The MIT and FSL Permitted-Purpose
restrictions during the active window protect the project from
cloud-vendor SaaS rebranding while the 2-year window is open; after
the window closes, the release is plain Apache 2.0 with no extra
restrictions.

Active development always lives under FSL-1.1-Apache-2.0. The most
recent release is always at most two years away from becoming plain
Apache 2.0 — even if upstream development stops, the codebase
eventually becomes fully OSI-open.

## Two-license landscape

The base license for the core changed once. Pre-ADR-0032 releases
were published under **`FSL-1.1-MIT`** per ADR 0001; ADR 0032 (dated
2026-05-19) migrated the base to **`FSL-1.1-Apache-2.0`**.

The conversion clock is per-release and independent: a release stays
on the terms it was published under and converts on its own
anniversary. The result is a two-license landscape during the FSL
window:

| Release range                    | License at publication | Converts to    |
| -------------------------------- | ---------------------- | -------------- |
| `v0.0.1` – `v0.1.96` (and `v0.1.0-mvp`) | `FSL-1.1-MIT`          | MIT            |
| Post-ADR-0032 (from `v0.1.97` forward)  | `FSL-1.1-Apache-2.0`   | Apache 2.0     |

`LICENSE-MIT` is preserved in the repository root for two reasons:
plugin SDKs continue to use MIT from day one, and pre-ADR-0032
releases continue converting to MIT.

## Why this two-step history

ADR 0001 (2026-05-06) established FSL-1.1-MIT for the core. The FSL
wrap — protection against cloud-vendor rebranding during the active
window — was retained when ADR 0032 (2026-05-19) migrated the base.
The migration's rationale (Apache patent grant + retaliation,
Kubernetes/CNCF convention alignment, Sentry consistency, explicit
trademark posture) is documented in ADR 0032.

ADR 0001's Status field is marked `Superseded` only for the base
license choice; its FSL wrap rationale and the plugin MIT carve-out
remain authoritative.

## Source files and SPDX headers

All tracked source files in the platform core declare an
`SPDX-License-Identifier: FSL-1.1-Apache-2.0` header in their first
five lines (enforced by `scripts/check-spdx-headers.sh`). Plugin and
SDK files declare `SPDX-License-Identifier: MIT`. See
`docs/contributing/license-headers.md` for per-language syntax.

## References

- `LICENSE` — the FSL-1.1-Apache-2.0 text.
- `LICENSE-APACHE` — the Apache 2.0 conversion target (full text).
- `LICENSE-MIT` — the MIT text (plugins + pre-ADR-0032 conversion target).
- `LICENSE-CC-BY-4.0` — the CC-BY-4.0 text covering documentation prose.
- `NOTICE` — short licensing notice in the repository root.
- `docs/adr/0001-license-fsl-1-1-mit.md` — original license decision
  (superseded for the base choice).
- `docs/adr/0032-license-fsl-1-1-apache-2-0.md` — base-license
  migration decision and rationale.
- <https://fsl.software/> — Functional Source License canonical home.
