// SPDX-FileCopyrightText: 2026 AppRafter contributors
// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//
// landing/cms smoke — lock down the Payload + Next routing scaffold.
// Phase H expands collections and globals; this file should grow with
// new assertions for each addition (one assertion per slug is the goal).

import { describe, expect, test } from 'bun:test';
import { existsSync, readFileSync } from 'node:fs';
import { join } from 'node:path';

const ROOT = import.meta.dir;

describe('landing/cms scaffold', () => {
  test('payload + next configs present', () => {
    expect(existsSync(join(ROOT, 'src/payload.config.ts'))).toBe(true);
    expect(existsSync(join(ROOT, 'next.config.mjs'))).toBe(true);
  });

  test('Payload (payload) route group exposes admin and API endpoints', () => {
    const routes = [
      'src/app/(payload)/layout.tsx',
      'src/app/(payload)/admin/[[...segments]]/page.tsx',
      'src/app/(payload)/admin/[[...segments]]/not-found.tsx',
      'src/app/(payload)/admin/importMap.js',
      'src/app/(payload)/api/[...slug]/route.ts',
      'src/app/(payload)/api/graphql/route.ts',
      'src/app/(payload)/api/graphql-playground/route.ts',
    ];
    for (const r of routes) {
      expect(existsSync(join(ROOT, r))).toBe(true);
    }
  });

  test('Postgres adapter wired into payload.config', () => {
    const cfg = readFileSync(join(ROOT, 'src/payload.config.ts'), 'utf8');
    expect(cfg).toContain('@payloadcms/db-postgres');
    expect(cfg).toContain('postgresAdapter');
  });

  test('Users collection (default-auth admin) registered', () => {
    const usersPath = join(ROOT, 'src/collections/Users.ts');
    expect(existsSync(usersPath)).toBe(true);
    const users = readFileSync(usersPath, 'utf8');
    expect(users).toContain('auth: true');
  });

  test('WaitlistSignups collection — unique email + public create + admin-only read', () => {
    const wl = readFileSync(join(ROOT, 'src/collections/WaitlistSignups.ts'), 'utf8');
    expect(wl).toContain("slug: 'waitlist-signups'");
    expect(wl).toContain('unique: true');
    expect(wl).toContain('afterChange');
    // public create — anyone may submit; read locked to authed users.
    expect(wl).toMatch(/create:\s*\(\)\s*=>\s*true/);
    expect(wl).toMatch(/read:.*Boolean\(req\.user\)/);
  });

  test('Booking global has Calendly default + {{url}} template placeholder', () => {
    const b = readFileSync(join(ROOT, 'src/globals/Booking.ts'), 'utf8');
    expect(b).toContain("slug: 'booking'");
    expect(b).toContain('calendly.com');
    expect(b).toContain('{{url}}');
  });

  test('sendDiscoveryEmail hook is idempotent + degrades without SMTP', () => {
    const hook = readFileSync(join(ROOT, 'src/hooks/sendDiscoveryEmail.ts'), 'utf8');
    // Idempotency: skip when callEmailSentAt already set.
    expect(hook).toContain('callEmailSentAt');
    // Degradation: warn + return when mailer is null.
    expect(hook).toContain('SMTP not configured');
    // Recursion guard for the .update() inside the hook.
    expect(hook).toMatch(/depth:\s*0/);
  });

  test('mailer returns null when SMTP_HOST is empty (dev default)', () => {
    const m = readFileSync(join(ROOT, 'src/lib/mailer.ts'), 'utf8');
    expect(m).toContain('SMTP_HOST');
    expect(m).toContain('return null');
    expect(m).toContain('MAIL_FROM');
  });

  test('all 13 content globals + Booking are registered + reachable from disk', () => {
    const cfg = readFileSync(join(ROOT, 'src/payload.config.ts'), 'utf8');
    const globals = [
      'SiteSettings',
      'LandingHero',
      'ValueProps',
      'ScalingJourney',
      'TierLadder',
      'Comparison',
      'LandingTransparency',
      'BoringTech',
      'Advantages',
      'Roadmap',
      'BootstrapStrip',
      'FooterContent',
      'WaitlistFormCopy',
      'Booking',
    ];
    for (const g of globals) {
      expect(existsSync(join(ROOT, `src/globals/${g}.ts`))).toBe(true);
      expect(cfg).toContain(`import { ${g} }`);
    }
  });

  test('seed core (seedGlobals) maps every fallback JSON to a global slug', () => {
    const seed = readFileSync(join(ROOT, 'src/seed/seedGlobals.ts'), 'utf8');
    const mappings: [string, string][] = [
      ['siteSettings', 'site-settings'],
      ['landingHero', 'landing-hero'],
      ['valueProps', 'value-props'],
      ['scalingJourney', 'scaling-journey'],
      ['tierLadder', 'tier-ladder'],
      ['comparison', 'comparison'],
      ['transparency', 'landing-transparency'],
      ['boringTech', 'boring-tech'],
      ['advantages', 'advantages'],
      ['roadmap', 'roadmap'],
      ['bootstrapStrip', 'bootstrap-strip'],
      ['footer', 'footer-content'],
      ['waitlistCopy', 'waitlist-form-copy'],
    ];
    for (const [file, slug] of mappings) {
      expect(seed).toContain(`${file}: '${slug}'`);
    }
  });

  test('content seeding: onInit auto-seed (if-empty) + bundled seed.mjs + rebuild suppression', () => {
    // Shared seed core: if-empty mode + suppresses notifyRebuild on bulk load.
    const core = readFileSync(join(ROOT, 'src/seed/seedGlobals.ts'), 'utf8');
    expect(core).toContain('ifEmpty');
    expect(core).toContain('skipRebuild');

    // Server self-seeds empty globals on boot (onInit) — replaces a Job,
    // since the apprafter Application CRD renders only Deployment+Service.
    const cfg = readFileSync(join(ROOT, 'src/payload.config.ts'), 'utf8');
    expect(cfg).toContain('onInit');
    expect(cfg).toContain('seedGlobals');
    // CLI seeder disables that onInit so it doesn't double-run.
    expect(cfg).toContain('SEED_SKIP_ONINIT');
    const cli = readFileSync(join(ROOT, 'src/seed/seed.ts'), 'utf8');
    expect(cli).toContain('SEED_SKIP_ONINIT');

    // notifyRebuild honours the skip flag (no preview rebuilds on seed).
    const nr = readFileSync(join(ROOT, 'src/hooks/notifyRebuild.ts'), 'utf8');
    expect(nr).toContain('context?.skipRebuild');

    // Dockerfile bundles the standalone seed artifact + its fallback data.
    const df = readFileSync(join(ROOT, 'Dockerfile'), 'utf8');
    expect(df).toContain('seed.mjs');
    expect(df).toContain('/app/cms/seed-data');
    expect(df).toContain('SEED_FALLBACK_DIR');
  });

  test('Next standalone output enabled for Docker build', () => {
    const nextCfg = readFileSync(join(ROOT, 'next.config.mjs'), 'utf8');
    expect(nextCfg).toContain("output: 'standalone'");
    expect(nextCfg).toContain('outputFileTracingRoot');
  });

  test('Dockerfile present + two-stage (bun builder → node runtime)', () => {
    const dfPath = join(ROOT, 'Dockerfile');
    expect(existsSync(dfPath)).toBe(true);
    const df = readFileSync(dfPath, 'utf8');
    expect(df).toMatch(/FROM oven\/bun.*AS builder/);
    expect(df).toMatch(/FROM node.*AS runtime/);
    // standalone copy: the WHOLE tree is copied verbatim (NOT
    // flattened into /app), so Bun's `../../node_modules/.bun`
    // symlinks under cms/node_modules keep resolving; the server
    // launches from the cms/ subdir.
    expect(df).toContain('/landing/cms/.next/standalone /app');
    expect(df).toContain('cms/server.js');
    expect(df).toContain('.next/static');
    // public/ must exist in the source tree for the runtime COPY
    // to succeed — Next standalone explicitly excludes it.
    expect(df).toContain('/landing/cms/public');
  });

  test('public/ exists with robots.txt blocking crawlers', () => {
    const robots = join(ROOT, 'public/robots.txt');
    expect(existsSync(robots)).toBe(true);
    const body = readFileSync(robots, 'utf8');
    expect(body).toContain('User-agent: *');
    expect(body).toContain('Disallow: /');
  });

  test('githubDispatch helper centralises the dispatch call', () => {
    const helper = readFileSync(join(ROOT, 'src/lib/githubDispatch.ts'), 'utf8');
    expect(helper).toContain('GITHUB_DISPATCH_TOKEN');
    expect(helper).toContain('GITHUB_REPO');
    expect(helper).toContain('/dispatches');
    expect(helper).toMatch(/export async function fireGithubDispatch\(/);
  });

  test('notifyRebuild + withRebuildHook fire content-changed dispatch + stamp Publishing', () => {
    const hook = readFileSync(join(ROOT, 'src/hooks/notifyRebuild.ts'), 'utf8');
    expect(hook).toContain("'landing-content-changed'");
    expect(hook).toContain('fireGithubDispatch');
    // Also stamps Publishing.lastEditAt + lastEditedGlobal.
    expect(hook).toContain("slug: 'publishing'");
    expect(hook).toContain('lastEditAt');
    expect(hook).toContain('lastEditedGlobal');

    const helper = readFileSync(join(ROOT, 'src/lib/withRebuildHook.ts'), 'utf8');
    expect(helper).toContain('notifyRebuild');
    expect(helper).toContain('afterChange');

    const cfg = readFileSync(join(ROOT, 'src/payload.config.ts'), 'utf8');
    expect(cfg).toContain('withRebuildHook(SiteSettings)');
    expect(cfg).toContain('withRebuildHook(LandingHero)');
    expect(cfg).toContain('withRebuildHook(WaitlistFormCopy)');
    // Booking + Publishing are the explicit exceptions: Booking
    // never flows into the static page; Publishing IS the
    // promotion controller and has its own beforeChange hook.
    expect(cfg).not.toContain('withRebuildHook(Booking)');
    expect(cfg).not.toContain('withRebuildHook(Publishing)');
  });

  test('Publishing global tracks last edit / last promote + edit log + trigger checkbox', () => {
    const pub = readFileSync(join(ROOT, 'src/globals/Publishing.ts'), 'utf8');
    expect(pub).toContain("slug: 'publishing'");
    expect(pub).toContain('lastEditAt');
    expect(pub).toContain('lastEditedGlobal');
    expect(pub).toContain('lastPromotedAt');
    expect(pub).toContain('promoteToProd');
    expect(pub).toContain('beforeChange');
    // editLog is the rolling "edits since last promote" buffer,
    // capped at 20 entries so the admin doesn't drown in history.
    expect(pub).toContain('editLog');
    expect(pub).toContain('maxRows: 20');
    // Admin-only read access — these timestamps reveal edit cadence.
    expect(pub).toMatch(/read:.*Boolean\(req\.user\)/);
  });

  test('promoteToProd hook fires landing-promote-to-prod + resets checkbox + clears log', () => {
    const hook = readFileSync(join(ROOT, 'src/hooks/promoteToProd.ts'), 'utf8');
    expect(hook).toContain("'landing-promote-to-prod'");
    expect(hook).toContain('fireGithubDispatch');
    // One-shot: clear the trigger so re-saving doesn't re-fire.
    expect(hook).toContain('promoteToProd: false');
    expect(hook).toContain('lastPromotedAt');
    // The edit log is the "what changed since last promote" buffer
    // — wipe it so the admin gets a clean slate after a promote.
    expect(hook).toContain('editLog: []');
  });

  test('notifyRebuild prepends to editLog + caps at 20 entries', () => {
    const hook = readFileSync(join(ROOT, 'src/hooks/notifyRebuild.ts'), 'utf8');
    expect(hook).toContain('editLog');
    // Implementation contract: read current → prepend → slice(0, 20).
    expect(hook).toContain('EDIT_LOG_CAP');
    expect(hook).toMatch(/slice\(0,\s*EDIT_LOG_CAP\)/);
    // Entry shape — at, global, editor email.
    expect(hook).toContain('req.user?.email');
  });

  test('Next root / redirects to /admin (no public frontend on the cms host)', () => {
    const nextCfg = readFileSync(join(ROOT, 'next.config.mjs'), 'utf8');
    expect(nextCfg).toContain('/admin');
    expect(nextCfg).toContain('redirects');
  });
});
