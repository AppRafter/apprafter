# schemas/

CUE schemas for every platform CRD (`Application`, `ServiceProvider`,
`ResourceClaim`, `AccessGrant`, `MigrationPlan`, `ExternalSurface`,
`Infrastructure`, `*Plugin`). The schemas are the source of truth:
they drive admission validation, OpenAPI v3 CRD generation, and code
generation for the operator.

## Layout

| Path                                      | Contents                                                  |
| ----------------------------------------- | --------------------------------------------------------- |
| `v1alpha1/types.cue`                      | Shared definitions (`#TypeMeta`, `#ObjectMeta`, enums).   |
| `v1alpha1/<crd>.cue`                      | One file per CRD; skeleton fields for the v1alpha1 group. |
| `k8s/`                                    | Imported upstream Kubernetes types (populated in phase 1.7). |

## CUE module

The CUE module manifest lives at the **repository root**
(`cue.mod/module.cue`, module name `apprafter.io`), so that
`schemas/` and `examples/` share the same module and import paths
look like:

```cue
import v1alpha1 "apprafter.io/schemas/v1alpha1"
```

## Lint locally

```sh
./scripts/lint-cue.sh
```

The script falls back to `nix run nixpkgs#cue --` if no local CUE
binary is on `PATH`.
