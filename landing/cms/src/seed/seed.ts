// SPDX-FileCopyrightText: 2026 AppRafter contributors
// SPDX-License-Identifier: FSL-1.1-Apache-2.0

/**
 * CLI content seeder. Overwrites (or, with --if-empty / SEED_IF_EMPTY=1,
 * only fills empty) Payload globals from the fallback JSONs. Two ways
 * to run it:
 *
 *   - local dev:   `bun run seed`
 *   - in the image: `node cms/seed.mjs` (bundled by the Dockerfile)
 *
 * The server seeds itself on boot via payload.config `onInit`; this CLI
 * exists for an explicit overwrite that mirrors edited JSONs back into
 * the admin. SEED_SKIP_ONINIT disables that onInit pass while this CLI
 * is the one driving the seed (set before getPayload so init reads it).
 *
 * Fallback JSONs come from $SEED_FALLBACK_DIR (the image bakes it to the
 * seed-data/ dir beside the bundle) or ../../../web/src/data/fallback.
 */

import path from 'node:path';
import { fileURLToPath } from 'node:url';
import { getPayload } from 'payload';

import config from '../payload.config';
import { seedGlobals } from './seedGlobals';

// Disable the payload.config onInit auto-seed for this CLI process —
// the CLI drives the seed itself below. Set before getPayload() (which
// runs onInit) reads it. Imports above only define the config; onInit
// fires later, during getPayload().
process.env.SEED_SKIP_ONINIT = '1';

const here = path.dirname(fileURLToPath(import.meta.url));
const fallbackDir =
  process.env.SEED_FALLBACK_DIR ?? path.resolve(here, '../../../web/src/data/fallback');
const ifEmpty = process.env.SEED_IF_EMPTY === '1' || process.argv.includes('--if-empty');

async function main() {
  const payload = await getPayload({ config });
  console.log(`seeding from ${fallbackDir}${ifEmpty ? ' (if-empty mode)' : ''}`);
  const res = await seedGlobals(payload, { fallbackDir, ifEmpty, log: (m) => console.log(m) });
  console.log(`done — ${res.seeded} seeded, ${res.skipped} skipped, ${res.failed} failed`);
  process.exit(res.failed > 0 ? 1 : 0);
}

main().catch((err) => {
  console.error(err);
  process.exit(1);
});
