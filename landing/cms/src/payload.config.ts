// SPDX-FileCopyrightText: 2026 AppRafter contributors
// SPDX-License-Identifier: FSL-1.1-Apache-2.0

import path from 'node:path';
import { fileURLToPath } from 'node:url';
import { postgresAdapter } from '@payloadcms/db-postgres';
import { lexicalEditor } from '@payloadcms/richtext-lexical';
import { buildConfig } from 'payload';

import { Users } from './collections/Users';
import { WaitlistSignups } from './collections/WaitlistSignups';
import { Advantages } from './globals/Advantages';
import { Booking } from './globals/Booking';
import { BootstrapStrip } from './globals/BootstrapStrip';
import { BoringTech } from './globals/BoringTech';
import { Comparison } from './globals/Comparison';
import { FooterContent } from './globals/FooterContent';
import { LandingHero } from './globals/LandingHero';
import { LandingTransparency } from './globals/LandingTransparency';
import { Publishing } from './globals/Publishing';
import { Roadmap } from './globals/Roadmap';
import { ScalingJourney } from './globals/ScalingJourney';
import { SiteSettings } from './globals/SiteSettings';
import { TierLadder } from './globals/TierLadder';
import { ValueProps } from './globals/ValueProps';
import { WaitlistFormCopy } from './globals/WaitlistFormCopy';
import { withRebuildHook } from './lib/withRebuildHook';
import { migrations } from './migrations';
import { seedGlobals } from './seed/seedGlobals';

const dirname = path.dirname(fileURLToPath(import.meta.url));

// Port + URL derivation. PAYLOAD_PUBLIC_SERVER_URL wins if set
// (e.g. production https://cms.apprafter.dev); otherwise build the
// dev URL from LANDING_CMS_PORT so changing the port in package.json
// scripts flows here automatically.
const cmsPort = process.env.LANDING_CMS_PORT ?? '3000';
const serverURL = process.env.PAYLOAD_PUBLIC_SERVER_URL ?? `http://localhost:${cmsPort}`;

// CORS allowlist. LANDING_CMS_CORS_ORIGINS is a comma-separated
// override; the default tracks LANDING_WEB_PORT and
// LANDING_WEB_PREVIEW_PORT so the dev pair stays consistent without
// hand-editing the array.
const webPort = process.env.LANDING_WEB_PORT ?? '4321';
const webPreviewPort = process.env.LANDING_WEB_PREVIEW_PORT ?? '4322';
const corsOrigins = (
  process.env.LANDING_CMS_CORS_ORIGINS ??
  `http://localhost:${webPort},http://localhost:${webPreviewPort},https://apprafter.dev`
)
  .split(',')
  .map((s) => s.trim())
  .filter(Boolean);

// CSRF allowlist for cookie-auth on mutations. Payload defaults this to
// just [serverURL] and silently drops the auth cookie whenever a
// request's Origin is not on the list. So editing the admin from any
// other origin — e.g. a `kubectl port-forward` to http://localhost:8080
// while serverURL is the public https host — strips req.user and 403s
// every save with "You are not allowed to perform this action" (reads
// still work, so the admin looks logged in). serverURL is always
// trusted; LANDING_CMS_CSRF_ORIGINS (comma-separated) adds extra hosts
// (port-forward, preview) without re-pointing serverURL, which also
// drives the admin + email absolute URLs.
const csrfOrigins = [
  serverURL,
  ...(process.env.LANDING_CMS_CSRF_ORIGINS ?? '')
    .split(',')
    .map((s) => s.trim())
    .filter(Boolean),
];

export default buildConfig({
  serverURL,
  secret: process.env.PAYLOAD_SECRET ?? '',
  db: postgresAdapter({
    pool: { connectionString: process.env.DATABASE_URL ?? '' },
    // The standalone runtime forces NODE_ENV=production, which disables
    // Drizzle `push` (and push pulls in drizzle-kit, which withPayload
    // strips from the standalone trace). So ship versioned migrations:
    // the adapter runs any pending `prodMigrations` on connect at boot,
    // creating/updating the schema with no CLI and no drizzle-kit at
    // runtime. After changing collections/globals, regenerate with
    // `bun run payload migrate:create` and commit src/migrations/.
    prodMigrations: migrations,
  }),
  editor: lexicalEditor(),
  collections: [Users, WaitlistSignups],
  // Every content global is wrapped with the rebuild-dispatch
  // afterChange hook (see lib/withRebuildHook). Booking and
  // Publishing stay bare — Booking's content never flows into the
  // static page, and Publishing IS the promotion controller (its
  // own beforeChange handles the promote-to-prod dispatch, so
  // wrapping with notifyRebuild would cause double-fires).
  globals: [
    withRebuildHook(SiteSettings),
    withRebuildHook(LandingHero),
    withRebuildHook(ValueProps),
    withRebuildHook(ScalingJourney),
    withRebuildHook(TierLadder),
    withRebuildHook(Comparison),
    withRebuildHook(LandingTransparency),
    withRebuildHook(BoringTech),
    withRebuildHook(Advantages),
    withRebuildHook(Roadmap),
    withRebuildHook(BootstrapStrip),
    withRebuildHook(FooterContent),
    withRebuildHook(WaitlistFormCopy),
    Booking,
    Publishing,
  ],
  typescript: {
    outputFile: path.resolve(dirname, '../payload-types.ts'),
  },
  localization: {
    locales: ['en'],
    defaultLocale: 'en',
    fallback: true,
  },
  cors: corsOrigins,
  csrf: csrfOrigins,
  admin: {
    user: Users.slug,
    meta: {
      titleSuffix: '— AppRafter CMS',
    },
  },
  // Bootstrap content: on every boot (after migrations) seed any global
  // never written yet, so a fresh deploy lands the fallback content with
  // no manual step. if-empty → redeploys never overwrite editor edits.
  // The apprafter Application CRD only renders Deployment+Service, so
  // this in-process hook (mirroring prodMigrations) replaces a seed Job.
  // The CLI seeder (bun run seed / node cms/seed.mjs) sets SEED_SKIP_ONINIT
  // to drive the seed itself. Fallback JSONs come from $SEED_FALLBACK_DIR
  // (baked to /app/cms/seed-data in the image) or the workspace source in
  // local dev. Best-effort — a failure never blocks startup.
  onInit: async (payload) => {
    if (process.env.SEED_SKIP_ONINIT === '1') {
      return;
    }
    try {
      const fallbackDir =
        process.env.SEED_FALLBACK_DIR ?? path.resolve(dirname, '../../web/src/data/fallback');
      const res = await seedGlobals(payload, { fallbackDir, ifEmpty: true });
      if (res.seeded > 0) {
        payload.logger.info(`[seed] onInit seeded ${res.seeded} empty global(s)`);
      }
    } catch (err) {
      payload.logger.error({ err }, '[seed] onInit auto-seed failed (non-fatal)');
    }
  },
});
