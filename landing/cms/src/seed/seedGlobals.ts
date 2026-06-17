// SPDX-FileCopyrightText: 2026 AppRafter contributors
// SPDX-License-Identifier: FSL-1.1-Apache-2.0

/**
 * Shared content-seed core: upsert every Payload content global from
 * the matching JSON in landing/web/src/data/fallback/ (the same data
 * the web image bakes in). Two callers share this:
 *
 *   - the server `onInit` (payload.config.ts) — runs on every boot in
 *     if-empty mode, so a fresh deploy lands the fallback content with
 *     no manual step and redeploys never clobber editor edits;
 *   - the CLI (seed.ts → `bun run seed` / `node cms/seed.mjs`) — for
 *     an explicit overwrite that mirrors edited JSONs back in.
 *
 * Writes carry `context.skipRebuild` so notifyRebuild stays quiet
 * during bulk load (no preview rebuilds, no Publishing edit-log noise).
 */

import fs from 'node:fs/promises';
import path from 'node:path';
import type { Payload } from 'payload';

/** Map fallback JSON filename (sans extension) → Payload global slug. */
export const GLOBAL_MAP: Record<string, string> = {
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
  legalTerms: 'legal-terms',
  legalPrivacy: 'legal-privacy',
  notFound: 'not-found',
};

export interface SeedOptions {
  /** Directory holding the `<file>.json` fallback files. */
  fallbackDir: string;
  /** Only write globals never persisted yet — safe to re-run. */
  ifEmpty: boolean;
  /** Progress sink; defaults to the payload logger at info level. */
  log?: (msg: string) => void;
}

export interface SeedResult {
  seeded: number;
  skipped: number;
  failed: number;
}

export async function seedGlobals(payload: Payload, opts: SeedOptions): Promise<SeedResult> {
  const log = opts.log ?? ((m: string) => payload.logger.info(m));
  const res: SeedResult = { seeded: 0, skipped: 0, failed: 0 };

  for (const [file, slug] of Object.entries(GLOBAL_MAP)) {
    const s = slug as Parameters<typeof payload.updateGlobal>[0]['slug'];
    try {
      if (opts.ifEmpty) {
        const existing = await payload.findGlobal({ slug: s, depth: 0, overrideAccess: true });
        if (existing && (existing as { updatedAt?: unknown }).updatedAt) {
          log(`• skip   ${slug.padEnd(22)} (already has content)`);
          res.skipped++;
          continue;
        }
      }
      const raw = await fs.readFile(path.join(opts.fallbackDir, `${file}.json`), 'utf8');
      await payload.updateGlobal({
        slug: s,
        data: JSON.parse(raw) as Parameters<typeof payload.updateGlobal>[0]['data'],
        context: { skipRebuild: true },
        overrideAccess: true,
      });
      log(`✓ seeded ${slug.padEnd(22)} from ${file}.json`);
      res.seeded++;
    } catch (err) {
      payload.logger.error({ err }, `✗ failed to seed ${slug} from ${file}.json`);
      res.failed++;
    }
  }
  return res;
}
