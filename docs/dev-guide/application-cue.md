# Writing Application.cue

AppRafter applications are described by a CUE manifest that lives
in the `apprafter/` directory of your repository. When you push the
repository, Argo CD detects the `.cue` files, runs the CUE
Config-Management-Plugin (CMP) sidecar to compile them, and passes
the resulting Kubernetes YAML to its sync pipeline. The AppRafter
operator then reconciles the `Application` CR into a Deployment and
Service.

You do not maintain a separate rendered-output branch or run a
local pre-commit step. Edit CUE, commit, push — GitOps handles
the rest.

## The canonical filename

Place your manifest at `apprafter/Application.cue` in the root
of your repository (or the repository path you registered with
`apprafter app add`). The CMP discovery rule is:

```yaml
discover:
  find:
    glob: "**/apprafter*.cue"
```

Any file matching that glob anywhere in the repository (or in the
configured `spec.source.path`) triggers CUE compilation. The
recommended layout is `apprafter/Application.cue`; avoid naming
other CUE files in the repo `apprafter*.cue` unless you want them
compiled alongside the manifest.

For a monorepo with multiple services, each service can have its
own `apprafter/Application.cue`. Control which paths Argo CD
rescans on each push with the
`argocd.argoproj.io/manifest-generate-paths` annotation on the
Argo CD `Application` CR — for example
`manifest-generate-paths: /parser,/shared-schemas`.

## Minimal manifest

```cue
// apprafter/Application.cue
package apprafter

import v1alpha1 "apprafter.io/schemas/v1alpha1"

app: v1alpha1.#Application & {
    metadata: {
        name:      "my-service"
        namespace: "apprafter"   // matches app add --namespace default
    }
    spec: base: {
        image:    "ghcr.io/my-org/my-service:1.0.0"
        replicas: 1
        expose: {
            port:   8080
            public: false
        }
        env: {
            LOG_LEVEL: "info"
        }
    }
}
```

### Field reference

All fields live under `spec.base` (or `spec.environments.<env>` for
overrides — see below). The Rust operator mirrors this exactly via
`ApplicationSpec { base, environments }`.

| Field             | Type                        | Notes                                               |
| ----------------- | --------------------------- | --------------------------------------------------- |
| `image`           | `string`                    | OCI image reference. Required (via base or all envs). |
| `replicas`        | `int & >=0`                 | Zero is valid (scale-to-zero). Defaults to 1 at render time. |
| `expose.port`     | `int & >0 & <=65535`        | Container port to expose.                           |
| `expose.public`   | `bool` (default `false`)    | Whether to create a public-facing HTTPRoute.        |
| `expose.network`  | `"public" \| "internal" \| "vpn"` (default `"internal"`) | Network visibility for the generated HTTPRoute. |
| `env`             | `[string]: string`          | Literal string env-vars only (no secret refs in v1alpha1). |

The admission webhook enforces non-empty `image` and the
cross-field rule (image must be reachable through `spec.base.image`
or every `spec.environments[*].image`). The CRD OpenAPI schema
rejects negative replicas and out-of-range port numbers.

## Multi-environment patterns

`spec.environments` holds per-environment overrides. The operator
reads `APPRAFTER_ENV` to select which environment key to unify
onto `spec.base` before rendering.

Unification rules:
- `image`, `replicas`, `expose` — **replace**: the environment
  value overwrites the base value when present.
- `env` — **merge with override-wins**: base env-vars are preserved
  and environment-specific vars are added; if the same key appears
  in both, the environment value wins.

```cue
// apprafter/Application.cue
package apprafter

import v1alpha1 "apprafter.io/schemas/v1alpha1"

app: v1alpha1.#Application & {
    metadata: {
        name:      "parser"
        namespace: "apprafter"
    }
    spec: {
        base: {
            image:    "ghcr.io/my-org/parser:1.2.0"
            replicas: 2
            expose: {
                port:   8080
                public: false
            }
            env: {
                LOG_LEVEL: "info"
                DB_HOST:   "postgres.svc"
            }
        }
        environments: {
            dev: {
                replicas: 1
                expose: {
                    port:    8080
                    network: "vpn"
                }
                env: {
                    LOG_LEVEL: "debug"   // overrides base LOG_LEVEL
                }
            }
            prod: {
                replicas: 5
                expose: {
                    port:   8080
                    public: true
                }
            }
        }
    }
}
```

With `APPRAFTER_ENV=dev` the operator renders:

- `image`: `ghcr.io/my-org/parser:1.2.0` (from base, no dev override)
- `replicas`: 1 (dev overrides base)
- `expose.network`: `"vpn"` (dev overrides base)
- `env`: `{ LOG_LEVEL: "debug", DB_HOST: "postgres.svc" }` (merge, dev wins on LOG_LEVEL)

With `APPRAFTER_ENV=prod`:

- `image`: `ghcr.io/my-org/parser:1.2.0`
- `replicas`: 5
- `expose.public`: `true`
- `env`: `{ LOG_LEVEL: "info", DB_HOST: "postgres.svc" }` (base unchanged)

If `APPRAFTER_ENV` is not set, the operator uses `spec.base` directly
without applying any environment overlay.

## How the CUE CMP works

The `argocd-cue-cmp` sidecar container runs inside the Argo CD
`argocd-repo-server` pod. When Argo CD clones a repository and the
discovery rule matches a `apprafter*.cue` file, the sidecar runs:

```sh
cue export ./... --out yaml
```

The CUE standard library resolves imports (including
`apprafter.io/schemas/v1alpha1`) from a vendored copy embedded in
the sidecar image. The resulting YAML — one or more Kubernetes
resource documents separated by `---` — is returned to Argo CD and
processed like any other manifest source.

The sidecar is installed and kept up to date by the platform-stack
chart; no manual sidecar configuration is required on your part.

## Troubleshooting compile errors

When CUE compilation fails, Argo CD surfaces the error in the
Application sync status. The sync operation shows `ComparisonError`
or `SyncError`; the full error output is in the sync log.

### Viewing the error

In the Argo CD UI: open the Application → click the sync status
badge → expand the error accordion. The CMP wrapper normalizes
CUE errors to a single-line summary in the badge, with the full
`cue export` stderr in the expandable details.

From the CLI:

```sh
# Check Argo CD Application sync state.
kubectl get applications.argoproj.io <app-name> -n argocd \
    -o jsonpath='{.status.conditions}'

# Tail CMP sidecar logs for the full cue output.
kubectl logs -n argocd \
    -l app.kubernetes.io/name=argocd-repo-server \
    -c argocd-cue-cmp --tail=50
```

### Common errors

**`package "apprafter.io/schemas/v1alpha1" not found`**

The import path must match exactly. Confirm the `package` directive
at the top of your file and the import string. The sidecar bundles
the schemas — do not vendor them yourself in the app repository
unless you are also supplying a `cue.mod/module.cue` with
`replace` directives.

**`field not allowed: <field-name>`**

The CUE schema uses `close()` semantics in some contexts. Check
that you are only writing fields declared in `#ApplicationSpec`.
Fields from future spec versions (`needs`, `autoscale`,
`confidential`) are not yet in v1alpha1 and will produce this
error.

**`conflicting values ... (mismatched types)`**

A CUE unification conflict. Typically caused by assigning a string
to an integer field or vice versa. Check `expose.port` (integer),
`replicas` (integer), and `env` values (must be strings).

**Admission-webhook rejection after sync**

If CUE compiles cleanly but the operator status shows a webhook
error, the issue is a cross-field invariant the CRD OpenAPI schema
cannot express. Common causes:

- `image` is empty in `spec.base` and not present in every
  `spec.environments[*]` entry. Every environment that lacks its own
  `image` inherits from base; if base has no image either, the
  webhook rejects the CR.
- `env` key does not match `^[A-Z_][A-Z0-9_]*$`. Lowercase or
  special characters in env-var keys are rejected.
- `env` key contains a character outside DNS-1123 (dot, slash, etc.).

## Registering your application

After committing and pushing `apprafter/Application.cue`, register
the repository with Argo CD so it tracks your repo:

```sh
# From the root of your application repository:
apprafter app add

# Or with an explicit URL:
apprafter app add https://github.com/my-org/my-service.git
```

`app add` creates an Argo CD `Application` CR that points at your
repository. The CUE CMP kicks in on the first sync and the
AppRafter operator reconciles the generated `Application` CR into
a running Deployment.

## Where to look next

- [`schemas/v1alpha1/application.cue`](https://github.com/apprafter/apprafter/blob/main/schemas/v1alpha1/application.cue)
  — the CUE schema `#Application` and `#ApplicationSpec` are defined
  here. This is the authoritative field list.
- [`operator/operator-core/src/application.rs`](https://github.com/apprafter/apprafter/blob/main/operator/operator-core/src/application.rs)
  — the Rust mirror of the schema; `APPRAFTER_ENV` selection and the
  per-environment merge semantics are implemented here.
- [ADR 0029](../adr/0029-cue-cmp.md) — CUE CMP design rationale.
- [`examples/applications/parser.cue`](https://github.com/apprafter/apprafter/blob/main/examples/applications/parser.cue)
  — a worked multi-environment example.
- [`docs/dev-guide/quickstart.md`](./quickstart.md) — scaffold and
  register a first Application end-to-end.
