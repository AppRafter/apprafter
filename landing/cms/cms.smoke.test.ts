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

  test('seed script maps every fallback JSON to a global slug', () => {
    const seed = readFileSync(join(ROOT, 'src/seed/seed.ts'), 'utf8');
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
    // standalone copy paths.
    expect(df).toContain('.next/standalone/cms');
    expect(df).toContain('.next/static');
  });

  test('Next root / redirects to /admin (no public frontend on the cms host)', () => {
    const nextCfg = readFileSync(join(ROOT, 'next.config.mjs'), 'utf8');
    expect(nextCfg).toContain('/admin');
    expect(nextCfg).toContain('redirects');
  });
});
