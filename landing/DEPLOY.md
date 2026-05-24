# Landing deployment

Target deploy (per Q1 + Q2, 2026-05-22):

- **Host:** the same Hetzner VPS that runs the rest of the AppRafter
  community/dev infrastructure.
- **Reverse proxy:** Caddy.
- **Two upstreams behind Caddy:**
  - `apprafter.dev` → static files from `landing/web/dist/` served
    directly by Caddy with gzip + zstd.
  - `cms.apprafter.dev` → `next start` from `landing/cms/.next/`,
    listening on `localhost:${LANDING_CMS_PORT}` (default 3000),
    behind Caddy as a reverse proxy.
- **Database:** standalone Postgres 16 container — separate from
  any future shared platform Postgres; the landing CMS has its own
  data store and lifecycle.
- **Mail:** `nodemailer` over an SMTP relay (Resend / Postmark /
  similar — chosen at deploy time, secret injected via env).

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

For the auto-fire chain to work, **the auto-tag has to push the
tag as a "user"**, not as the default `GITHUB_TOKEN` (which is
explicitly blocked from triggering downstream workflows).
Set `LANDING_AUTOTAG_PAT` repo secret with a fine-grained PAT —
see the workflow's header comment for the exact scope. Without
the PAT, the tag is still created (visible in the repo); the
operator just re-runs the release manually:

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

## One-time setup

```sh
# 1) Pull the images on the deploy host (or run via systemd-unit
#    below which pulls automatically on each restart).
podman pull ghcr.io/apprafter/landing-web:landing-v0.1.0
podman pull ghcr.io/apprafter/landing-cms:landing-v0.1.0

# 2) Wire the CMS env + Postgres (see following sections), then
#    start the systemd units (one for web, one for cms).
sudo systemctl daemon-reload
sudo systemctl enable --now apprafter-landing-web apprafter-landing-cms

# 3) Seed the content globals into the running CMS (one-shot;
#    idempotent — re-run after JSON edits if you want them
#    mirrored back into Payload). Either:
#    (a) inside the cms container:
podman exec apprafter-landing-cms node /app/seed.js
#    (b) or with a local bun checkout pointed at the prod CMS:
DATABASE_URI=... PAYLOAD_SECRET=... \
  bun --filter @apprafter/landing-cms run seed
```

The web image is content-static — it ships with the JSON
fallbacks baked in (`LANDING_USE_FALLBACK=1` during image build),
so it renders without depending on the live CMS at boot. To
refresh static output after content edits, cut a new
`landing-v*` tag (or trigger the workflow via `workflow_dispatch`)
and re-pull.

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

## Postgres container

```sh
sudo mkdir -p /srv/apprafter-cms-pg
sudo podman run -d --name apprafter-cms-pg \
  --restart=always \
  -e POSTGRES_DB=apprafter_cms \
  -e POSTGRES_USER=apprafter \
  -e POSTGRES_PASSWORD=$(pwgen -s 32 1) \
  -v /srv/apprafter-cms-pg:/var/lib/postgresql/data \
  -p 127.0.0.1:5432:5432 \
  docker.io/library/postgres:16
```

Keep the password in `/etc/apprafter-cms.env` (mode 0600, root + the
service user only). Add the matching `DATABASE_URI` there too.

## systemd unit for the CMS

`/etc/systemd/system/apprafter-cms.service`:

```ini
[Unit]
Description=AppRafter landing CMS (Payload 3 + Next 15)
After=network.target podman.socket
Wants=podman.socket

[Service]
Type=simple
User=apprafter-cms
Group=apprafter-cms
WorkingDirectory=/opt/apprafter-landing/cms
EnvironmentFile=/etc/apprafter-cms.env
ExecStart=/usr/local/bin/bun run start
Restart=on-failure
RestartSec=5s

[Install]
WantedBy=multi-user.target
```

`/etc/apprafter-cms.env` (0600):

```
PAYLOAD_SECRET=<64+ char random>
DATABASE_URI=postgres://apprafter:<pw>@127.0.0.1:5432/apprafter_cms
PAYLOAD_PUBLIC_SERVER_URL=https://cms.apprafter.dev
LANDING_CMS_PORT=3000
LANDING_CMS_CORS_ORIGINS=https://apprafter.dev

# Auto-rebuild of the static web image on content changes.
# Generate a fine-grained PAT at
#   github.com/settings/personal-access-tokens/new
# scoped to the apprafter repo with permission
#   "Repository permissions > Contents: write"
# (or a classic PAT with `repo` scope).
GITHUB_DISPATCH_TOKEN=<token>
GITHUB_REPO=AppRafter/apprafter

# SMTP (Resend example)
SMTP_HOST=smtp.resend.com
SMTP_PORT=587
SMTP_USER=resend
SMTP_PASS=<resend api key>
SMTP_FROM=hello@apprafter.dev
```

## Caddyfile

```caddy
apprafter.dev {
  root * /var/www/apprafter.dev
  file_server
  encode gzip zstd
  header /fonts/* Cache-Control "public, max-age=31536000, immutable"
  header /_astro/* Cache-Control "public, max-age=31536000, immutable"
  header / Cache-Control "public, max-age=3600"
  try_files {path} {path}/ /404.html
  handle_errors {
    rewrite * /404.html
    file_server
  }
}

cms.apprafter.dev {
  reverse_proxy localhost:3000
  encode gzip zstd
}
```

The `try_files` line keeps trailing-slash routing working
(`/privacy` and `/privacy/` both resolve), and the `handle_errors`
block serves the prebuilt 404 page on misses.

## Content-driven rebuilds — preview / promote / prod

Three tag streams on `landing-web`:

| Tag | Stream | Updated by |
|---|---|---|
| `:preview` + `:preview-<sha>` | every CMS save | `landing-preview-build.yml` — builds with `LANDING_USE_FALLBACK=0` + `LANDING_CMS_URL=https://cms.apprafter.dev` (live fetch) |
| `:prod` + `:latest` | every Publish in admin | `landing-promote-to-prod.yml` — retags `:preview` → `:prod` + `:latest` via `docker buildx imagetools create`, no rebuild |
| `:landing-vX.Y.Z` | every release tag | `release-landing.yml` — independent pinned-release path |

Argo CD on the cluster watches `:prod` for the production app
(`landing-web` Application — `landing/web/apprafter/Application.cue`)
and `:preview` for the preview app (`landing-web-preview` Application
— `landing/web/apprafter/Application-preview.cue`, same package,
both vet in one `cue vet ./landing/web/apprafter/` pass).

Preview should sit behind basic-auth / IP-allowlist on
`preview.apprafter.dev`. v1alpha1 doesn't model HTTP middleware,
so the gating lives at the outer Caddy layer — add to your
Caddyfile:

```caddy
preview.apprafter.dev {
  basicauth /* {
    apprafter <bcrypt hash from `caddy hash-password`>
  }
  reverse_proxy <preview-pod-ip>:80
  encode gzip zstd
}
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

Recommended setup with podman:

**Option A — podman auto-update (preferred).** Run the web
container with `--label io.containers.autoupdate=registry`
and enable the `podman-auto-update.timer` systemd unit:

```sh
sudo systemctl enable --now podman-auto-update.timer
```

`podman auto-update` polls each tagged image, pulls the new
digest, and restarts the container if the digest changed. The
default timer fires daily; for tighter loops drop a `[Timer]
OnCalendar=*:0/5` override.

**Option B — explicit pull cycle.** A short systemd timer:

```ini
# /etc/systemd/system/apprafter-landing-web-pull.timer
[Unit]
Description=Periodic pull for landing-web :prod tag

[Timer]
OnCalendar=*:0/5
Persistent=true

[Install]
WantedBy=timers.target
```

```ini
# /etc/systemd/system/apprafter-landing-web-pull.service
[Service]
Type=oneshot
ExecStart=/usr/bin/podman pull ghcr.io/apprafter/landing-web:prod
ExecStartPost=/bin/systemctl try-restart apprafter-landing-web.service
```

### CMS-side wiring

The Payload hook lives at `landing/cms/src/hooks/notifyRebuild.ts`
and fires on every content global's afterChange. It POSTs to
GitHub's `repository_dispatch` API with type
`landing-content-changed`, which the workflow listens for.

For the hook to fire, set `GITHUB_DISPATCH_TOKEN` and `GITHUB_REPO`
in `/etc/apprafter-cms.env` (see the env block above). Without
them the hook logs a warning and skips — useful in dev.

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

`pg_dump` cron (root crontab on the host):

```cron
15 4 * * * podman exec apprafter-cms-pg pg_dump -U apprafter -d apprafter_cms | gzip > /srv/backups/apprafter-cms/$(date +\%F).sql.gz
```

Retain 14 days; mirror to off-host storage via the project's
standard backup pipeline.

## Verification after deploy

```sh
curl -I https://apprafter.dev/                  # 200, gzip
curl -I https://apprafter.dev/privacy/          # 200
curl -I https://apprafter.dev/sitemap-index.xml # 200
curl -I https://cms.apprafter.dev/admin/        # 200 (login screen)
curl -X POST -H 'Content-Type: application/json' \
  -d '{"email":"deploy-smoke@apprafter.dev"}' \
  https://apprafter.dev/api/waitlist-signups    # 201 (proxied via Caddy)
```

The waitlist POST should land in the admin under `Collections →
Waitlist Signups`. Delete the test entry afterwards.

## Repo-level CI

Lint + typecheck + smoke tests run on every PR via the existing
repo workflows (`/.github/workflows/lint.yml`, `test.yml`). The
Bun workspace-children filter added in commit `dca51b5` skips
`landing/cms` and `landing/web` and runs everything from the
`landing/` root once.

If you want a separate workflow scoped to `landing/**` (path
filtering, separate badge), the template lives at
`landing/ci/landing-ci.example.yml` once that gets added; until
then the unified workflows are sufficient.
