# AppRafter GitOps walk — bootstrap from Git

This guide walks a cluster operator through wiring `spec.argocd.bootstrapRepo`
for each combination of `(GitHub | GitLab) × (public | private)`. Goal is one
runbook the operator can follow start-to-finish without lateral reading.

## Prerequisites (all quadrants)

- An AppRafter cluster bootstrapped via `platform-cli` against Hetzner Cloud.
  Operator + admission-webhook install by default since v0.1.64; see the
  [operator quickstart](./quickstart.md).
- A Git repository containing at least one Argo CD `Application` manifest at
  the path you'll target with `bootstrapPath` (or the repo root if not set).
  A bare empty repo will sync as a no-op — operator-facing UX is identical.
- `kubectl` configured against the cluster (run `platform-cli kubeconfig
  | tee /tmp/kc` and `export KUBECONFIG=/tmp/kc`).
- For the **private** quadrants: ability to generate a PAT (Personal Access
  Token) on the platform, scoped to read the bootstrap repo.

The `spec.argocd.bootstrapRepo` field accepts an HTTPS URL like
`https://github.com/org/repo.git` (works for both GitHub and GitLab). SSH
URLs (`git@github.com:org/repo.git`) are NOT supported in this cycle —
PAT-over-HTTPS is the only auth method.

## Quadrant 1: GitHub × public

### Steps

1. Create or pick a public GitHub repository, e.g. `https://github.com/your-org/state.git`.
2. Add an Argo CD `Application` manifest to the repo root (or a subpath you'll point at):

   ```yaml
   # apps/hello.yaml — a minimal child app
   apiVersion: argoproj.io/v1alpha1
   kind: Application
   metadata:
     name: hello
     namespace: argocd
   spec:
     project: default
     source:
       repoURL: https://github.com/your-org/state.git
       targetRevision: HEAD
       path: apps/hello
     destination:
       server: https://kubernetes.default.svc
       namespace: default
     syncPolicy:
       automated:
         prune: true
         selfHeal: true
   ```

3. In your `Infrastructure.cue` manifest, set:

   ```cue
   spec: argocd: {
       domain:        "argo.example.com"  // optional — Argo CD UI exposure
       bootstrapRepo: "https://github.com/your-org/state.git"
       bootstrapPath: "."                  // or "clusters/tier-1" etc.
   }
   ```

4. Run `platform-cli cluster-bootstrap`.

### DoD checklist

- [ ] `kubectl get application bootstrap -n argocd -o jsonpath='{.status.sync.status}'` returns `Synced`.
- [ ] `kubectl get application bootstrap -n argocd -o jsonpath='{.status.health.status}'` returns `Healthy`.
- [ ] If `apps/` contains an Argo CD `Application` manifest, that child Application appears in `kubectl get applications.argoproj.io -A`.
- [ ] No `Secret` exists in `argocd` namespace with name `apprafter-bootstrap-repo-creds` (public repo path does not need it).

### Troubleshooting

| Symptom                                    | Likely cause                              | Fix                                                                       |
| ------------------------------------------ | ----------------------------------------- | ------------------------------------------------------------------------- |
| bootstrap App stuck `OutOfSync`            | path mismatch                              | Verify `bootstrapPath` matches the actual folder in the repo.            |
| bootstrap App `Unknown` health            | Argo CD pod not ready yet                  | `kubectl wait --for=condition=Ready -n argocd pod -l app.kubernetes.io/name=argocd-server --timeout=120s` |
| `Application not found` error             | Argo CD's repository scanner missed it     | `kubectl describe application bootstrap -n argocd` — check `conditions`. |

## Quadrant 2: GitLab × public

### Steps

Same as Quadrant 1, but with `https://gitlab.com/your-group/state.git` as the
repo URL. GitLab also supports nested groups: `https://gitlab.com/your-group/sub-group/state.git`.

### DoD checklist

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
6. Run `platform-cli cluster-bootstrap`. The summary line at the end includes `+ Argo CD repo-creds Secret in argocd namespace` confirming the Secret was applied.

### DoD checklist

- [ ] `kubectl get secret apprafter-bootstrap-repo-creds -n argocd` exists.
- [ ] `kubectl get application bootstrap -n argocd -o jsonpath='{.status.sync.status}'` returns `Synced`.
- [ ] Argo CD UI (or `kubectl get application bootstrap -n argocd -o yaml`) shows no `ComparisonError` referencing 401/403.
- [ ] If the repo contains an Argo CD `Application` manifest, that child Application appears in `kubectl get applications.argoproj.io -A`.

### Troubleshooting

| Symptom                                    | Likely cause                              | Fix                                                                       |
| ------------------------------------------ | ----------------------------------------- | ------------------------------------------------------------------------- |
| `401 Unauthorized`                         | PAT expired or wrong                       | Generate a new PAT, re-export `APPRAFTER_ARGOCD_REPO_TOKEN`, re-run `cluster-bootstrap`. |
| `404 Not Found`                            | PAT lacks access to that specific repo     | In GitHub settings, edit the PAT to grant access to the bootstrap repo.   |
| `Secret apprafter-bootstrap-repo-creds not created` | env-var was empty when `cluster-bootstrap` ran | `echo "${APPRAFTER_ARGOCD_REPO_TOKEN:0:5}…"` to verify it's set, re-run. |
| Sync stuck > 30s after bootstrap            | Argo CD reconcile interval (~3min default) | `kubectl exec -n argocd deploy/argocd-repo-server -- argocd repo list` to force a probe; or wait. |

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

### DoD checklist

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
platform-cli cluster-bootstrap
```

`cluster-bootstrap` is idempotent — the Secret is overwritten with the new
token value via `kubectl apply`. Argo CD picks up the change on its next
reconcile (within ~3 minutes by default; force with `kubectl annotate
application bootstrap -n argocd argocd.argoproj.io/refresh=hard --overwrite`).

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
bootstrap `Application` is NOT auto-deleted (idempotent `kubectl apply`
semantics), so also run:

```sh
kubectl delete application bootstrap -n argocd
```
