---
description: "Wiring a bootstrap repository end to end, for each combination of GitHub or GitLab and public or private."
---

# Connect a Git repository — GitHub or GitLab, public or private

This guide walks a cluster operator through wiring `spec.argocd.bootstrapRepo`
for each combination of `(GitHub | GitLab) × (public | private)`. Goal is one
runbook the operator can follow start-to-finish without lateral reading.

## End-to-end flow

AppRafter uses a two-layer GitOps model:

1. **Platform layer** — the platform stack (Cilium, cert-manager, AppRafter
   operator, Argo CD itself) is managed by the `PlatformStack` CR and
   reconciled by Argo CD from a versioned OCI chart. This layer is set up
   by `apprafter cluster-bootstrap` and requires no Git repository from you.

2. **Application layer** — your application repositories contain CUE or
   YAML manifests. When you register a repository with `apprafter app add`,
   Argo CD creates an `Application` CR in the `apps` AppProject that tracks
   your repo. The CUE CMP sidecar compiles `apprafter/Application.cue` into
   an AppRafter `Application` CR YAML on every sync; the AppRafter operator
   reconciles that CR into a Deployment and Service.

The `spec.argocd.bootstrapRepo` flow described in this runbook wires an Argo
CD `Application` that Argo CD manages directly from a Git repository — useful
for platform-level manifests or for operators who prefer raw YAML or nested
Argo CD `Application` manifests in their bootstrap repository.

## Prerequisites (all quadrants)

- An AppRafter cluster bootstrapped via `apprafter` against Hetzner Cloud.
  Operator + admission-webhook are installed as part of the platform stack;
  see the [operator quickstart](./quickstart.md).
- A Git repository containing at least one manifest at the path
  you will target with `bootstrapPath` (or the repo root if not set).
  AppRafter `Application` CRs, raw `Deployment` / `Service` manifests, or
  nested Argo CD `Application` manifests all work. A bare empty repo
  syncs as a no-op.
- `kubectl` configured against the cluster (run `apprafter kubeconfig
  | tee /tmp/kc` and `export KUBECONFIG=/tmp/kc`).
- For the **private** quadrants: ability to generate a PAT (Personal Access
  Token) on the platform, scoped to read the bootstrap repo.

The `spec.argocd.bootstrapRepo` field accepts an HTTPS URL like
`https://github.com/org/repo.git` (works for both GitHub and GitLab). SSH
URLs (`git@github.com:org/repo.git`) are NOT supported in this cycle —
PAT-over-HTTPS is the only auth method.

**Note on `kubectl get application ...`:** AppRafter ships its own
`applications.apprafter.io` CRD (workload definitions consumed by the
operator), which shadows Argo CD's `applications.argoproj.io` short name.
`kubectl get application bootstrap -n argocd` therefore resolves to our
CRD and returns `NotFound`. Always use the fully-qualified Argo CD form
in this runbook: `kubectl get applications.argoproj.io bootstrap -n argocd`.

## Quadrant 1: GitHub × public

### Steps

1. Create or pick a public GitHub repository, e.g. `https://github.com/your-org/state.git`.
2. Add an AppRafter `Application` CR at a `manifests/` subpath.
   The CMP sidecar is not involved here because the bootstrap repo is
   tracked directly by Argo CD as a raw manifest source; place the
   compiled YAML directly:

   ```yaml
   # manifests/hello.yaml
   apiVersion: apprafter.io/v1alpha1
   kind: Application
   metadata:
     name: gitops-hello
     namespace: apprafter
     labels:
       apprafter.io/managed-by: apprafter
   spec:
     base:
       image: nginxdemos/hello:plain-text
       replicas: 1
       expose:
         port: 80
         network: internal
   ```

   The AppRafter operator reconciles this CR into a Deployment and
   Service via server-side apply; the admission webhook validates
   it on create and update.

   Operators who prefer a nested Argo CD `Application` layer (to
   manage multiple child apps from one bootstrap repo) can put Argo
   CD `Application` manifests in `manifests/` instead. The bootstrap
   App syncs whatever the path resolves to.

3. In your `Infrastructure.cue` manifest, set:

   ```cue
   spec: argocd: {
       // domain: "argo.example.com"   // optional — Argo CD UI exposure
       bootstrapRepo: "https://github.com/your-org/state.git"
       bootstrapPath: "manifests"
   }
   ```

4. Run the full bootstrap flow:

   ```sh
   apprafter bootstrap-all     # or: apprafter apply && apprafter kubeconfig && apprafter cluster-bootstrap
   apprafter kubeconfig > /tmp/kc && export KUBECONFIG=/tmp/kc
   ```

   The `cluster-bootstrap` summary line ends with
   `+ bootstrap Application from <repo>` and (for public repos)
   does NOT include `+ Argo CD repo-creds Secret`.

### Checklist — did it work?

Check the **two surfaces AppRafter ships** for this
feature — the Argo CD web UI (`cluster-bootstrap` installs it) and the
`apprafter argocd-password` subcommand. `kubectl` checks are
sanity-only supplements.

**1. Argo CD UI check (primary)** — open the UI and visually verify:

```sh
# Starts a local port-forward, prints credentials, and opens the browser.
apprafter open argocd
```

Log in as `admin` / `<password from CLI output>` and confirm:

- [ ] One Application visible in the list: `bootstrap`.
- [ ] Status badge: **Synced** (green).
- [ ] Status badge: **Healthy** (green).
- [ ] Drill into the `bootstrap` App → the resource tree shows every
      manifest in `bootstrapPath` (the AppRafter `Application` CR
      and, after operator reconciliation, the child Deployment and
      Service), each node green.
- [ ] Expand the AppRafter `Application` CR node; its `status.phase`
      reads `Ready`.

**2. kubectl sanity checks (secondary)** — same state, machine-readable:

- [ ] `kubectl get applications.argoproj.io bootstrap -n argocd -o jsonpath='{.status.sync.status}'` → `Synced`.
- [ ] `kubectl get applications.argoproj.io bootstrap -n argocd -o jsonpath='{.status.health.status}'` → `Healthy`.
- [ ] `kubectl get applications.argoproj.io -A` lists `bootstrap` (and any child Argo CD `Application`s the repo defines).
- [ ] `kubectl get applications.apprafter.io gitops-hello -n apprafter -o jsonpath='{.status.phase}'` → `Ready`.
- [ ] `kubectl get secret apprafter-bootstrap-repo-creds -n argocd` returns `NotFound` (public repo path does not create a Secret).

### Troubleshooting

| Symptom                                    | Likely cause                              | Fix                                                                       |
| ------------------------------------------ | ----------------------------------------- | ------------------------------------------------------------------------- |
| `apprafter open argocd` fails to connect   | cluster not reachable or kubeconfig stale  | Re-run `apprafter kubeconfig --refresh` and retry.                        |
| UI login rejects password                  | copy-paste added trailing whitespace       | Re-run `apprafter argocd-password` and copy the exact line.              |
| bootstrap App stuck `OutOfSync`            | path mismatch                              | Verify `bootstrapPath` matches the actual folder in the repo.            |
| bootstrap App `Unknown` health             | Argo CD pod not ready yet                  | `kubectl wait --for=condition=Ready -n argocd pod -l app.kubernetes.io/name=argocd-server --timeout=120s` |
| AppRafter `Application` CR stuck `AwaitingMigrationApproval` | destructive change gated | Run `apprafter migration list` and approve or revert the Git commit.   |
| `Application not found` error              | Argo CD's repository scanner missed it     | `kubectl describe applications.argoproj.io bootstrap -n argocd` — check `conditions`. |

## Quadrant 2: GitLab × public

### Steps

Same as Quadrant 1, but with `https://gitlab.com/your-group/state.git` as the
repo URL. GitLab also supports nested groups: `https://gitlab.com/your-group/sub-group/state.git`.

### Checklist — did it work?

Identical to Quadrant 1.

### Troubleshooting

Identical to Quadrant 1, with one addition:

| Symptom                                    | Likely cause                              | Fix                                                                       |
| ------------------------------------------ | ----------------------------------------- | ------------------------------------------------------------------------- |
| Clone hangs                                | GitLab.com rate-limited the IP             | Wait a few minutes; check https://status.gitlab.com.                     |

## Quadrant 3: GitHub × private

### Steps

1. Repo: `https://github.com/your-org/private-state.git` (visibility: Private).
2. Generate a **fine-grained PAT** at https://github.com/settings/personal-access-tokens/new:
   - **Resource owner**: the user or org that owns the repo.
   - **Repository access**: select only the bootstrap repo.
   - **Repository permissions** → **Contents**: `Read-only`.
   - **Repository permissions** → **Metadata**: `Read-only` (auto-required).
   - Expiration: pick a duration your security policy allows. AppRafter has no automatic rotation — you re-supply a fresh token when this one expires.
3. Copy the PAT (starts with `github_pat_...`).
4. In your shell:

   ```sh
   export APPRAFTER_ARGOCD_REPO_TOKEN='github_pat_...'
   # Optional — defaults to "apprafter" if unset:
   # export APPRAFTER_ARGOCD_REPO_USERNAME='your-github-handle'
   ```

5. Set `spec.argocd.bootstrapRepo` in `Infrastructure.cue` exactly as in Quadrant 1.
6. Run `apprafter cluster-bootstrap`. The summary line at the end includes `+ Argo CD repo-creds Secret in argocd namespace` confirming the Secret was applied.

### Checklist — did it work?

Same two-surface pattern as Quadrant 1, plus a Secret-presence check
(`kubectl` only, since AppRafter doesn't ship a UI for repo creds).

**1. Argo CD UI check (primary)** — open via CLI:

```sh
apprafter open argocd
```

Log in as `admin` / `<password from CLI output>`, then on the `bootstrap` App:

- [ ] Status: **Synced** + **Healthy** (green).
- [ ] In the resource tree, no node shows a `ComparisonError` mentioning
      401 / 403 (Argo CD would surface a credential failure here).
- [ ] Drill-down resources match the private repo's content.
- [ ] AppRafter `Application` CR node (if the repo contains one) shows
      `status.phase=Ready`.

**2. kubectl sanity checks**:

- [ ] `kubectl get secret apprafter-bootstrap-repo-creds -n argocd` returns the Secret.
- [ ] `kubectl get applications.argoproj.io bootstrap -n argocd -o jsonpath='{.status.sync.status}'` → `Synced`.
- [ ] `kubectl get applications.argoproj.io -A` lists `bootstrap` (and any child Argo CD `Application`s the repo defines).

### Troubleshooting

| Symptom                                    | Likely cause                              | Fix                                                                       |
| ------------------------------------------ | ----------------------------------------- | ------------------------------------------------------------------------- |
| `401 Unauthorized` in UI / `kubectl describe app bootstrap` | PAT expired or wrong               | Generate a new PAT, re-export `APPRAFTER_ARGOCD_REPO_TOKEN`, re-run `cluster-bootstrap`. |
| `404 Not Found`                            | PAT lacks access to that specific repo     | In GitHub settings, edit the PAT to grant access to the bootstrap repo.   |
| `Secret apprafter-bootstrap-repo-creds not created` | env-var was empty when `cluster-bootstrap` ran | `echo "${APPRAFTER_ARGOCD_REPO_TOKEN:0:5}…"` to verify it's set, re-run. |
| Sync stuck > 30s after bootstrap            | Argo CD reconcile interval (~3min default) | Click `REFRESH` in the UI (or `kubectl annotate applications.argoproj.io bootstrap -n argocd argocd.argoproj.io/refresh=hard --overwrite`). Alternatively, re-open with `apprafter open argocd` and use the UI Refresh button. |

## Quadrant 4: GitLab × private

### Steps

1. Repo: `https://gitlab.com/your-group/private-state.git` (visibility: Private).
2. Generate a **Project Access Token** in GitLab UI → Project → Settings → Access Tokens:
   - **Token name**: `apprafter-bootstrap` (any).
   - **Role**: `Reporter` (sufficient for read-only clone).
   - **Scopes**: `read_repository`.
   - **Expiration**: pick a duration your security policy allows.
3. Copy the token (starts with `glpat-...`).
4. Export the env-vars + set `Infrastructure.cue` + run `cluster-bootstrap` (same as Quadrant 3).
5. Optional: prefer Project Access Token over personal PAT when the repo is owned by a project rather than a user — easier to rotate without affecting other access.

### Checklist — did it work?

Identical to Quadrant 3.

### Troubleshooting

| Symptom                                    | Likely cause                              | Fix                                                                       |
| ------------------------------------------ | ----------------------------------------- | ------------------------------------------------------------------------- |
| `401 Unauthorized`                         | Token expired, revoked, or scope wrong     | Regenerate with `read_repository` scope, re-export, re-run.              |
| `404 Not Found`                            | Group-level token used against a project   | Use a Project Access Token, not Group Access Token, for single-project access. |
| `403 Forbidden`                            | Role too restrictive                       | Token role must be at least `Reporter`.                                  |

## Token rotation

When your PAT or Project Access Token expires:

```sh
export APPRAFTER_ARGOCD_REPO_TOKEN='<new-token>'
apprafter cluster-bootstrap
```

`cluster-bootstrap` is idempotent — the Secret is overwritten with the new
token value via `kubectl apply`. Argo CD picks up the change on its next
reconcile (within ~3 minutes by default; force with `kubectl annotate
applications.argoproj.io bootstrap -n argocd argocd.argoproj.io/refresh=hard --overwrite`).

## Revoking access

To remove the credentials from the cluster without re-running `cluster-bootstrap`:

```sh
kubectl delete secret apprafter-bootstrap-repo-creds -n argocd
```

The bootstrap `Application` stays declarative but its next reconcile will
fail with 401 until either creds are re-supplied or the repo is made public.
Argo CD's `selfHeal: true` will keep retrying.

To stop the bootstrap loop entirely, remove `spec.argocd.bootstrapRepo` from
your `Infrastructure.cue` and re-run `cluster-bootstrap` — the existing
bootstrap Argo CD `Application` is NOT auto-deleted (idempotent `kubectl apply`
semantics), so also run:

```sh
kubectl delete applications.argoproj.io bootstrap -n argocd
```
