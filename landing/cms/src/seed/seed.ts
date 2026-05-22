// SPDX-FileCopyrightText: 2026 AppRafter contributors
// SPDX-License-Identifier: FSL-1.1-Apache-2.0

/**
 * Seed every Payload global from the corresponding JSON file in
 * landing/web/src/data/fallback/. Idempotent — runs `updateGlobal`
 * which upserts. Safe to re-run after edits to the JSONs (overwrites
 * whatever's in the admin).
 *
 * Run from landing/cms: `bun run seed`.
 */

import fs from 'node:fs/promises';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import { getPayload } from 'payload';

import config from '../payload.config';

const here = path.dirname(fileURLToPath(import.meta.url));
const FALLBACK_DIR = path.resolve(here, '../../../web/src/data/fallback');

/** Map JSON filename (sans extension) to Payload global slug. */
const MAP: Record<string, string> = {
  siteSettings: 'site-settings',
  landingHero: 'landing-hero',
  valueProps: 'value-props',
  scalingJourney: 'scaling-journey',
  tierLadder: 'tier-ladder',
  comparison: 'comparison',
  transparency: 'landing-transparency',
  boringTech: 'boring-tech',
  advantages: 'advantages',
  roadmap: 'roadmap',
  bootstrapStrip: 'bootstrap-strip',
  footer: 'footer-content',
  waitlistCopy: 'waitlist-form-copy',
};

async function main() {
  const payload = await getPayload({ config });

  for (const [file, slug] of Object.entries(MAP)) {
    const filepath = path.join(FALLBACK_DIR, `${file}.json`);
    try {
      const data = JSON.parse(await fs.readFile(filepath, 'utf8'));
      await payload.updateGlobal({
        slug: slug as Parameters<typeof payload.updateGlobal>[0]['slug'],
        data,
      });
      console.log(`✓ seeded ${slug.padEnd(22)} from ${file}.json`);
    } catch (err) {
      console.error(`✗ failed to seed ${slug} from ${file}.json:`, err);
      process.exitCode = 1;
    }
  }

  process.exit(process.exitCode ?? 0);
}

main().catch((err) => {
  console.error(err);
  process.exit(1);
});
