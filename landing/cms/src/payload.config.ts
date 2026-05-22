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
import { Roadmap } from './globals/Roadmap';
import { ScalingJourney } from './globals/ScalingJourney';
import { SiteSettings } from './globals/SiteSettings';
import { TierLadder } from './globals/TierLadder';
import { ValueProps } from './globals/ValueProps';
import { WaitlistFormCopy } from './globals/WaitlistFormCopy';

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

export default buildConfig({
  serverURL,
  secret: process.env.PAYLOAD_SECRET ?? '',
  db: postgresAdapter({
    pool: { connectionString: process.env.DATABASE_URI ?? '' },
  }),
  editor: lexicalEditor(),
  collections: [Users, WaitlistSignups],
  globals: [
    SiteSettings,
    LandingHero,
    ValueProps,
    ScalingJourney,
    TierLadder,
    Comparison,
    LandingTransparency,
    BoringTech,
    Advantages,
    Roadmap,
    BootstrapStrip,
    FooterContent,
    WaitlistFormCopy,
    Booking,
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
  admin: {
    user: Users.slug,
    meta: {
      titleSuffix: '— AppRafter CMS',
    },
  },
});
