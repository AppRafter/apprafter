---
description: "Which SPDX identifier each directory takes, the comment syntax per language, and how generated and vendored files are handled."
---

# SPDX License Headers

Every source file in this repository must declare its license via an
`SPDX-License-Identifier` comment as the first or second line of the file.

Two identifiers are in use across the monorepo:

| Path                                            | SPDX identifier        |
| ----------------------------------------------- | ---------------------- |
| `cli/`, `operator/`, `schemas/`, `manifests/`   | `FSL-1.1-Apache-2.0`   |
| `providers/`, `backstage-plugins/`              | `MIT`                  |

Plugins and SDKs are MIT from day one to keep contribution friction
minimal; the platform core uses FSL-1.1-Apache-2.0 (see `NOTICE` and
ADR 0032 for the rationale). Releases v0.0.1 through v0.1.96 were
published under the previous FSL-1.1-MIT base — see ADR 0032 for the
migration history.

## Per-language syntax

Rust:

```rust
// SPDX-License-Identifier: FSL-1.1-Apache-2.0
```

TypeScript / JavaScript / OneBun:

```ts
// SPDX-License-Identifier: MIT
```

CUE:

```cue
// SPDX-License-Identifier: FSL-1.1-Apache-2.0
```

YAML / Bash / Dockerfile (hash comments):

```yaml
# SPDX-License-Identifier: FSL-1.1-Apache-2.0
```

HTML / Markdown outside `docs/` (HTML comment, optional):

```html
<!-- SPDX-License-Identifier: FSL-1.1-Apache-2.0 -->
```

Markdown under `docs/` takes **no** per-file header. That whole tree is
covered by `docs/license.md` + `LICENSE-CC-BY-4.0` — prose is CC-BY-4.0,
code samples embedded in the pages are Apache-2.0. A per-file
`FSL-1.1-Apache-2.0` header on a `docs/` Markdown file is wrong; remove it
if you find one.

## Generated files

Files emitted by build tooling (CRD manifests rendered from CUE, codegen
output, lockfiles) inherit the SPDX identifier of their source. Lockfiles
themselves do not need a header.

## Third-party files

Files vendored from upstream projects keep their original headers
unmodified. Add a `// SPDX-FileCopyrightText:` line if the upstream did
not already have one.

## Enforcement

Once CI is wired up, it will verify that every tracked source file under
the paths above declares an appropriate `SPDX-License-Identifier`. Files
without one will fail the lint stage.
