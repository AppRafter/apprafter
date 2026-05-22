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

## One-time setup

```sh
# 1) Build the artefacts on the deploy host (or anywhere with bun
#    available, then rsync the dist/ + .next/ trees over):
cd landing
bun install --frozen-lockfile
bun run build:cms          # produces cms/.next/
PUBLIC_CMS_URL=https://cms.apprafter.dev bun run build:web
                            # produces web/dist/

# 2) Place static output where Caddy serves from:
sudo rsync -a --delete web/dist/ /var/www/apprafter.dev/

# 3) Set up the CMS systemd unit (template below) and start it.
sudo systemctl daemon-reload
sudo systemctl enable --now apprafter-cms

# 4) Seed the content globals (one-shot; idempotent — re-run after
#    JSON edits if you want them mirrored back into Payload):
sudo -u apprafter-cms env $(cat /etc/apprafter-cms.env | xargs) \
  bun --filter @apprafter/landing-cms run seed
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
