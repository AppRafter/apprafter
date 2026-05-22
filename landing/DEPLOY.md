# Landing deployment

> **Stub** — final deployment recipe lands in Phase J3 of
> `docs/2026-05-22-landing-implementation-plan.md`.

The current target (per Q1/Q2, 2026-05-22):

- **Host:** the same Hetzner VPS that runs the rest of AppRafter's
  community/dev infrastructure.
- **Topology:** Caddy reverse-proxy in front, two upstreams —
  - `apprafter.dev` → static files from `landing/web/dist/`
  - `cms.apprafter.dev` → `next start` from `landing/cms/.next/`, behind
    the proxy on `localhost:3000`.
- **Database:** standalone Postgres 16 (Docker container, not the
  project's shared platform Postgres).
- **Mail:** `nodemailer` over an SMTP relay (Resend / Postmark / similar
  — chosen at deploy time, secret injected via env).

A detailed walk-through (Caddyfile, systemd units, backup cron) goes
here once Phase A–I complete.
