# Landing: releasing and content promotion

Both landing applications run **on the AppRafter cluster**, deployed by Argo CD
from the manifests in this repository:

| | Manifest | Host |
|---|---|---|
| Web (production) | `landing/web/apprafter/Application.cue` | `apprafter.dev` |
| Web (preview) | `landing/web/apprafter/Application-preview.cue` | internal only |
| CMS | `landing/cms/apprafter/Application.cue` | `cms.apprafter.dev` |

What each piece of that chain is responsible for, and where every value comes
from, is
[Worked example: deploying the landing site and its CMS](../docs/how-it-works/deploying-the-landing-and-cms.md).

This file is the operator-side companion: how a release is cut, how a content
change reaches production, and how to check both afterwards.

## Releasing

Three workflows together cover the deploy flow — you usually
don't have to touch them by hand:

| Workflow | Trigger | Output |
|---|---|---|
| `landing-autotag.yml` | push to `master` with `landing/**` changes | bumps patch on latest `landing-v*` tag and pushes it |
| `release-landing.yml` | `landing-v*` tag push | builds + pushes `:landing-vX.Y.Z` + `:latest` for both images |
| `landing-preview-build.yml` | Payload `afterChange` on content globals | builds + pushes `:preview` + `:preview-<sha>` (web only, live CMS) |
| `landing-promote-to-prod.yml` | Publishing.promoteToProd checkbox | retags `:preview` → `:prod` + `:latest` (no rebuild, byte-identical image) |

So the normal flow is:

```sh
# Commit landing changes to master:
git push origin master

# landing-autotag fires (because landing/** changed):
#   landing-v0.1.5 → tagged at the same commit
#
# release-landing fires (because a landing-v* tag landed):
#   ghcr.io/apprafter/landing-web:landing-v0.1.5  (+ :latest)
#   ghcr.io/apprafter/landing-cms:landing-v0.1.5  (+ :latest)
#
# Argo CD on the cluster watches :latest (or pin to the explicit
# tag) and rolls out the new image.
```

No repo secret needed for the auto-fire chain. `GITHUB_TOKEN`
alone can't trigger `push: tags`-listening workflows (GitHub's
anti-recursion), but it CAN call `workflow_dispatch` — that is
an explicit exception to the rule. landing-autotag exploits it
by running `gh workflow run release-landing.yml --ref <new-tag>`
right after pushing the tag, which fires release-landing with
`github.ref_name = <new-tag>` exactly as a tag-push would.

If you ever need to re-fire release-landing by hand (e.g. a
build flaked, an orphaned tag from before this workflow shape):

```sh
gh workflow run release-landing.yml --ref landing-v0.1.5
```

### Manual / patched tags

For minor/major bumps (auto-tag is patch-only) or to skip the
auto-tag system entirely:

```sh
git tag landing-v0.2.0       # next minor — by hand
git push origin landing-v0.2.0
# auto-tag will pick up from here on the next push: 0.2.1, 0.2.2…
```

### Initial-pull note

First push of each package on GHCR makes it private. Flip to
public once via the GitHub UI: Packages → <package> → Settings →
Change visibility → Public. After that the host (or in-cluster
runtime) can pull without auth.

## Building from source (fallback path)

If you don't want to tag a release yet, build locally:

```sh
cd landing
bun install --frozen-lockfile

# Web — uses fallback JSONs unless LANDING_USE_FALLBACK=0 is set
# and the CMS is reachable. Defaults are correct for offline
# image builds.
docker build -f web/Dockerfile -t landing-web:dev .

# CMS — Next standalone output; runtime is node:22-alpine.
docker build -f cms/Dockerfile -t landing-cms:dev .

# Smoke-run locally on the same ports as the dev workflow:
docker run --rm -p 4321:80 landing-web:dev
docker run --rm -p 3000:3000 \
  -e DATABASE_URI=postgres://... \
  -e PAYLOAD_SECRET=... \
  landing-cms:dev
```


## Content-driven rebuilds — preview / promote / prod

Three tag streams on `landing-web`:

| Tag | Stream | Updated by |
|---|---|---|
| `:preview` + `:preview-<sha>` | every CMS save | `landing-preview-build.yml` — builds with `LANDING_USE_FALLBACK=0` + `LANDING_CMS_URL=https://cms.apprafter.dev` (live fetch) |
| `:prod` + `:latest` | every Publish in admin | `landing-promote-to-prod.yml` — retags `:preview` → `:prod` + `:latest` via `docker buildx imagetools create`, no rebuild |
| `:landing-vX.Y.Z` | every release tag | `release-landing.yml` — independent pinned-release path |

Argo CD on the cluster watches `:latest` for the production app
(`landing-web` Application — `landing/web/apprafter/Application.cue`)
and `:preview` for the preview app (`landing-web-preview` Application
— `landing/web/apprafter/Application-preview.cue`, same package,
both vet in one `cue vet ./landing/web/apprafter/` pass).

The preview application is `network: internal` — it has no public route at
all, which is what makes it safe to point at unreviewed content. Reach it
with a port-forward:

```sh
kubectl -n apprafter port-forward deploy/landing-web-preview 8080:80
```

### Promote flow (admin)

1. Admin edits any content global → `notifyRebuild` stamps
   `Publishing.lastEditAt` + `lastEditedGlobal` and fires the
   `landing-content-changed` dispatch.
2. `landing-preview-build.yml` rebuilds and pushes `:preview`
   within ~2–3 min (cache hits).
3. Admin opens the Publishing global, inspects the diff:
   - `lastEditAt` = newest content save
   - `lastPromotedAt` = newest prod promote
   - if `lastEditAt > lastPromotedAt`, preview is ahead.
   - `editLog` shows the last 20 edits since the most recent
     promote (cleared on each Promote), with `{at, global, editor}`
     per entry so reviewers can spot exactly what's pending.
4. Admin reviews `preview.apprafter.dev` (gated host).
5. Ticks `promoteToProd` checkbox and saves.
6. `promoteToProd` beforeChange fires
   `landing-promote-to-prod` dispatch, stamps `lastPromotedAt`,
   and resets the checkbox to `false`.
7. `landing-promote-to-prod.yml` retags `:preview` → `:prod` +
   `:latest`. Same image bytes as preview — no rebuild, no drift.
8. Argo CD pulls the new digest and rolls out.

Failure modes the design covers:
- GitHub outage during `notifyRebuild` → save still persists,
  Publishing.lastEditAt still stamped, dispatch failure logged.
  Re-trigger via `gh workflow run landing-preview-build.yml`.
- `:preview` doesn't exist yet (first promote) →
  `landing-promote-to-prod.yml` fails with a clear error
  ("trigger landing-preview-build first").
- Operator promotes by accident → next save in any content
  global re-creates `:preview`, then a second Promote click
  fixes prod.

Argo CD is what pulls each new digest — there is no timer, no
`podman auto-update`, and nothing to configure on a host. The operator
resolves the tag to its current digest on every reconcile, so a retag rolls
the Deployment by itself.

### CMS-side wiring

The Payload hook lives at `landing/cms/src/hooks/notifyRebuild.ts`
and fires on every content global's afterChange. It POSTs to
GitHub's `repository_dispatch` API with type
`landing-content-changed`, which the workflow listens for.

For the hook to fire, set `GITHUB_DISPATCH_TOKEN` and `GITHUB_REPO`
on the CMS deployment. Without them the hook logs a warning and skips —
useful in dev.

### Manual rebuild (escape hatch)

```sh
# Either trigger the workflow by hand:
gh workflow run landing-preview-build.yml

# Or send the dispatch event directly (mirrors what the Payload
# hook does):
curl -X POST \
  -H "Accept: application/vnd.github+json" \
  -H "Authorization: Bearer $GITHUB_DISPATCH_TOKEN" \
  -H "X-GitHub-Api-Version: 2022-11-28" \
  https://api.github.com/repos/AppRafter/apprafter/dispatches \
  -d '{"event_type":"landing-content-changed"}'
```

The workflow has `concurrency: cancel-in-progress: true`, so
rapid-fire edits coalesce to a single final build.

## Backups

The CMS database is a `needs.pg` claim on the cluster, so it is captured by
the platform's own backup — `apprafter backup create`, and the scheduled
off-site push if it is enabled. There is no separate cron for it.

## Verification after a deploy

```sh
curl -I https://apprafter.dev/                  # 200, gzip
curl -I https://apprafter.dev/privacy/          # 200
curl -I https://apprafter.dev/sitemap-index.xml # 200
curl -I https://cms.apprafter.dev/admin/        # 200 (login screen)
curl -X POST -H 'Content-Type: application/json' \
  -d '{"email":"deploy-smoke@apprafter.dev"}' \
  https://apprafter.dev/api/waitlist-signups    # 201
```

The waitlist POST should land in the admin under `Collections → Waitlist
Signups`. Delete the test entry afterwards.

`apprafter app status landing-web` and `apprafter app status landing-cms` are
the cluster-side view of the same question.
