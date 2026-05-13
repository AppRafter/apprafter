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
2. Add raw Kubernetes manifests at a `manifests/` subpath — Argo CD's
   bootstrap `Application` syncs them directly, no nested Argo CD
   `Application` layer needed for the smoke walk:

   ```yaml
   # manifests/hello.yaml
   apiVersion: apps/v1
   kind: Deployment
   metadata:
     name: gitops-hello
     namespace: default
     labels: { apprafter: "true", app: gitops-hello }
   spec:
     replicas: 1
     selector: { matchLabels: { app: gitops-hello } }
     template:
       metadata:
         labels: { app: gitops-hello, apprafter: "true" }
       spec:
         containers:
           - name: hello
             image: nginxdemos/hello:plain-text
             ports: [{ containerPort: 80 }]
   ---
   apiVersion: v1
   kind: Service
   metadata:
     name: gitops-hello
     namespace: default
     labels: { apprafter: "true" }
   spec:
     type: ClusterIP
     selector: { app: gitops-hello }
     ports: [{ port: 80, targetPort: 80 }]
   ```

   (Operators preferring a nested Argo CD `Application` layer — to
   manage multiple child apps from one bootstrap repo — can put
   Argo CD `Application` manifests in `manifests/` instead. The
   bootstrap App syncs whatever the path resolves to.)

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
   cd cli
   export APPRAFTER_MANIFEST=../examples/infrastructure/tier-1-hetzner.cue
   cargo run --bin platform-cli -- apply
   # wait ~3-5 min for cloud-init
   cargo run --bin platform-cli -- kubeconfig | tee /tmp/kc
   export KUBECONFIG=/tmp/kc
   cargo run --bin platform-cli -- cluster-bootstrap
   ```

   The `cluster-bootstrap` summary line ends with
   `+ bootstrap Application from <repo>` and (for public repos)
   does NOT include `+ Argo CD repo-creds Secret`.

### DoD checklist

The DoD must exercise the **two surfaces AppRafter ships** for this
feature — the Argo CD web UI (`cluster-bootstrap` installs it) and the
`platform-cli argocd-password` subcommand. `kubectl` checks are
sanity-only supplements.

**1. Argo CD UI walk (primary)** — open the UI and visually verify:

```sh
# Terminal A: port-forward the Argo CD server (keep running).
kubectl -n argocd port-forward svc/argocd-server 8080:443

# Terminal B: get the admin password via our CLI (NOT raw kubectl).
cd cli && cargo run --bin platform-cli -- argocd-password
```

Browse to `https://localhost:8080` (accept the self-signed cert
warning), log in as `admin` / `<password from CLI>`, and confirm:

- [ ] One Application visible in the list: `bootstrap`.
- [ ] Status badge: **Synced** (green).
- [ ] Status badge: **Healthy** (green).
- [ ] Drill into the `bootstrap` App → the resource tree shows every
      manifest in `bootstrapPath` (e.g. a `Deployment` + `Service`),
      each node green.

**2. kubectl sanity checks (secondary)** — same state, machine-readable:

- [ ] `kubectl get application bootstrap -n argocd -o jsonpath='{.status.sync.status}'` → `Synced`.
- [ ] `kubectl get application bootstrap -n argocd -o jsonpath='{.status.health.status}'` → `Healthy`.
- [ ] `kubectl get applications.argoproj.io -A` lists `bootstrap` (and any child Argo CD `Application`s the repo defines).
- [ ] `kubectl get secret apprafter-bootstrap-repo-creds -n argocd` returns `NotFound` (public repo path doesn't create a Secret).

### Troubleshooting

| Symptom                                    | Likely cause                              | Fix                                                                       |
| ------------------------------------------ | ----------------------------------------- | ------------------------------------------------------------------------- |
| `argocd-password` errors                   | state cache miss after fresh apply         | Re-run `cargo run --bin platform-cli -- argocd-password --refresh`.       |
| Browser can't reach `https://localhost:8080` | port-forward terminated                    | Re-run `kubectl -n argocd port-forward svc/argocd-server 8080:443`.       |
| UI login rejects password                  | copy-paste added trailing whitespace       | Re-run `argocd-password` and copy the exact line.                         |
| bootstrap App stuck `OutOfSync`            | path mismatch                              | Verify `bootstrapPath` matches the actual folder in the repo.            |
| bootstrap App `Unknown` health             | Argo CD pod not ready yet                  | `kubectl wait --for=condition=Ready -n argocd pod -l app.kubernetes.io/name=argocd-server --timeout=120s` |
| `Application not found` error              | Argo CD's repository scanner missed it     | `kubectl describe application bootstrap -n argocd` — check `conditions`. |

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

Same two-surface pattern as Quadrant 1, plus a Secret-presence check
(`kubectl` only, since AppRafter doesn't ship a UI for repo creds).

**1. Argo CD UI walk (primary)** — port-forward + `platform-cli argocd-password`:

```sh
# Terminal A
kubectl -n argocd port-forward svc/argocd-server 8080:443
# Terminal B
cd cli && cargo run --bin platform-cli -- argocd-password
```

Open `https://localhost:8080`, log in, then on the `bootstrap` App:

- [ ] Status: **Synced** + **Healthy** (green).
- [ ] In the resource tree, no node shows a `ComparisonError` mentioning
      401 / 403 (Argo CD would surface a credential failure here).
- [ ] Drill-down resources match the private repo's content.

**2. kubectl sanity checks**:

- [ ] `kubectl get secret apprafter-bootstrap-repo-creds -n argocd` returns the Secret.
- [ ] `kubectl get application bootstrap -n argocd -o jsonpath='{.status.sync.status}'` → `Synced`.
- [ ] `kubectl get applications.argoproj.io -A` lists `bootstrap` (and any child Argo CD `Application`s the repo defines).

### Troubleshooting

| Symptom                                    | Likely cause                              | Fix                                                                       |
| ------------------------------------------ | ----------------------------------------- | ------------------------------------------------------------------------- |
| `401 Unauthorized` in UI / `kubectl describe app bootstrap` | PAT expired or wrong               | Generate a new PAT, re-export `APPRAFTER_ARGOCD_REPO_TOKEN`, re-run `cluster-bootstrap`. |
| `404 Not Found`                            | PAT lacks access to that specific repo     | In GitHub settings, edit the PAT to grant access to the bootstrap repo.   |
| `Secret apprafter-bootstrap-repo-creds not created` | env-var was empty when `cluster-bootstrap` ran | `echo "${APPRAFTER_ARGOCD_REPO_TOKEN:0:5}…"` to verify it's set, re-run. |
| Sync stuck > 30s after bootstrap            | Argo CD reconcile interval (~3min default) | Click `REFRESH` in the UI (or `kubectl annotate application bootstrap -n argocd argocd.argoproj.io/refresh=hard --overwrite`). |

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
