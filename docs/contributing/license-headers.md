# SPDX License Headers

Every source file in this repository must declare its license via an
`SPDX-License-Identifier` comment as the first or second line of the file.

Two identifiers are in use across the monorepo:

| Path                                            | SPDX identifier |
| ----------------------------------------------- | --------------- |
| `cli/`, `operator/`, `schemas/`, `manifests/`   | `FSL-1.1-MIT`   |
| `providers/`, `backstage-plugins/`              | `MIT`           |

Plugins and SDKs are MIT from day one to keep contribution friction
minimal; the platform core uses FSL-1.1-MIT (see `NOTICE` for the rationale).

## Per-language syntax

Rust:

```rust
// SPDX-License-Identifier: FSL-1.1-MIT
```

TypeScript / JavaScript / OneBun:

```ts
// SPDX-License-Identifier: MIT
```

CUE:

```cue
// SPDX-License-Identifier: FSL-1.1-MIT
```

YAML / Bash / Dockerfile (hash comments):

```yaml
# SPDX-License-Identifier: FSL-1.1-MIT
```

HTML / Markdown (HTML comment, optional):

```html
<!-- SPDX-License-Identifier: FSL-1.1-MIT -->
```

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
