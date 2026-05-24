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

The recommended path is to pull prebuilt container images from
GHCR — `.github/workflows/release-landing.yml` builds and pushes
them on every `landing-v*` tag:

```sh
# Cut a release tag on master:
git tag landing-v0.1.0
git push origin landing-v0.1.0
# GitHub Actions runs both image builds in parallel (~3-5 min
# each on cache hits). After it lands:
#   ghcr.io/apprafter/landing-web:landing-v0.1.0   (+ :latest)
#   ghcr.io/apprafter/landing-cms:landing-v0.1.0   (+ :latest)
```

First push of each package on GHCR makes it private. Flip to
public once via the GitHub UI: Packages → <package> → Settings →
Change visibility → Public. After that the host can `podman pull`
without auth.

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

## Rebuild trigger

The site is fully static. Content changes in Payload do not
auto-rebuild the static output — the dev-server-style live reload
only happens locally. Two pragmatic options:

1. **Manual rebuild after big edits.** `ssh` to the host, run
   `bun run build:web && rsync ... /var/www/apprafter.dev/`.
2. **Webhook-driven rebuild.** Add a Payload `afterChange` hook on
   each content global that POSTs to a small webhook endpoint on
   the host (e.g. via Caddy + a tiny systemd-managed bash script)
   which runs the rebuild script. Defer until edit cadence justifies
   the moving piece.

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
