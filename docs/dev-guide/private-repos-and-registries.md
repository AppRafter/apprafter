---
description: "The two authentication surfaces a private application touches — Argo CD's repository clone and the node's image pull — and the single credential that covers both."
---

# Private repos & registries

Shipping a private app touches **two distinct authentication
surfaces**, and they do not use the same token type on GitHub — this
is the most common source of bring-up grief:

1. **Source repo clone** — Argo CD's repo-server clones your
   `Application.cue` repo to render it.
2. **Container image pull** — the node (kubelet) pulls your image from
   the registry to run it.

A public repo and a public image need no credentials at all. The
table below is the whole decision space:

| | **Public** | **Private** |
| --- | --- | --- |
| **Source repo** (Argo CD clone) | nothing to do | register a git credential — see [Register once](#register-once) |
| **Container image** (node pull) | nothing to do | register a pull credential — same command, see [GHCR token scopes](#ghcr-token-scopes) |

## Register once, covers both {#register-once}

`apprafter repo creds add` registers **one** credential that the
operator uses for **both** surfaces. It seals your token client-side
(the CLI never holds the controller's key) into a `SourceCredential`,
and the operator derives the Argo CD repo-cred and the workload
pull-secret from it:

```sh
apprafter repo creds add my-org \
    --url-prefix https://github.com/my-org \
    --token <PAT>          # omit to be prompted with masked entry
```

```text
✓ SourceCredential 'my-org' registered (material sealed).
  Repo prefix:   https://github.com/my-org
  Registry host: ghcr.io/my-org  (inferred)

The operator derives the Argo repo-cred + workload pull-secret. Check validity with:
  apprafter repo creds show my-org
```

Every Application whose `repoURL` (and, for GitHub, whose `ghcr.io`
image path) starts with the registered prefix inherits the
credential. For a **GitHub** org the registry host is inferred
automatically (`github.com/my-org` → `ghcr.io/my-org`), so the single
token has to be valid for **both** GitHub Git access **and** GHCR —
which dictates the token type below.

## GHCR token scopes {#ghcr-token-scopes}

GHCR (`ghcr.io`) is strict about token types. Because the single
credential above must also pull from GHCR, the launch default is a
**classic** personal access token:

- ✅ **Classic PAT** with **`read:packages`** (to pull images), plus
  **`repo`** when the package inherits a private repository's
  visibility or you also clone a private repo with the same token.
- ❌ **Fine-grained PATs** — GHCR does not accept them (there is no
  packages permission in the fine-grained model); pulls fail with
  `403`.
- ❌ **GitHub App installation tokens** — work for the API and Git,
  but **not** for `ghcr.io`.

This is a GitHub-side constraint, not an AppRafter one — the platform
stores whatever token you give it securely, but GHCR will reject the
wrong type at pull time (the app lands in `ImagePullBackOff`). Create
a classic PAT at **Settings → Developer settings → Personal access
tokens → Tokens (classic)**.

!!! note "Pulling from a registry that is *not* GHCR?"
    Docker Hub, ECR, GCR, GitLab Container Registry, Gitea/Forgejo and
    friends each have their own scope rules. The single-PAT inference
    only applies to the GitHub → GHCR pairing; for other registries
    register the registry credential explicitly with a `--url-prefix`
    matching that host, and follow that provider's auth guide.

## Private source repo only (no private image)

If your image is public but the **source repo** is private, you only
need the Git-clone half — and it is the same `apprafter repo creds add`
as above, run once after the cluster is up. There is no separate
bootstrap-time path: a repository becomes the cluster's business when
you register an application from it with `apprafter app add`, never
before.

A private repo clone on GitHub also accepts a **fine-grained PAT**
(Contents: Read-only) *if that credential is used for Git only* — but
if the same prefix also pulls a private GHCR image, fall back to the
classic PAT per the rule above.

GitLab private repos use a **Project Access Token** with
`read_repository`; self-hosted Gitea/Forgejo use an arbitrary token
format, so pass `--no-validate` to `repo creds add` to skip the
GitHub/GitLab format regex.

## Verify and rotate

```sh
apprafter repo creds list           # registered credentials + their prefixes
apprafter repo creds show my-org    # validity status (token masked)
apprafter repo creds rotate my-org  # in-place token swap (no Argo reconnect window)
apprafter repo creds remove my-org  # refuses if apps still depend on the prefix; --force overrides
```

`apprafter repo creds show` reports the operator's validity verdict —
if it isn't `GitValid=True` (and, for a registry, the pull check
passing), Argo CD or the kubelet will fail at clone/pull time.

## Troubleshooting

- **`ImagePullBackOff`** on a private image → almost always a wrong
  token type for GHCR (fine-grained instead of classic) or a missing
  `read:packages` scope. See
  [Troubleshooting → registry auth](../operator-guide/troubleshooting.md#registry-auth).
- **`401` / `404`** in the Argo CD UI on the source repo → the
  Git credential is expired, lacks repo access, or wasn't registered
  for the matching `--url-prefix`. Run `apprafter repo creds show` and
  read the validity verdict: anything other than `GitValid=True` is the
  answer, and [Verify and rotate](#verify-and-rotate) above has the
  rotate/re-register commands. Treat a `404` on a repository you can
  browse in a web UI as an auth failure rather than a missing repo —
  GitHub and GitLab both answer `404` instead of `403` for a private
  repository the credential may not read, so the cause is either no
  registered prefix matching this `repoURL` or a token without access
  to it.
