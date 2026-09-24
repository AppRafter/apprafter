// SPDX-FileCopyrightText: 2026 AppRafter contributors
// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//
// Build-time cache-busting for static assets in `public/`. Astro copies
// public/ verbatim with stable names, so a changed favicon keeps its URL
// and stays pinned in Cloudflare's edge cache (default Browser Cache TTL
// 4h) + the browser's favicon cache until the TTL lapses. Appending a
// short content hash as `?v=<hash>` to the <link>/<meta> href gives each
// new build a fresh cache key the moment the bytes change — and nothing
// to bust when they don't. Server-only: this reads the filesystem and is
// called from `.astro` frontmatter (SSR'd at build), never client code.

import { createHash } from 'node:crypto';
import { existsSync, readFileSync } from 'node:fs';
import { resolve, sep } from 'node:path';
import { fileURLToPath } from 'node:url';

/** First 8 hex chars of the content's sha256 — enough to key a cache. */
export function shortHash(content: string | Uint8Array): string {
  return createHash('sha256').update(content).digest('hex').slice(0, 8);
}

/**
 * Locate `public/`. Two contexts have to agree, and `import.meta.url` alone
 * does not cover both: under `bun test` this module runs from source, so the
 * source-relative path is right; during `astro build` it is bundled into a
 * prerender chunk, whose location moved to `dist/.prerender/chunks/` in Astro
 * 7 — there the source-relative path points at `dist/public` and the build
 * dies with "cannot read public asset". `astro build`, `astro dev` and the
 * Docker build all run with the Astro project root as cwd, so try cwd first
 * and keep the source-relative path for the test/tsx case. Failing loudly
 * here beats `assetExists` quietly returning false and silently dropping the
 * versioned <link> from every page.
 */
function locatePublicDir(): string {
  const candidates = [
    resolve(process.cwd(), 'public'),
    resolve(fileURLToPath(new URL('../../public/', import.meta.url))),
  ];
  const found = candidates.find((candidate) => existsSync(candidate));
  if (!found) {
    throw new Error(`asset-version: cannot locate public/ (tried ${candidates.join(', ')})`);
  }
  return found;
}

const PUBLIC_DIR = locatePublicDir();

/**
 * Resolve a `public/`-relative path, returning the absolute path only when it
 * stays inside public/ — a `../` traversal (or any path escaping the root)
 * yields null. Keeps a stray/dynamic ref from reading files outside public/.
 */
function containedPath(relPath: string): string | null {
  const full = resolve(PUBLIC_DIR, relPath);
  return full === PUBLIC_DIR || full.startsWith(PUBLIC_DIR + sep) ? full : null;
}

/** Content hash of a `public/`-relative asset, for `?v=` cache-busting. */
export function assetVersion(relPath: string): string {
  const full = containedPath(relPath);
  try {
    if (!full) throw new Error('path escapes public/');
    return shortHash(readFileSync(full));
  } catch (cause) {
    throw new Error(`asset-version: cannot read public asset "${relPath}"`, { cause });
  }
}

/** Whether a `public/`-relative asset exists — for optionally-versioned refs. */
export function assetExists(relPath: string): boolean {
  const full = containedPath(relPath);
  return full !== null && existsSync(full);
}
